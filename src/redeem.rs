// Redemption queue + trade history tracker.
//
// Two JSON files:
//   data/pending_redemptions.json — queue of positions awaiting redemption
//   data/trade_history.json       — completed trades with WIN/LOSS outcome
//
// Flow:
//   1. After each copy trade, `record_pending()` adds an entry to the queue.
//   2. A background loop checks every 60s. Entries older than 15 minutes are
//      eligible for redemption.
//   3. On redemption attempt:
//      - Success → WIN (position redeemed for USDC). Recorded in trade_history.
//      - Revert/insufficient → LOSS (position worthless). Recorded in trade_history.
//      - Other error → move to end of queue, retry next cycle.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::chain;
use crate::consts::{
    PENDING_REDEMPTIONS_FILE, REDEMPTION_DELAY_SECS, REDEMPTION_POLL_INTERVAL_SECS,
    TRADE_HISTORY_FILE,
};

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRedemption {
    /// Polymarket conditionId (hex string for on-chain redemption).
    pub condition_id: String,
    /// Market name (e.g. "BTCUSD", "Will X happen?").
    pub market: String,
    /// Trade side: "BUY" or "SELL".
    pub side: String,
    /// Actual shares filled.
    pub filled_shares: f64,
    /// Actual price per share.
    pub fill_price: f64,
    /// Token ID of the position.
    pub token_id: String,
    /// Unix timestamp (secs) when the trade was placed.
    pub trade_ts: i64,
    /// Number of redemption attempts so far.
    #[serde(default)]
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeHistoryEntry {
    /// Market name (e.g. "BTCUSD", "ETHUSD", etc).
    pub market: String,
    /// "WIN" or "LOSS".
    pub outcome: String,
    /// Actual shares bought/sold (from FAK fill).
    pub filled_shares: f64,
    /// Price paid per share.
    pub fill_price: f64,
    /// Trade side: "BUY" or "SELL".
    pub side: String,
    /// Unix timestamp (secs) when the trade was placed.
    pub trade_ts: i64,
    /// Unix timestamp (secs) when the outcome was determined.
    pub resolved_ts: i64,
}

// ── Ledger I/O ──────────────────────────────────────────────────────────────

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn load_queue() -> VecDeque<PendingRedemption> {
    let path = Path::new(PENDING_REDEMPTIONS_FILE);
    if !path.exists() {
        return VecDeque::new();
    }
    match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
            eprintln!("WARN: corrupt redemption queue, resetting: {e}");
            VecDeque::new()
        }),
        Err(e) => {
            eprintln!("WARN: cannot read redemption queue: {e}");
            VecDeque::new()
        }
    }
}

fn save_queue(entries: &VecDeque<PendingRedemption>) -> Result<()> {
    let json = serde_json::to_string_pretty(entries)?;
    fs::write(PENDING_REDEMPTIONS_FILE, json).context("write redemption queue")?;
    Ok(())
}

fn load_history() -> Vec<TradeHistoryEntry> {
    let path = Path::new(TRADE_HISTORY_FILE);
    if !path.exists() {
        return Vec::new();
    }
    match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
            eprintln!("WARN: corrupt trade history, resetting: {e}");
            Vec::new()
        }),
        Err(e) => {
            eprintln!("WARN: cannot read trade history: {e}");
            Vec::new()
        }
    }
}

fn save_history(entries: &[TradeHistoryEntry]) -> Result<()> {
    let json = serde_json::to_string_pretty(entries)?;
    fs::write(TRADE_HISTORY_FILE, json).context("write trade history")?;
    Ok(())
}

fn append_history(entry: TradeHistoryEntry) {
    let mut history = load_history();
    history.push(entry);
    if let Err(e) = save_history(&history) {
        eprintln!("WARN: failed to save trade history: {e:#}");
    }
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Record a new trade for future redemption (called after each successful copy).
pub fn record_pending(
    condition_id: &str,
    market: &str,
    side: &str,
    filled_shares: f64,
    fill_price: f64,
    token_id: &str,
) {
    let mut queue = load_queue();

    // Don't duplicate same condition_id + side + token
    if queue.iter().any(|e| e.condition_id == condition_id && e.token_id == token_id) {
        return;
    }

    queue.push_back(PendingRedemption {
        condition_id: condition_id.to_string(),
        market: market.to_string(),
        side: side.to_string(),
        filled_shares,
        fill_price,
        token_id: token_id.to_string(),
        trade_ts: now_secs(),
        attempts: 0,
    });

    if let Err(e) = save_queue(&queue) {
        eprintln!("WARN: failed to save redemption queue: {e:#}");
    } else {
        eprintln!("Redemption queued: {} ({}) — {:.2} shares @ {:.4}", market, condition_id, filled_shares, fill_price);
    }
}

/// Background loop: check the queue every 60s, process entries older than 15 min.
/// Failed redemptions move to the back of the queue. Runs forever.
pub async fn run_redemption_loop(private_key: String) {
    let _client = Client::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(REDEMPTION_POLL_INTERVAL_SECS));

    loop {
        ticker.tick().await;

        let mut queue = load_queue();
        if queue.is_empty() {
            continue;
        }

        let now = now_secs();
        let queue_len = queue.len();
        let mut processed = 0;
        let mut retry_later: Vec<PendingRedemption> = Vec::new();

        // Process entries from the front of the queue
        while let Some(mut entry) = queue.pop_front() {
            let age_secs = (now - entry.trade_ts).max(0) as u64;

            // Not yet ripe — put it back and stop (queue is ordered by trade_ts)
            if age_secs < REDEMPTION_DELAY_SECS {
                queue.push_front(entry);
                break;
            }

            entry.attempts += 1;
            processed += 1;

            eprintln!(
                "Redeeming {} (attempt #{}, age {}m)…",
                entry.market,
                entry.attempts,
                age_secs / 60
            );

            match chain::redeem_single(&private_key, &entry.condition_id).await {
                Ok(_) => {
                    // WIN — position redeemed successfully
                    eprintln!("WIN: {} — redeemed successfully", entry.market);
                    append_history(TradeHistoryEntry {
                        market: entry.market.clone(),
                        outcome: "WIN".to_string(),
                        filled_shares: entry.filled_shares,
                        fill_price: entry.fill_price,
                        side: entry.side.clone(),
                        trade_ts: entry.trade_ts,
                        resolved_ts: now,
                    });
                }
                Err(e) => {
                    let msg = format!("{e:#}");

                    if msg.contains("revert") || msg.contains("insufficient") {
                        // LOSS — position is worthless (revert = nothing to redeem)
                        eprintln!("LOSS: {} — {}", entry.market, msg);
                        append_history(TradeHistoryEntry {
                            market: entry.market.clone(),
                            outcome: "LOSS".to_string(),
                            filled_shares: entry.filled_shares,
                            fill_price: entry.fill_price,
                            side: entry.side.clone(),
                            trade_ts: entry.trade_ts,
                            resolved_ts: now,
                        });
                    } else {
                        // Transient error — move to back of queue for retry
                        eprintln!(
                            "Redeem FAILED for {} (attempt #{}, retrying later): {:#}",
                            entry.market, entry.attempts, e
                        );
                        retry_later.push(entry);
                    }
                }
            }
        }

        // Append retries to the back of the queue
        for entry in retry_later {
            queue.push_back(entry);
        }

        if processed > 0 {
            if let Err(e) = save_queue(&queue) {
                eprintln!("WARN: failed to update redemption queue: {e:#}");
            }
            eprintln!(
                "Redemption cycle: processed {processed}/{queue_len}, {} remaining in queue",
                queue.len()
            );
        }
    }
}

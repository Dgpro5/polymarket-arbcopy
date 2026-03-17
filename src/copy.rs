// Trade tracking — poll target wallet, detect arb opportunities, log to Discord.

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::consts::{ARB_FILE, DATA_API, TARGET_WALLET};

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TargetTrade {
    #[serde(rename = "conditionId", default)]
    pub condition_id: String,
    #[serde(rename = "transactionHash", default)]
    pub transaction_hash: String,
    #[serde(rename = "proxyWallet", default)]
    pub proxy_wallet: String,
    #[serde(rename = "asset", default)]
    pub token_id: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    #[serde(default)]
    pub timestamp: f64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub slug: String,
    #[serde(rename = "outcome", default)]
    pub outcome: String,
    #[serde(rename = "usdcSize", default)]
    pub usdc_size: f64,
    #[serde(rename = "outcomeIndex", default)]
    pub outcome_index: u32,
}

/// A single unmatched trade leg waiting for its opposite outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmatchedLeg {
    pub condition_id: String,
    pub title: String,
    pub outcome: String,
    pub outcome_index: u32,
    pub price: f64,
    pub size: f64,
    pub tx_hash: String,
    pub timestamp: f64,
}

/// A matched arb pair: YES leg + NO leg on the same market, sum < $1.00.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbMatch {
    pub condition_id: String,
    pub title: String,
    pub yes_price: f64,
    pub no_price: f64,
    /// Spread = 1.00 - (yes_price + no_price). Positive = profit.
    pub spread: f64,
    /// Matched share count (min of the two legs).
    pub matched_shares: f64,
    /// Profit in USD = spread * matched_shares.
    pub profit_usd: f64,
    pub yes_tx: String,
    pub no_tx: String,
    pub matched_at: i64,
}

/// Persistent state saved to arb_opportunities.json.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArbData {
    pub matches: Vec<ArbMatch>,
    /// Unmatched legs waiting for the opposite outcome, keyed by condition_id.
    pub unmatched: HashMap<String, Vec<UnmatchedLeg>>,
}

pub struct TrackingState {
    pub last_poll_ts: i64,
    pub seen_trade_ids: HashSet<String>,
    pub arb_data: ArbData,
    /// Trades detected since last report (for the 5-min summary).
    pub trades_since_report: u32,
    /// Arb matches found since last report.
    pub new_matches_since_report: Vec<ArbMatch>,
}

// ── Public API ───────────────────────────────────────────────────────────────

pub fn new_tracking_state() -> TrackingState {
    let arb_data = load_arb_data();
    TrackingState {
        last_poll_ts: now_secs(),
        seen_trade_ids: HashSet::new(),
        arb_data,
        trades_since_report: 0,
        new_matches_since_report: Vec::new(),
    }
}

/// Main tick: poll for new trades, try to match arbs. No Discord sends here.
pub async fn poll_and_track(client: &Client, state: &mut TrackingState) -> Result<()> {
    let trades = fetch_target_trades(client, state.last_poll_ts).await?;

    if trades.is_empty() {
        return Ok(());
    }

    let mut newest_ts = state.last_poll_ts;

    for trade in &trades {
        let trade_ts = trade.timestamp as i64;
        if trade_ts > newest_ts {
            newest_ts = trade_ts;
        }

        if state.seen_trade_ids.contains(&trade.transaction_hash) {
            continue;
        }
        state.seen_trade_ids.insert(trade.transaction_hash.clone());
        state.trades_since_report += 1;

        // Only BUY trades form arb legs (buying both sides of a market).
        if trade.side != "BUY" {
            eprintln!(
                "Trade (SELL, skipping arb): {} {:.2}@{:.4} | {}",
                trade.title, trade.size, trade.price, trade.outcome
            );
            continue;
        }

        eprintln!(
            "Trade: BUY {} {:.2}@{:.4} | {}",
            trade.title, trade.size, trade.price, trade.outcome
        );

        let leg = UnmatchedLeg {
            condition_id: trade.condition_id.clone(),
            title: trade.title.clone(),
            outcome: trade.outcome.clone(),
            outcome_index: trade.outcome_index,
            price: trade.price,
            size: trade.size,
            tx_hash: trade.transaction_hash.clone(),
            timestamp: trade.timestamp,
        };

        try_match_arb(state, leg);
    }

    state.last_poll_ts = newest_ts;
    save_arb_data(&state.arb_data);
    Ok(())
}

/// Take the report data and reset counters. Called every 5 minutes.
pub fn take_report(state: &mut TrackingState) -> ReportData {
    let mut new_matches = std::mem::take(&mut state.new_matches_since_report);
    // Sort by spread descending (biggest arb first).
    new_matches.sort_by(|a, b| b.spread.partial_cmp(&a.spread).unwrap_or(std::cmp::Ordering::Equal));

    let all_time_count = state.arb_data.matches.len();
    let unmatched_count: usize = state.arb_data.unmatched.values().map(|v| v.len()).sum();
    let trades = state.trades_since_report;

    state.trades_since_report = 0;

    ReportData {
        trades_detected: trades,
        new_matches,
        all_time_arb_count: all_time_count,
        pending_legs: unmatched_count,
    }
}

pub struct ReportData {
    pub trades_detected: u32,
    pub new_matches: Vec<ArbMatch>,
    pub all_time_arb_count: usize,
    pub pending_legs: usize,
}

// ── Arb matching ─────────────────────────────────────────────────────────────

fn try_match_arb(state: &mut TrackingState, new_leg: UnmatchedLeg) {
    let cid = new_leg.condition_id.clone();
    let legs = state.arb_data.unmatched.entry(cid).or_default();

    // Find the best opposite-outcome leg to match with (maximize spread).
    let opposite_index = if new_leg.outcome_index == 0 { 1 } else { 0 };

    let mut best_idx: Option<usize> = None;
    let mut best_spread: f64 = f64::MIN;

    for (i, leg) in legs.iter().enumerate() {
        if leg.outcome_index != opposite_index {
            continue;
        }
        let sum = new_leg.price + leg.price;
        let spread = 1.0 - sum;
        if spread > 0.0 && spread > best_spread {
            best_spread = spread;
            best_idx = Some(i);
        }
    }

    if let Some(idx) = best_idx {
        let matched_leg = legs.remove(idx);

        let (yes_price, yes_tx, no_price, no_tx) = if new_leg.outcome_index == 0 {
            (new_leg.price, new_leg.tx_hash.clone(), matched_leg.price, matched_leg.tx_hash.clone())
        } else {
            (matched_leg.price, matched_leg.tx_hash.clone(), new_leg.price, new_leg.tx_hash.clone())
        };

        let matched_shares = new_leg.size.min(matched_leg.size);
        let spread = 1.0 - (yes_price + no_price);
        let profit_usd = spread * matched_shares;

        let arb = ArbMatch {
            condition_id: new_leg.condition_id.clone(),
            title: new_leg.title.clone(),
            yes_price,
            no_price,
            spread,
            matched_shares,
            profit_usd,
            yes_tx,
            no_tx,
            matched_at: now_secs(),
        };

        eprintln!(
            "  ARB MATCHED: {} | Yes {:.2}c + No {:.2}c = {:.2}c | spread {:.2}c | ${:.4} profit",
            arb.title,
            yes_price * 100.0,
            no_price * 100.0,
            (yes_price + no_price) * 100.0,
            spread * 100.0,
            profit_usd
        );

        // If there are leftover shares from the larger leg, keep them unmatched.
        if new_leg.size > matched_leg.size {
            let leftover = UnmatchedLeg {
                size: new_leg.size - matched_leg.size,
                ..new_leg
            };
            legs.push(leftover);
        } else if matched_leg.size > new_leg.size {
            let leftover = UnmatchedLeg {
                size: matched_leg.size - new_leg.size,
                ..matched_leg
            };
            legs.push(leftover);
        }

        state.new_matches_since_report.push(arb.clone());
        state.arb_data.matches.push(arb);
    } else {
        // No match found — store as unmatched.
        legs.push(new_leg);
    }

    // Clean up empty entries.
    state.arb_data.unmatched.retain(|_, v| !v.is_empty());
}

// ── Trade discovery ──────────────────────────────────────────────────────────

async fn fetch_target_trades(client: &Client, since_ts: i64) -> Result<Vec<TargetTrade>> {
    let url = format!(
        "{DATA_API}/activity?user={TARGET_WALLET}&type=TRADE&limit=100&sortBy=TIMESTAMP&sortDirection=DESC&start={since_ts}"
    );

    let resp = client.get(&url).send().await.context("fetch target trades")?;
    let status = resp.status();
    let body = resp.text().await?;

    if !status.is_success() {
        return Err(anyhow!("Data API error ({}): {}", status, body));
    }

    let trades: Vec<TargetTrade> = serde_json::from_str(&body).context("parse target trades")?;
    Ok(trades)
}

// ── JSON persistence ─────────────────────────────────────────────────────────

fn load_arb_data() -> ArbData {
    let path = Path::new(ARB_FILE);
    if !path.exists() {
        return ArbData::default();
    }
    match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
            eprintln!("WARN: corrupt arb file, resetting: {e}");
            ArbData::default()
        }),
        Err(e) => {
            eprintln!("WARN: cannot read arb file: {e}");
            ArbData::default()
        }
    }
}

fn save_arb_data(data: &ArbData) {
    if let Err(e) = (|| -> Result<()> {
        let json = serde_json::to_string_pretty(data)?;
        fs::write(ARB_FILE, json).context("write arb file")?;
        Ok(())
    })() {
        eprintln!("WARN: failed to save arb data: {e:#}");
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

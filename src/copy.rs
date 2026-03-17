// Trade tracking — poll target wallet trades and log them to Discord.

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashSet;

use crate::alerts;
use crate::consts::{DATA_API, TARGET_WALLET};

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

pub struct TrackingState {
    /// Unix timestamp (seconds) — only fetch trades newer than this.
    pub last_poll_ts: i64,
    pub seen_trade_ids: HashSet<String>,
}

// ── Public API ───────────────────────────────────────────────────────────────

pub fn new_tracking_state() -> TrackingState {
    TrackingState {
        last_poll_ts: now_secs(),
        seen_trade_ids: HashSet::new(),
    }
}

/// Main tick: poll for new trades, log new ones to Discord.
pub async fn poll_and_log(client: &Client, state: &mut TrackingState) -> Result<()> {
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

        // Dedup
        if state.seen_trade_ids.contains(&trade.transaction_hash) {
            continue;
        }

        state.seen_trade_ids.insert(trade.transaction_hash.clone());

        let notional = trade.size * trade.price;

        eprintln!(
            "Trade detected: {} {} {:.2} shares @ {:.4} (${:.2}) | {}",
            trade.side, trade.title, trade.size, trade.price, notional, trade.outcome
        );

        alerts::send_trade_detected(
            client,
            &trade.side,
            &trade.title,
            &trade.outcome,
            trade.price,
            trade.size,
            notional,
            &trade.transaction_hash,
        )
        .await;
    }

    state.last_poll_ts = newest_ts;
    Ok(())
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

// ── Helpers ──────────────────────────────────────────────────────────────────

fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

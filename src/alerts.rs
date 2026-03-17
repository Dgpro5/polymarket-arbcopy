// Discord webhook alerts for the trade tracker.

use reqwest::Client;
use serde_json::json;

use crate::consts::{DISCORD_WEBHOOK_URL, ERROR_DISCORD_WEBHOOK_URL, POLL_INTERVAL_MS};

// ── Public API ──────────────────────────────────────────────────────────────

pub async fn send_startup(client: &Client) {
    let msg = format!(
        "**Trade Tracker Started**\nTarget: `dustedfloor`\nPolling every **{}ms**",
        POLL_INTERVAL_MS
    );
    send_main(client, &msg).await;
}

pub async fn send_trade_detected(
    client: &Client,
    side: &str,
    market: &str,
    outcome: &str,
    price: f64,
    shares: f64,
    notional: f64,
    tx_hash: &str,
) {
    let msg = format!(
        "**TRADE DETECTED**\nSide: **{}**\nMarket: `{}`\nOutcome: **{}**\nPrice: **{:.4}**\nShares: **{:.2}**\nNotional: **${:.2}**\nTx: `{}`",
        side, market, outcome, price, shares, notional, tx_hash
    );
    send_main(client, &msg).await;
}

pub async fn send_poll_error(client: &Client, error: &str) {
    let msg = format!("**POLL ERROR**\n```\n{}\n```", error);
    send_error(client, &msg).await;
}

// ── Internal ────────────────────────────────────────────────────────────────

async fn send_main(client: &Client, content: &str) {
    send_webhook(client, DISCORD_WEBHOOK_URL, content).await;
}

async fn send_error(client: &Client, content: &str) {
    send_webhook(client, ERROR_DISCORD_WEBHOOK_URL, content).await;
}

async fn send_webhook(client: &Client, url: &str, content: &str) {
    let truncated = if content.len() > 1950 {
        format!("{}…\n(truncated)", &content[..1950])
    } else {
        content.to_string()
    };

    let body = json!({ "content": truncated });

    match client.post(url).json(&body).send().await {
        Ok(resp) if !resp.status().is_success() => {
            eprintln!("Discord webhook error: HTTP {}", resp.status());
        }
        Err(e) => {
            eprintln!("Discord webhook send failed: {e:#}");
        }
        _ => {}
    }
}

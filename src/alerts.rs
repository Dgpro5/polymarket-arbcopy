// Discord webhook alerts for the copy trading bot.
//
// Main webhook   → startup, successful copy trades.
// Error webhook  → failed orders, polling errors.

use reqwest::Client;
use serde_json::json;

use crate::consts::{DISCORD_WEBHOOK_URL, ERROR_DISCORD_WEBHOOK_URL};

// ── Public API ──────────────────────────────────────────────────────────────

pub async fn send_startup(client: &Client, balance: f64, max_session: f64) {
    let msg = format!(
        "**Copy Trading Bot Started**\nBalance: **${:.2}** USDC.e\nSession limit: **${:.2}** (80%)\nPolling every **10s**",
        balance, max_session
    );
    send_main(client, &msg).await;
}

pub async fn send_copy_success(
    client: &Client,
    side: &str,
    market: &str,
    price: f64,
    shares: f64,
    copy_usd: f64,
    order_id: &str,
) {
    let msg = format!(
        "**COPY TRADE PLACED**\nSide: **{}**\nMarket: `{}`\nPrice: **{:.4}**\nShares: **{:.2}**\nCost: **${:.2}**\nOrder: `{}`",
        side, market, price, shares, copy_usd, order_id
    );
    send_main(client, &msg).await;
}

pub async fn send_copy_error(client: &Client, context: &str, error: &str) {
    let msg = format!(
        "**COPY TRADE FAILED**\nContext: `{}`\nError:\n```\n{}\n```",
        context, error
    );
    send_error(client, &msg).await;
}

pub async fn send_poll_error(client: &Client, error: &str) {
    let msg = format!(
        "**POLL ERROR**\n```\n{}\n```",
        error
    );
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

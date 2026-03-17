// Discord webhook alerts for the trade tracker.

use reqwest::Client;
use serde_json::json;

use crate::consts::{DISCORD_WEBHOOK_URL, ERROR_DISCORD_WEBHOOK_URL, POLL_INTERVAL_MS, REPORT_INTERVAL_SECS};
use crate::copy::{BigReportData, ReportData};

// ── Public API ──────────────────────────────────────────────────────────────

pub async fn send_startup(client: &Client) {
    let msg = format!(
        "**Trade Tracker Started**\nTarget: `dustedfloor`\nPolling every **{}ms** | Reports every **{}m** | Big report every **6h**",
        POLL_INTERVAL_MS,
        REPORT_INTERVAL_SECS / 60
    );
    send_main(client, &msg).await;
}

pub async fn send_arb_report(client: &Client, report: &ReportData) {
    let net_pnl = report.arb_profit + report.total_sell_proceeds - report.total_spent;
    let pnl_emoji = if net_pnl >= 0.0 { "🟢" } else { "🔴" };

    let mut msg = format!(
        "**📊 30-MIN ARB REPORT**\n\
         Trades detected: **{}**\n\
         New arb matches: **{}**\n\
         All-time arbs: **{}**\n\
         Pending unmatched legs: **{}**\n\n\
         **💰 P&L SUMMARY**\n\
         Total spent (buys): **${:.2}**\n\
         Total received (sells): **${:.2}**\n\
         Arb profit (matched): **${:.4}**\n\
         Unmatched exposure: **${:.2}**\n\
         {} Net P&L: **${:.4}**",
        report.trades_detected,
        report.new_matches.len(),
        report.all_time_arb_count,
        report.pending_legs,
        report.total_spent,
        report.total_sell_proceeds,
        report.arb_profit,
        report.unmatched_exposure,
        pnl_emoji,
        net_pnl
    );

    if !report.new_matches.is_empty() {
        msg.push_str("\n\n**Top Arb Opportunities (by spread):**");
        // Show up to 10 best matches
        for (i, arb) in report.new_matches.iter().take(10).enumerate() {
            msg.push_str(&format!(
                "\n{}. `{}` — Yes **{:.1}c** + No **{:.1}c** = **{:.1}c** | spread **{:.1}c** | **{:.2}** shares | profit **${:.4}**",
                i + 1,
                arb.title,
                arb.yes_price * 100.0,
                arb.no_price * 100.0,
                (arb.yes_price + arb.no_price) * 100.0,
                arb.spread * 100.0,
                arb.matched_shares,
                arb.profit_usd
            ));
        }
        if report.new_matches.len() > 10 {
            msg.push_str(&format!("\n… and {} more", report.new_matches.len() - 10));
        }
    }

    send_main(client, &msg).await;
}

pub async fn send_big_report(client: &Client, report: &BigReportData) {
    let net_pnl = report.arb_profit + report.total_sell_proceeds - report.total_spent;
    let pnl_emoji = if net_pnl >= 0.0 { "🟢" } else { "🔴" };

    let total_matched_cost: f64 = report.all_matches.iter()
        .map(|m| (m.yes_price + m.no_price) * m.matched_shares)
        .sum();
    let total_matched_return: f64 = report.all_matches.iter()
        .map(|m| m.matched_shares)
        .sum();

    let mut msg = format!(
        "**📈 6-HOUR SUMMARY REPORT**\n\
         ━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
         Total trades: **{}**\n\
         Arb matches: **{}**\n\n\
         **💰 6H P&L**\n\
         Total spent (buys): **${:.2}**\n\
         Total received (sells): **${:.2}**\n\
         Arb cost (matched pairs): **${:.2}**\n\
         Arb guaranteed return: **${:.2}**\n\
         Arb profit: **${:.4}**\n\
         {} **Net P&L: ${:.4}**\n\
         ━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
        report.trades_detected,
        report.all_matches.len(),
        report.total_spent,
        report.total_sell_proceeds,
        total_matched_cost,
        total_matched_return,
        report.arb_profit,
        pnl_emoji,
        net_pnl
    );

    if !report.all_matches.is_empty() {
        msg.push_str("\n\n**All Arb Matches (by spread):**");
        for (i, arb) in report.all_matches.iter().take(20).enumerate() {
            msg.push_str(&format!(
                "\n{}. `{}` — Yes **{:.1}c** + No **{:.1}c** | spread **{:.1}c** | **{:.2}** shares | profit **${:.4}**",
                i + 1,
                arb.title,
                arb.yes_price * 100.0,
                arb.no_price * 100.0,
                arb.spread * 100.0,
                arb.matched_shares,
                arb.profit_usd
            ));
        }
        if report.all_matches.len() > 20 {
            msg.push_str(&format!("\n… and {} more", report.all_matches.len() - 20));
        }
    }

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

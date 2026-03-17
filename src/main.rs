mod alerts;
mod consts;
mod copy;

use anyhow::Result;
use std::fs;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    fs::create_dir_all(consts::DATA_DIR)?;

    let client = reqwest::Client::new();
    let mut state = copy::new_tracking_state();

    eprintln!("Trade tracker started.");
    eprintln!("Target: {} (dustedfloor)", consts::TARGET_WALLET);
    eprintln!("Poll interval: {}ms | Report interval: {}m", consts::POLL_INTERVAL_MS, consts::REPORT_INTERVAL_SECS / 60);

    alerts::send_startup(&client).await;

    let mut poll_interval = tokio::time::interval(Duration::from_millis(consts::POLL_INTERVAL_MS));
    let mut report_interval = tokio::time::interval(Duration::from_secs(consts::REPORT_INTERVAL_SECS));
    // Skip the first report tick (don't send an empty report on startup).
    report_interval.tick().await;

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                match copy::poll_and_track(&client, &mut state).await {
                    Ok(()) => {}
                    Err(e) => {
                        let err_msg = format!("{e:#}");
                        eprintln!("Poll error: {err_msg}");
                        alerts::send_poll_error(&client, &err_msg).await;
                    }
                }
            }
            _ = report_interval.tick() => {
                let report = copy::take_report(&mut state);
                eprintln!(
                    "Sending 5-min report: {} trades, {} new arbs, {} all-time, {} pending",
                    report.trades_detected, report.new_matches.len(),
                    report.all_time_arb_count, report.pending_legs
                );
                alerts::send_arb_report(&client, &report).await;
            }
        }
    }
}

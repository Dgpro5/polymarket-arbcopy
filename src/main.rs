mod alerts;
mod consts;
mod copy;

use anyhow::Result;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let client = reqwest::Client::new();
    let mut state = copy::new_tracking_state();

    eprintln!("Trade tracker started.");
    eprintln!("Target: {} (dustedfloor)", consts::TARGET_WALLET);
    eprintln!("Poll interval: {}ms", consts::POLL_INTERVAL_MS);

    alerts::send_startup(&client).await;

    let mut interval = tokio::time::interval(Duration::from_millis(consts::POLL_INTERVAL_MS));

    loop {
        interval.tick().await;

        match copy::poll_and_log(&client, &mut state).await {
            Ok(()) => {}
            Err(e) => {
                let err_msg = format!("{e:#}");
                eprintln!("Poll error: {err_msg}");
                alerts::send_poll_error(&client, &err_msg).await;
            }
        }
    }
}

mod alerts;
mod auth;
mod chain;
mod consts;
mod copy;
mod encrypt;
mod redeem;

use anyhow::Result;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    fs::create_dir_all(consts::DATA_DIR)?;

    // 1. Decrypt / first-time setup of private key
    let private_key = encrypt::get_private_key()?;

    // 2. Setup wallet + derive API credentials (L1 auth)
    let wallet = auth::setup_wallet(&private_key).await?;

    let client = reqwest::Client::new();

    // 3. Ensure on-chain approvals (USDC + ERC-1155)
    chain::ensure_approvals(&client, &wallet).await?;

    // 4. Initial balance check & rebalance (USDC.e ≥ $25, POL ≥ 50)
    chain::check_and_rebalance(&client, &wallet).await?;

    // 5. Check USDC.e balance and compute session limit (80%)
    let balance = chain::get_balance(&client, &wallet.address).await?;
    let max_session = balance * consts::MAX_BALANCE_FRACTION;
    eprintln!("USDC.e balance: ${:.2}", balance);
    eprintln!("Session limit:  ${:.2} ({}%)", max_session, (consts::MAX_BALANCE_FRACTION * 100.0) as u32);

    if balance < 1.0 {
        return Err(anyhow::anyhow!(
            "USDC.e balance too low (${:.2}). Deposit funds to your wallet first.",
            balance
        ));
    }

    // 6. Initialize copy state
    let mut state = copy::new_copy_state(max_session);

    // 7. Startup alert
    eprintln!("Copy trading bot started.");
    eprintln!("Target: {} ({})", consts::TARGET_WALLET, "dustedfloor");
    eprintln!("Copy fraction: {:.2}% of target notional", consts::COPY_FRACTION * 100.0);
    eprintln!("Poll interval: {}s", consts::POLL_INTERVAL_SECS);

    alerts::send_startup(&client, balance, max_session).await;

    // 8. Spawn background redemption loop (checks every 60s, redeems after 15 min)
    let redeem_key = private_key.clone();
    tokio::spawn(async move {
        redeem::run_redemption_loop(redeem_key).await;
    });

    // 9. Spawn background balance check every 5 minutes
    let balance_wallet = Arc::clone(&wallet);
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut interval = tokio::time::interval(Duration::from_secs(
            consts::BALANCE_CHECK_INTERVAL_SECS,
        ));
        // Skip the first tick (we already checked on startup)
        interval.tick().await;

        loop {
            interval.tick().await;
            if let Err(e) = chain::check_and_rebalance(&client, &balance_wallet).await {
                eprintln!("Balance check error: {e:#}");
            }
        }
    });

    // 10. Main polling loop
    let mut interval = tokio::time::interval(Duration::from_secs(consts::POLL_INTERVAL_SECS));

    loop {
        interval.tick().await;

        match copy::poll_and_copy(&client, &wallet, &mut state).await {
            Ok(()) => {}
            Err(e) => {
                let err_msg = format!("{e:#}");
                eprintln!("Poll error: {err_msg}");
                alerts::send_poll_error(&client, &err_msg).await;
            }
        }
    }
}

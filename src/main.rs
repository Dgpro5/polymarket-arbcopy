mod alerts;
mod auth;
mod chain;
mod consts;
mod copy;
mod encrypt;

use anyhow::Result;
use std::fs;
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

    // 4. Check USDC.e balance and compute session limit (80%)
    let balance = chain::get_balance(&client, &wallet.address).await?;
    let max_session = balance * consts::MAX_BALANCE_FRACTION;
    eprintln!("USDC.e balance: ${:.2}", balance);
    eprintln!("Session limit:  ${:.2} ({}%)", max_session, (consts::MAX_BALANCE_FRACTION * 100.0) as u32);

    if balance < 1.0 {
        return Err(anyhow::anyhow!(
            "USDC.e balance too low (${:.2}). Deposit USDC.e to your wallet first.",
            balance
        ));
    }

    // 5. Initialize copy state
    let mut state = copy::new_copy_state(max_session);

    // 6. Startup alert
    eprintln!("Copy trading bot started.");
    eprintln!("Target: {} ({})", consts::TARGET_WALLET, "dustedfloor");
    eprintln!("Copy fraction: {:.2}% of target notional", consts::COPY_FRACTION * 100.0);
    eprintln!("Poll interval: {}s", consts::POLL_INTERVAL_SECS);

    alerts::send_startup(&client, balance, max_session).await;

    // 7. Main polling loop
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

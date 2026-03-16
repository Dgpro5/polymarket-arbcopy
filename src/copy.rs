// Copy trading logic — trade discovery, sizing, order execution, position tracking.

use anyhow::{Context, Result, anyhow};
use ethers::prelude::*;
use ethers::signers::Signer;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::alerts;
use crate::auth::{self, TradingWallet};
use crate::consts::{
    CHAIN_ID, CLOB_API, COPY_FRACTION, CTF_EXCHANGE_ADDRESS, DATA_API, TARGET_WALLET,
    ZERO_ADDRESS,
};

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TargetTrade {
    pub id: String,
    #[serde(rename = "conditionId", default)]
    pub condition_id: String,
    #[serde(rename = "proxyWalletAddress", default)]
    pub proxy_wallet: String,
    #[serde(rename = "asset", default)]
    pub token_id: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    #[serde(rename = "createdAt", default)]
    pub timestamp: String,
    #[serde(rename = "title", default)]
    pub market: String,
    #[serde(rename = "usdcSize", default)]
    pub usdc_size: f64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Position {
    pub token_id: String,
    pub shares: f64,
    pub notional: f64,
}

pub struct CopyState {
    pub positions: HashMap<String, Position>,
    pub session_notional: f64,
    pub max_session_notional: f64,
    pub last_poll_timestamp: String,
    pub copied_trade_ids: HashSet<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct PolymarketOrderStruct {
    pub salt: u64,
    pub maker: String,
    pub signer: String,
    pub taker: String,
    #[serde(rename = "tokenId")]
    pub token_id: String,
    #[serde(rename = "makerAmount")]
    pub maker_amount: String,
    #[serde(rename = "takerAmount")]
    pub taker_amount: String,
    pub side: String,
    pub expiration: String,
    pub nonce: String,
    #[serde(rename = "feeRateBps")]
    pub fee_rate_bps: String,
    pub signature: String,
    #[serde(rename = "signatureType")]
    pub signature_type: u8,
}

#[derive(Debug, Serialize)]
pub struct CreateOrderRequest {
    pub order: PolymarketOrderStruct,
    pub owner: String,
    #[serde(rename = "orderType")]
    pub order_type: String,
    #[serde(rename = "deferExec")]
    pub defer_exec: bool,
}

// ── Public API ───────────────────────────────────────────────────────────────

pub fn new_copy_state(max_session_notional: f64) -> CopyState {
    let now = chrono_now_iso();
    CopyState {
        positions: HashMap::new(),
        session_notional: 0.0,
        max_session_notional,
        last_poll_timestamp: now,
        copied_trade_ids: HashSet::new(),
    }
}

/// Main tick: poll for new trades, filter, copy.
pub async fn poll_and_copy(
    client: &Client,
    wallet: &Arc<TradingWallet>,
    state: &mut CopyState,
) -> Result<()> {
    let trades = fetch_target_trades(client, &state.last_poll_timestamp).await?;

    if trades.is_empty() {
        return Ok(());
    }

    let mut newest_ts = state.last_poll_timestamp.clone();

    for trade in &trades {
        // Advance cursor
        if trade.timestamp > newest_ts {
            newest_ts = trade.timestamp.clone();
        }

        // Dedup
        if state.copied_trade_ids.contains(&trade.id) {
            continue;
        }

        // Sizing
        let target_notional = trade.size * trade.price;
        let copy_usd = target_notional * COPY_FRACTION;
        if copy_usd < 0.50 {
            eprintln!(
                "Skip trade {} — copy size ${:.2} too small (target ${:.2})",
                trade.id, copy_usd, target_notional
            );
            state.copied_trade_ids.insert(trade.id.clone());
            continue;
        }

        // Risk check
        if state.session_notional + copy_usd > state.max_session_notional {
            eprintln!(
                "Skip trade {} — session limit reached (${:.2} + ${:.2} > ${:.2})",
                trade.id, state.session_notional, copy_usd, state.max_session_notional
            );
            continue;
        }

        let copy_shares = copy_usd / trade.price;

        eprintln!(
            "Copying trade: {} {} {:.2} shares @ {:.4} (${:.2}) | target: {:.2} shares (${:.2})",
            trade.side,
            &trade.market,
            copy_shares,
            trade.price,
            copy_usd,
            trade.size,
            target_notional
        );

        match execute_copy_trade(client, wallet, trade, copy_shares, copy_usd).await {
            Ok(order_id) => {
                state.copied_trade_ids.insert(trade.id.clone());
                state.session_notional += copy_usd;

                let pos = state
                    .positions
                    .entry(trade.token_id.clone())
                    .or_insert(Position {
                        token_id: trade.token_id.clone(),
                        shares: 0.0,
                        notional: 0.0,
                    });

                if trade.side == "BUY" {
                    pos.shares += copy_shares;
                    pos.notional += copy_usd;
                } else {
                    pos.shares -= copy_shares;
                    pos.notional -= copy_usd;
                }

                alerts::send_copy_success(
                    client,
                    &trade.side,
                    &trade.market,
                    trade.price,
                    copy_shares,
                    copy_usd,
                    &order_id,
                )
                .await;

                eprintln!("  -> Order placed: {order_id}");
            }
            Err(e) => {
                let err_msg = format!("{e:#}");
                alerts::send_copy_error(
                    client,
                    &format!("{} {} @ {:.4}", trade.side, trade.market, trade.price),
                    &err_msg,
                )
                .await;
                eprintln!("  -> Failed: {err_msg}");
                // Mark as copied to avoid retrying the same failing trade
                state.copied_trade_ids.insert(trade.id.clone());
            }
        }
    }

    state.last_poll_timestamp = newest_ts;
    Ok(())
}

// ── Trade discovery ──────────────────────────────────────────────────────────

async fn fetch_target_trades(
    client: &Client,
    since: &str,
) -> Result<Vec<TargetTrade>> {
    let url = format!(
        "{DATA_API}/activity?user={TARGET_WALLET}&type=TRADE&limit=100&sortBy=TIMESTAMP&sortDirection=DESC&start={since}"
    );

    let resp = client.get(&url).send().await.context("fetch target trades")?;
    let status = resp.status();
    let body = resp.text().await?;

    if !status.is_success() {
        return Err(anyhow!("Data API error ({}): {}", status, body));
    }

    let trades: Vec<TargetTrade> =
        serde_json::from_str(&body).context("parse target trades")?;
    Ok(trades)
}

// ── Order execution ──────────────────────────────────────────────────────────

async fn execute_copy_trade(
    client: &Client,
    wallet: &Arc<TradingWallet>,
    trade: &TargetTrade,
    copy_shares: f64,
    _copy_usd: f64,
) -> Result<String> {
    let fee_bps = get_fee_rate(client, &trade.token_id).await.unwrap_or(1000);

    let order = build_order(wallet, &trade.token_id, copy_shares, trade.price, &trade.side, fee_bps).await?;

    let result = submit_order(client, wallet, order).await?;

    let order_id = result
        .get("orderID")
        .or_else(|| result.get("orderID"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    if let Some(false) = result.get("success").and_then(|v| v.as_bool()) {
        let err = result
            .get("errorMsg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(anyhow!("Order rejected: {err}"));
    }

    Ok(order_id)
}

async fn get_fee_rate(client: &Client, token_id: &str) -> Result<u64> {
    let url = format!("{CLOB_API}/fee-rate?token_id={token_id}");
    let resp: Value = client.get(&url).send().await?.json().await?;
    resp.get("fee_rate_bps")
        .or_else(|| resp.get("feeRateBps"))
        .and_then(|v| v.as_str().or_else(|| v.as_u64().map(|_| "").or(None)))
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            resp.get("fee_rate_bps")
                .or_else(|| resp.get("feeRateBps"))
                .and_then(|v| v.as_u64())
        })
        .ok_or_else(|| anyhow!("could not parse fee rate: {resp}"))
}

async fn build_order(
    wallet: &Arc<TradingWallet>,
    token_id: &str,
    shares: f64,
    price: f64,
    side: &str,
    fee_bps: u64,
) -> Result<CreateOrderRequest> {
    let side_uint: u8 = if side == "BUY" { 0 } else { 1 };

    let (maker_amount, taker_amount) = if side_uint == 0 {
        // BUY: we pay USDC (maker), receive shares (taker)
        let maker = (price * shares * 1_000_000.0).round() as u128;
        let taker = (shares * 1_000_000.0).round() as u128;
        (maker, taker)
    } else {
        // SELL: we give shares (maker), receive USDC (taker)
        let maker = (shares * 1_000_000.0).round() as u128;
        let taker = (price * shares * 1_000_000.0).round() as u128;
        (maker, taker)
    };

    const MIN_MAKER: u128 = 1_000_000;
    if maker_amount < MIN_MAKER {
        return Err(anyhow!(
            "Maker amount ${:.4} below $1.00 minimum",
            maker_amount as f64 / 1_000_000.0
        ));
    }

    let mut salt_bytes = [0u8; 8];
    getrandom::getrandom(&mut salt_bytes).map_err(|e| anyhow!("RNG: {e}"))?;
    let salt = u64::from_le_bytes(salt_bytes);

    let signature = eip712_order_signature(
        &wallet.wallet,
        wallet.address,
        token_id,
        maker_amount,
        taker_amount,
        side_uint,
        salt,
        fee_bps,
        0, // expiration: 0 for FOK
    )
    .await?;

    Ok(CreateOrderRequest {
        order: PolymarketOrderStruct {
            salt,
            maker: format!("{:#x}", wallet.address),
            signer: format!("{:#x}", wallet.address),
            taker: ZERO_ADDRESS.to_string(),
            token_id: token_id.to_string(),
            maker_amount: maker_amount.to_string(),
            taker_amount: taker_amount.to_string(),
            side: side.to_string(),
            expiration: "0".to_string(),
            nonce: "0".to_string(),
            fee_rate_bps: fee_bps.to_string(),
            signature,
            signature_type: 0,
        },
        owner: wallet.creds.api_key.clone(),
        order_type: "FOK".to_string(),
        defer_exec: false,
    })
}

async fn eip712_order_signature(
    wallet: &LocalWallet,
    address: Address,
    token_id: &str,
    maker_amount: u128,
    taker_amount: u128,
    side: u8,
    salt: u64,
    fee_bps: u64,
    expiration: u64,
) -> Result<String> {
    use ethers::types::transaction::eip712::TypedData;

    let td: TypedData = serde_json::from_value(json!({
        "primaryType": "Order",
        "domain": {
            "name": "Polymarket CTF Exchange", "version": "1",
            "chainId": CHAIN_ID, "verifyingContract": CTF_EXCHANGE_ADDRESS
        },
        "types": {
            "EIP712Domain": [
                {"name": "name",              "type": "string"},
                {"name": "version",           "type": "string"},
                {"name": "chainId",           "type": "uint256"},
                {"name": "verifyingContract", "type": "address"}
            ],
            "Order": [
                {"name": "salt",          "type": "uint256"},
                {"name": "maker",         "type": "address"},
                {"name": "signer",        "type": "address"},
                {"name": "taker",         "type": "address"},
                {"name": "tokenId",       "type": "uint256"},
                {"name": "makerAmount",   "type": "uint256"},
                {"name": "takerAmount",   "type": "uint256"},
                {"name": "expiration",    "type": "uint256"},
                {"name": "nonce",         "type": "uint256"},
                {"name": "feeRateBps",    "type": "uint256"},
                {"name": "side",          "type": "uint8"},
                {"name": "signatureType", "type": "uint8"}
            ]
        },
        "message": {
            "salt": salt.to_string(), "maker": format!("{:#x}", address),
            "signer": format!("{:#x}", address), "taker": ZERO_ADDRESS,
            "tokenId": token_id, "makerAmount": maker_amount.to_string(),
            "takerAmount": taker_amount.to_string(), "expiration": expiration.to_string(),
            "nonce": "0", "feeRateBps": fee_bps.to_string(),
            "side": side, "signatureType": 0u8
        }
    }))?;

    let sig = wallet
        .sign_typed_data(&td)
        .await
        .map_err(|e| anyhow!("Order EIP-712 sign failed: {e}"))?;
    Ok(format!("0x{}", hex::encode(sig.to_vec())))
}

async fn submit_order(
    client: &Client,
    wallet: &Arc<TradingWallet>,
    order: CreateOrderRequest,
) -> Result<Value> {
    let body = serde_json::to_string(&order)?;
    let ts = auth::now_secs();
    let sig = auth::l2_signature(&wallet.creds.secret, ts, "POST", "/order", &body)?;

    let resp = client
        .post(format!("{CLOB_API}/order"))
        .header("Content-Type", "application/json")
        .header("POLY_ADDRESS", format!("{:#x}", wallet.address))
        .header("POLY_SIGNATURE", sig)
        .header("POLY_TIMESTAMP", ts.to_string())
        .header("POLY_API_KEY", &wallet.creds.api_key)
        .header("POLY_PASSPHRASE", &wallet.creds.passphrase)
        .body(body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(anyhow!("order failed (HTTP): {err}"));
    }
    Ok(resp.json().await?)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple ISO-8601 format without external crate
    let s = secs;
    let days = s / 86400;
    let rem = s % 86400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let sec = rem % 60;

    // Days since epoch to Y-M-D (simplified)
    let (y, mo, d) = epoch_days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{sec:02}Z")
}

fn epoch_days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

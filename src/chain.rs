// On-chain helpers — USDC.e balance check and token approvals.

use anyhow::{Context, Result, anyhow};
use ethers::prelude::*;
use ethers::signers::Signer;
use reqwest::Client;
use serde_json::{Value, json};
use std::env;
use std::time::Duration;

use crate::auth::TradingWallet;
use crate::consts::{
    ANKR_API_KEY_ENV, CHAIN_ID, CONDITIONAL_TOKENS_ADDRESS, CTF_EXCHANGE_ADDRESS,
    MIN_POL_BALANCE, MIN_USDC_BALANCE, POL_TOP_UP_USDC, USDC_E_POLYGON, USDC_TOP_UP_AMOUNT,
};

/// Native token placeholder used by OpenOcean and other DEX aggregators.
const NATIVE_TOKEN: &str = "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE";
/// Default slippage percentage for OpenOcean swaps.
const SWAP_SLIPPAGE: f64 = 3.0;

// ── Public API ───────────────────────────────────────────────────────────────

/// Get USDC.e balance for an address (returns human-readable USD amount).
pub async fn get_balance(client: &Client, address: &Address) -> Result<f64> {
    let rpc_url = ankr_rpc()?;
    let addr_hex = format!("{:x}", address);
    let calldata = format!("0x70a08231{:0>64}", addr_hex);

    let body = json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{ "to": USDC_E_POLYGON, "data": calldata }, "latest"],
        "id": 1
    });

    let resp = client.post(&rpc_url).json(&body).send().await?;
    let raw = resp.text().await?;
    let v: Value = serde_json::from_str(&raw)?;

    if let Some(err) = v.get("error") {
        return Err(anyhow!("eth_call error: {err}"));
    }

    let hex = v
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0")
        .trim_start_matches("0x");
    let raw_amount = u128::from_str_radix(hex, 16).unwrap_or(0);
    Ok(raw_amount as f64 / 1_000_000.0)
}

/// Ensure both USDC allowance and ERC-1155 approval are in place.
pub async fn ensure_approvals(client: &Client, wallet: &TradingWallet) -> Result<()> {
    ensure_allowance(client, wallet, CTF_EXCHANGE_ADDRESS)
        .await
        .context("USDC allowance")?;
    ensure_ctf_token_approval(client, wallet)
        .await
        .context("ERC-1155 approval")?;
    Ok(())
}

/// Get POL (native token) balance for an address.
pub async fn get_pol_balance(client: &Client, address: &Address) -> Result<f64> {
    let rpc_url = ankr_rpc()?;
    let body = json!({
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [format!("{:#x}", address), "latest"],
        "id": 1
    });
    let v: Value = client.post(&rpc_url).json(&body).send().await?.json().await?;
    if let Some(err) = v.get("error") {
        return Err(anyhow!("eth_getBalance error: {err}"));
    }
    let hex = v["result"]
        .as_str()
        .unwrap_or("0x0")
        .trim_start_matches("0x");
    Ok(u128::from_str_radix(hex, 16).unwrap_or(0) as f64 / 1e18)
}

/// Check and rebalance USDC.e and POL:
/// - If USDC.e < $25, swap POL → USDC.e to get $25.
/// - If POL < 50, swap $10 USDC.e → POL.
pub async fn check_and_rebalance(client: &Client, wallet: &TradingWallet) -> Result<()> {
    let usdc = get_balance(client, &wallet.address).await?;
    let pol = get_pol_balance(client, &wallet.address).await?;

    eprintln!("Balance check: ${:.2} USDC.e | {:.2} POL", usdc, pol);

    // POL too low → swap USDC.e → POL
    if pol < MIN_POL_BALANCE {
        if usdc >= POL_TOP_UP_USDC + 1.0 {
            eprintln!(
                "POL low ({pol:.2} < {MIN_POL_BALANCE}) — swapping ${POL_TOP_UP_USDC:.2} USDC.e → POL via OpenOcean…"
            );
            match openocean_swap(client, wallet, USDC_E_POLYGON, NATIVE_TOKEN, POL_TOP_UP_USDC, 6).await {
                Ok(hash) => eprintln!("USDC.e→POL swap confirmed: {hash}"),
                Err(e) => eprintln!("WARN: USDC.e→POL swap failed: {e:#}"),
            }
        } else {
            eprintln!("WARN: POL low ({pol:.2}) but not enough USDC.e (${usdc:.2}) to top up");
        }
    }

    // USDC.e too low → swap POL → USDC.e
    let usdc = get_balance(client, &wallet.address).await.unwrap_or(usdc);
    if usdc < MIN_USDC_BALANCE {
        let pol = get_pol_balance(client, &wallet.address).await.unwrap_or(pol);
        if pol > MIN_POL_BALANCE {
            // Estimate how much POL to swap for ~$25 USDC.e
            // POL is roughly $0.40-0.60, so ~60 POL ≈ $25. Use a generous amount.
            let swap_pol = (USDC_TOP_UP_AMOUNT * 3.0).min(pol - MIN_POL_BALANCE);
            if swap_pol > 1.0 {
                eprintln!(
                    "USDC.e low (${usdc:.2} < ${MIN_USDC_BALANCE}) — swapping {swap_pol:.2} POL → USDC.e via OpenOcean…"
                );
                match openocean_swap(client, wallet, NATIVE_TOKEN, USDC_E_POLYGON, swap_pol, 18).await {
                    Ok(hash) => eprintln!("POL→USDC.e swap confirmed: {hash}"),
                    Err(e) => eprintln!("WARN: POL→USDC.e swap failed: {e:#}"),
                }
            }
        } else {
            eprintln!("WARN: USDC.e low (${usdc:.2}) and POL too low ({pol:.2}) to swap");
        }
    }

    Ok(())
}

// ── OpenOcean DEX swap ──────────────────────────────────────────────────────

/// Execute an on-chain swap via OpenOcean DEX aggregator.
///
/// `token_in` / `token_out` are contract addresses (use `NATIVE_TOKEN` for POL).
/// `amount` is in human units; `decimals` is the token-in decimal count (18 for POL, 6 for USDC.e).
async fn openocean_swap(
    client: &Client,
    wallet: &TradingWallet,
    token_in: &str,
    token_out: &str,
    amount: f64,
    decimals: u32,
) -> Result<String> {
    let rpc_url = ankr_rpc()?;
    let gas_price = get_gas_price(client, &rpc_url).await?;
    let account = format!("{:#x}", wallet.address);

    let amount_raw = (amount * 10f64.powi(decimals as i32)) as u128;

    let url = format!(
        "https://open-api.openocean.finance/v4/polygon/swap?\
         inTokenAddress={token_in}&outTokenAddress={token_out}\
         &amountDecimals={amount_raw}&gasPriceDecimals={gas_price}\
         &slippage={SWAP_SLIPPAGE}&account={account}"
    );

    let resp: Value = client
        .get(&url)
        .send()
        .await
        .context("OpenOcean API request failed")?
        .json()
        .await
        .context("OpenOcean returned non-JSON")?;

    if resp.get("code").and_then(|c| c.as_u64()) != Some(200) {
        let msg = resp.get("error")
            .or_else(|| resp.get("message"))
            .unwrap_or(&resp);
        return Err(anyhow!("OpenOcean error: {msg}"));
    }

    let data = resp.get("data").ok_or_else(|| anyhow!("OpenOcean: missing 'data' in response"))?;
    let to_addr = data["to"].as_str().ok_or_else(|| anyhow!("OpenOcean: missing 'to'"))?;
    let calldata = data["data"].as_str().ok_or_else(|| anyhow!("OpenOcean: missing 'data.data'"))?;
    let value_str = data["value"].as_str().unwrap_or("0");
    let est_gas = data["estimatedGas"]
        .as_u64()
        .unwrap_or(300_000);

    // ERC-20 tokens need approval for the OpenOcean router before swapping.
    if token_in != NATIVE_TOKEN {
        ensure_allowance(client, wallet, to_addr).await
            .context("OpenOcean: approve token for router")?;
    }

    let value_wei = if value_str.starts_with("0x") {
        u128::from_str_radix(value_str.trim_start_matches("0x"), 16).unwrap_or(0)
    } else {
        value_str.parse::<u128>().unwrap_or(0)
    };

    let cd_bytes = hex::decode(calldata.trim_start_matches("0x"))
        .context("decode OpenOcean calldata")?;

    let nonce = get_nonce(client, &rpc_url, &wallet.address).await?;

    use ethers::types::transaction::eip2718::TypedTransaction;
    let tx = TypedTransaction::Legacy(ethers::types::TransactionRequest {
        from: Some(wallet.address),
        to: Some(to_addr.parse::<Address>().context("parse OpenOcean router")?.into()),
        nonce: Some(U256::from(nonce)),
        gas: Some(U256::from((est_gas as f64 * 1.5) as u64)),
        gas_price: Some(U256::from(gas_price * 2)),
        data: Some(cd_bytes.into()),
        value: Some(U256::from(value_wei)),
        chain_id: Some(U64::from(CHAIN_ID)),
        ..Default::default()
    });

    let sig = wallet
        .wallet
        .sign_transaction(&tx)
        .await
        .map_err(|e| anyhow!("sign OpenOcean swap: {e}"))?;
    let raw_tx = format!("0x{}", hex::encode(tx.rlp_signed(&sig)));

    let hash = send_raw_tx(client, &rpc_url, &raw_tx).await?;
    wait_for_receipt(client, &rpc_url, &hash).await?;
    Ok(hash)
}

// ── USDC allowance ──────────────────────────────────────────────────────────

async fn get_allowance(client: &Client, owner: &Address, spender: &str) -> Result<f64> {
    let rpc_url = ankr_rpc()?;
    let owner_hex = format!("{:x}", owner);
    let spender_hex = spender.trim_start_matches("0x");
    let calldata = format!("0xdd62ed3e{:0>64}{:0>64}", owner_hex, spender_hex);

    let body = json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{ "to": USDC_E_POLYGON, "data": calldata }, "latest"],
        "id": 1
    });

    let raw = client.post(&rpc_url).json(&body).send().await?.text().await?;
    let v: Value = serde_json::from_str(&raw)?;
    if let Some(err) = v.get("error") {
        return Err(anyhow!("allowance error: {err}"));
    }
    let hex = v
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0")
        .trim_start_matches("0x");
    Ok(u128::from_str_radix(hex, 16).unwrap_or(0) as f64 / 1_000_000.0)
}

async fn ensure_allowance(
    client: &Client,
    wallet: &TradingWallet,
    spender: &str,
) -> Result<()> {
    if get_allowance(client, &wallet.address, spender).await? >= 1000.0 {
        return Ok(());
    }

    eprintln!("USDC.e allowance low — sending approve…");

    let rpc_url = ankr_rpc()?;
    let nonce = get_nonce(client, &rpc_url, &wallet.address).await?;
    let gas_price = get_gas_price(client, &rpc_url).await?;

    let sp = spender.trim_start_matches("0x");
    let cd = hex::decode(format!("095ea7b3{:0>64}{}", sp, "f".repeat(64)))?;

    use ethers::types::transaction::eip2718::TypedTransaction;
    let tx = TypedTransaction::Legacy(ethers::types::TransactionRequest {
        from: Some(wallet.address),
        to: Some(USDC_E_POLYGON.parse::<Address>().unwrap().into()),
        nonce: Some(U256::from(nonce)),
        gas: Some(U256::from(100_000u64)),
        gas_price: Some(U256::from(gas_price * 3)),
        data: Some(cd.into()),
        value: Some(U256::zero()),
        chain_id: Some(U64::from(CHAIN_ID)),
        ..Default::default()
    });

    let sig = wallet
        .wallet
        .sign_transaction(&tx)
        .await
        .map_err(|e| anyhow!("sign approve: {e}"))?;
    let raw_tx = format!("0x{}", hex::encode(tx.rlp_signed(&sig)));

    let hash = send_raw_tx(client, &rpc_url, &raw_tx).await?;
    wait_for_receipt(client, &rpc_url, &hash).await?;
    eprintln!("USDC.e approval set: {hash}");
    Ok(())
}

// ── ERC-1155 approval ────────────────────────────────────────────────────────

async fn is_approved_for_all(
    client: &Client,
    owner: &Address,
    operator: &str,
    contract: &str,
) -> Result<bool> {
    let rpc_url = ankr_rpc()?;
    let o = format!("{:x}", owner);
    let op = operator.trim_start_matches("0x");
    let cd = format!("e985e9c5{:0>64}{:0>64}", o, op);

    let body = json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": contract, "data": format!("0x{cd}")}, "latest"],
        "id": 1
    });

    let raw = client.post(&rpc_url).json(&body).send().await?.text().await?;
    let v: Value = serde_json::from_str(&raw)?;
    if let Some(err) = v.get("error") {
        return Err(anyhow!("isApprovedForAll error: {err}"));
    }
    let hex = v
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0")
        .trim_start_matches("0x");
    Ok(u128::from_str_radix(hex, 16).unwrap_or(0) != 0)
}

async fn ensure_ctf_token_approval(client: &Client, wallet: &TradingWallet) -> Result<()> {
    if is_approved_for_all(
        client,
        &wallet.address,
        CTF_EXCHANGE_ADDRESS,
        CONDITIONAL_TOKENS_ADDRESS,
    )
    .await?
    {
        return Ok(());
    }

    eprintln!("ERC-1155 approval missing — sending setApprovalForAll…");

    let rpc_url = ankr_rpc()?;
    let nonce = get_nonce(client, &rpc_url, &wallet.address).await?;
    let gas_price = get_gas_price(client, &rpc_url).await?;

    let op = CTF_EXCHANGE_ADDRESS.trim_start_matches("0x");
    let cd = hex::decode(format!("a22cb465{:0>64}{:0>64}", op, "1"))?;

    use ethers::types::transaction::eip2718::TypedTransaction;
    let tx = TypedTransaction::Legacy(ethers::types::TransactionRequest {
        from: Some(wallet.address),
        to: Some(
            CONDITIONAL_TOKENS_ADDRESS
                .parse::<Address>()
                .unwrap()
                .into(),
        ),
        nonce: Some(U256::from(nonce)),
        gas: Some(U256::from(100_000u64)),
        gas_price: Some(U256::from(gas_price * 3)),
        data: Some(cd.into()),
        value: Some(U256::zero()),
        chain_id: Some(U64::from(CHAIN_ID)),
        ..Default::default()
    });

    let sig = wallet
        .wallet
        .sign_transaction(&tx)
        .await
        .map_err(|e| anyhow!("sign setApprovalForAll: {e}"))?;
    let raw_tx = format!("0x{}", hex::encode(tx.rlp_signed(&sig)));

    let hash = send_raw_tx(client, &rpc_url, &raw_tx).await?;
    wait_for_receipt(client, &rpc_url, &hash).await?;
    eprintln!("ERC-1155 approval set: {hash}");
    Ok(())
}

// ── RPC helpers ──────────────────────────────────────────────────────────────

fn ankr_rpc() -> Result<String> {
    let key = env::var(ANKR_API_KEY_ENV)
        .with_context(|| format!("Missing '{ANKR_API_KEY_ENV}' in .env"))?;
    Ok(format!("https://rpc.ankr.com/polygon/{}", key.trim()))
}

async fn get_nonce(client: &Client, rpc_url: &str, address: &Address) -> Result<u64> {
    let body = json!({
        "jsonrpc": "2.0", "method": "eth_getTransactionCount",
        "params": [format!("{:#x}", address), "latest"], "id": 1
    });
    let v: Value = client.post(rpc_url).json(&body).send().await?.json().await?;
    v["result"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| anyhow!("bad nonce: {v}"))
}

async fn get_gas_price(client: &Client, rpc_url: &str) -> Result<u128> {
    let body = json!({"jsonrpc":"2.0","method":"eth_gasPrice","params":[],"id":1});
    let v: Value = client.post(rpc_url).json(&body).send().await?.json().await?;
    v["result"]
        .as_str()
        .and_then(|s| u128::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| anyhow!("bad gas price: {v}"))
}

async fn send_raw_tx(client: &Client, rpc_url: &str, raw_tx: &str) -> Result<String> {
    let body = json!({
        "jsonrpc": "2.0", "method": "eth_sendRawTransaction",
        "params": [raw_tx], "id": 1
    });
    let v: Value = client.post(rpc_url).json(&body).send().await?.json().await?;
    if let Some(err) = v.get("error") {
        return Err(anyhow!("tx failed: {err}"));
    }
    v["result"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("no tx hash: {v}"))
}

async fn wait_for_receipt(client: &Client, rpc_url: &str, tx_hash: &str) -> Result<()> {
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let body = json!({
            "jsonrpc": "2.0", "method": "eth_getTransactionReceipt",
            "params": [tx_hash], "id": 1
        });
        let v: Value = client.post(rpc_url).json(&body).send().await?.json().await?;
        if let Some(r) = v.get("result").filter(|r| !r.is_null()) {
            return if r["status"].as_str().unwrap_or("0x0") == "0x1" {
                Ok(())
            } else {
                Err(anyhow!("Tx reverted. Check wallet has POL for gas."))
            };
        }
    }
    Err(anyhow!("Tx not confirmed within 30s: {tx_hash}"))
}

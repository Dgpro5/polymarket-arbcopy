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
    ANKR_API_KEY_ENV, CHAIN_ID, CONDITIONAL_TOKENS_ADDRESS, CTF_EXCHANGE_ADDRESS, USDC_E_POLYGON,
};

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

// L1/L2 authentication and wallet setup for Polymarket CLOB API.

use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE as BASE64};
use ethers::prelude::*;
use ethers::signers::{LocalWallet, Signer};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::consts::{CHAIN_ID, CLOB_API};

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ApiCredentials {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

#[derive(Debug, Clone)]
pub struct TradingWallet {
    pub wallet: LocalWallet,
    pub address: Address,
    pub proxy_address: Address,
    pub creds: ApiCredentials,
}

// ── Wallet setup ─────────────────────────────────────────────────────────────

pub async fn setup_wallet(private_key: &str) -> Result<Arc<TradingWallet>> {
    let client = Client::new();

    let wallet_signer = private_key
        .trim()
        .parse::<LocalWallet>()
        .context("parse private key")?;
    let address = wallet_signer.address();
    eprintln!("Wallet: {:#x}", address);

    let creds = get_or_create_api_creds(&wallet_signer, address, &client).await?;
    let proxy_address = get_proxy_wallet(&client, address, &creds).await?;
    eprintln!("Proxy wallet: {:#x}", proxy_address);

    Ok(Arc::new(TradingWallet {
        wallet: wallet_signer,
        address,
        proxy_address,
        creds,
    }))
}

// ── L1 EIP-712 Auth (wallet signature for API credential derivation) ────────

async fn l1_auth_signature(
    wallet: &LocalWallet,
    address: Address,
    timestamp: i64,
    nonce: u64,
) -> Result<String> {
    use ethers::types::transaction::eip712::TypedData;

    let td: TypedData = serde_json::from_value(json!({
        "primaryType": "ClobAuth",
        "domain": { "name": "ClobAuthDomain", "version": "1", "chainId": CHAIN_ID },
        "types": {
            "EIP712Domain": [
                {"name": "name",    "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"}
            ],
            "ClobAuth": [
                {"name": "address",   "type": "address"},
                {"name": "timestamp", "type": "string"},
                {"name": "nonce",     "type": "uint256"},
                {"name": "message",   "type": "string"}
            ]
        },
        "message": {
            "address":   format!("{:#x}", address),
            "timestamp": timestamp.to_string(),
            "nonce":     nonce,
            "message":   "This message attests that I control the given wallet"
        }
    }))?;

    let sig = wallet
        .sign_typed_data(&td)
        .await
        .map_err(|e| anyhow!("L1 ClobAuth sign failed: {e}"))?;
    Ok(format!("0x{}", hex::encode(sig.to_vec())))
}

// ── L2 HMAC-SHA256 Auth (for authenticated CLOB API calls) ──────────────────

pub fn l2_signature(
    secret: &str,
    timestamp: i64,
    method: &str,
    path: &str,
    body: &str,
) -> Result<String> {
    let msg = format!("{}{}{}{}", timestamp, method.to_uppercase(), path, body);
    let secret_bytes = BASE64.decode(secret).context("decode L2 secret")?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&secret_bytes).map_err(|e| anyhow!("HMAC error: {e}"))?;
    mac.update(msg.as_bytes());
    Ok(BASE64.encode(mac.finalize().into_bytes()))
}

// ── API credential derivation ────────────────────────────────────────────────

async fn get_or_create_api_creds(
    wallet: &LocalWallet,
    address: Address,
    client: &Client,
) -> Result<ApiCredentials> {
    let ts = now_secs();
    let sig = l1_auth_signature(wallet, address, ts, 0).await?;
    let addr = format!("{:#x}", address);

    // Try derive first (reuses existing creds)
    let resp = client
        .get(format!("{CLOB_API}/auth/derive-api-key"))
        .header("POLY_ADDRESS", &addr)
        .header("POLY_SIGNATURE", &sig)
        .header("POLY_TIMESTAMP", ts.to_string())
        .header("POLY_NONCE", "0")
        .send()
        .await?;
    let raw = resp.text().await?;
    let v: Value = serde_json::from_str(&raw)?;

    if let (Some(k), Some(s), Some(p)) = (
        v.get("apiKey").and_then(|v| v.as_str()),
        v.get("secret").and_then(|v| v.as_str()),
        v.get("passphrase").and_then(|v| v.as_str()),
    ) {
        return Ok(ApiCredentials {
            api_key: k.into(),
            secret: s.into(),
            passphrase: p.into(),
        });
    }

    // Fall back to create new
    let ts2 = now_secs();
    let sig2 = l1_auth_signature(wallet, address, ts2, 0).await?;

    let resp2 = client
        .post(format!("{CLOB_API}/auth/api-key"))
        .header("POLY_ADDRESS", &addr)
        .header("POLY_SIGNATURE", &sig2)
        .header("POLY_TIMESTAMP", ts2.to_string())
        .header("POLY_NONCE", "0")
        .send()
        .await?;
    let raw2 = resp2.text().await?;
    let v2: Value = serde_json::from_str(&raw2)?;

    let api_key = v2.get("apiKey").and_then(|v| v.as_str()).ok_or_else(|| {
        anyhow!(
            "Could not get API creds. Server: {raw2}\n\
             If \"Could not create api key\", visit polymarket.com and accept ToS with wallet {addr}."
        )
    })?;
    let secret = v2
        .get("secret")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no secret: {raw2}"))?;
    let passphrase = v2
        .get("passphrase")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no passphrase: {raw2}"))?;

    Ok(ApiCredentials {
        api_key: api_key.into(),
        secret: secret.into(),
        passphrase: passphrase.into(),
    })
}

// ── Proxy wallet lookup ──────────────────────────────────────────────────────

async fn get_proxy_wallet(
    client: &Client,
    address: Address,
    creds: &ApiCredentials,
) -> Result<Address> {
    let ts = now_secs();
    let sig = l2_signature(&creds.secret, ts, "GET", "/proxy-wallet-address", "")?;
    let addr_str = format!("{:#x}", address);

    let resp = client
        .get(format!("{CLOB_API}/proxy-wallet-address"))
        .header("POLY_ADDRESS", &addr_str)
        .header("POLY_SIGNATURE", &sig)
        .header("POLY_TIMESTAMP", ts.to_string())
        .header("POLY_API_KEY", &creds.api_key)
        .header("POLY_PASSPHRASE", &creds.passphrase)
        .send()
        .await
        .context("fetch proxy wallet")?;

    let raw = resp.text().await.context("read proxy wallet response")?;

    // The endpoint may return a JSON object {"address": "0x..."} or a plain quoted string "0x..."
    let proxy_str = if let Ok(body) = serde_json::from_str::<Value>(&raw) {
        if let Some(s) = body.as_str() {
            // Plain JSON string: "0x..."
            s.to_string()
        } else {
            // JSON object: {"address": "0x..."}
            body.get("address")
                .or_else(|| body.get("proxyAddress"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("no proxy wallet in response: {raw}"))?
                .to_string()
        }
    } else {
        // Plain text (unquoted)
        raw.trim().to_string()
    };

    proxy_str
        .parse::<Address>()
        .context(format!("parse proxy wallet address from: {raw}"))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

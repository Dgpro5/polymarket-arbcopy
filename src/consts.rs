// Centralized constants for the copy trading bot.

// ── Polymarket APIs ─────────────────────────────────────────────────────────

pub const CLOB_API: &str = "https://clob.polymarket.com";
pub const DATA_API: &str = "https://data-api.polymarket.com";

// ── Blockchain (Polygon, chain 137) ─────────────────────────────────────────

pub const CHAIN_ID: u64 = 137;
pub const CTF_EXCHANGE_ADDRESS: &str = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E";
pub const CONDITIONAL_TOKENS_ADDRESS: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";
pub const USDC_E_POLYGON: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

// ── Target user ─────────────────────────────────────────────────────────────

pub const TARGET_WALLET: &str = "0x571c285a83eba5322b5f916ba681669dc368a61f";

// ── Copy trading parameters ─────────────────────────────────────────────────

/// How often to poll the data API for new trades (seconds).
pub const POLL_INTERVAL_SECS: u64 = 10;

/// Fraction of the target's USD notional to copy.
/// Example: target spends $27.62 → we spend $27.62 * 0.0724 ≈ $2.00.
pub const COPY_FRACTION: f64 = 0.0724;

/// Minimum USD to spend per copy trade (floor).
pub const MIN_COPY_USD: f64 = 2.0;

/// Maximum fraction of USDC.e balance to deploy in a session.
pub const MAX_BALANCE_FRACTION: f64 = 0.80;

// ── Balance management ──────────────────────────────────────────────────────

/// Minimum USDC.e balance — if below this, swap POL → USDC.e.
pub const MIN_USDC_BALANCE: f64 = 25.0;
/// How much USDC.e to acquire when below threshold.
pub const USDC_TOP_UP_AMOUNT: f64 = 25.0;
/// Minimum POL balance (token count, not dollars) — if below, swap USDC.e → POL.
pub const MIN_POL_BALANCE: f64 = 50.0;
/// How much USDC.e to swap into POL when POL is low.
pub const POL_TOP_UP_USDC: f64 = 10.0;
/// How often to run balance checks (seconds).
pub const BALANCE_CHECK_INTERVAL_SECS: u64 = 300;

// ── Environment variables ───────────────────────────────────────────────────

pub const ANKR_API_KEY_ENV: &str = "ANKR_API_KEY";

// ── Redemption & trade history ──────────────────────────────────────────────

/// Wait this long after a trade before checking outcome / redeeming (seconds).
pub const REDEMPTION_DELAY_SECS: u64 = 900; // 15 minutes
/// How often the redemption loop checks for ripe entries (seconds).
pub const REDEMPTION_POLL_INTERVAL_SECS: u64 = 60;
/// JSON file for the redemption queue.
pub const PENDING_REDEMPTIONS_FILE: &str = "data/pending_redemptions.json";
/// JSON file for completed trade history (WIN/LOSS log).
pub const TRADE_HISTORY_FILE: &str = "data/trade_history.json";

// ── Discord webhooks ────────────────────────────────────────────────────────

pub const DISCORD_WEBHOOK_URL: &str =
    "https://discord.com/api/webhooks/1473284259363164211/4sgTuuoGlwS4OyJ5x6-QmpPA_Q1gvsIZB9EZrb9zWX6qyA0LMQklz3IupBfINPVnpsMZ";
pub const ERROR_DISCORD_WEBHOOK_URL: &str =
    "https://discord.com/api/webhooks/1475092817654055084/_mr0tTCdzyyoJtTBwNqE6KYj6SQ0XEegZFv4j5PejJ0vq2i1Vlt0oi7IFmeAt12j0TQW";

// ── Data storage ────────────────────────────────────────────────────────────

pub const DATA_DIR: &str = "data";

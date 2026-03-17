// Centralized constants for the trade tracker.

// ── Polymarket APIs ─────────────────────────────────────────────────────────

pub const DATA_API: &str = "https://data-api.polymarket.com";

// ── Target user ─────────────────────────────────────────────────────────────

pub const TARGET_WALLET: &str = "0x571c285a83eba5322b5f916ba681669dc368a61f";

// ── Polling ─────────────────────────────────────────────────────────────────

/// How often to poll the data API for new trades (milliseconds).
pub const POLL_INTERVAL_MS: u64 = 300;

// ── Discord webhooks ────────────────────────────────────────────────────────

pub const DISCORD_WEBHOOK_URL: &str =
    "https://discord.com/api/webhooks/1473284259363164211/4sgTuuoGlwS4OyJ5x6-QmpPA_Q1gvsIZB9EZrb9zWX6qyA0LMQklz3IupBfINPVnpsMZ";
pub const ERROR_DISCORD_WEBHOOK_URL: &str =
    "https://discord.com/api/webhooks/1475092817654055084/_mr0tTCdzyyoJtTBwNqE6KYj6SQ0XEegZFv4j5PejJ0vq2i1Vlt0oi7IFmeAt12j0TQW";

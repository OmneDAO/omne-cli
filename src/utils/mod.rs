//! Utility functions for the Omne CLI

#![allow(dead_code)] // Allow unused utility functions for future development

use crate::config::Config;
use anyhow::Result;
use console::{Emoji, Style};
use dialoguer::Confirm;
use hex;
use indicatif::{ProgressBar, ProgressStyle};
use rand::{rngs::OsRng, RngCore};
use reqwest::{header::AUTHORIZATION, Client, RequestBuilder};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::warn;

/// Create a spinner with a message
pub fn spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.enable_steady_tick(Duration::from_millis(120));
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("{spinner:.blue} {msg}")
            .unwrap(),
    );
    pb.set_message(message.to_string());
    pb
}

/// Create a progress bar
pub fn progress_bar(total: u64, message: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );
    pb.set_message(message.to_string());
    pb
}

/// Prompt user for confirmation
pub fn confirm(message: &str) -> Result<bool> {
    let result = Confirm::new()
        .with_prompt(message)
        .default(false)
        .interact()?;
    Ok(result)
}

/// Format file sizes in human-readable format
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{:.2} {}", size, UNITS[unit_index])
    }
}

/// Format duration in human-readable format
pub fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, secs)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, secs)
    } else {
        format!("{}s", secs)
    }
}

/// Format currency amounts
pub fn format_currency(amount: f64, currency: &str) -> String {
    match currency.to_uppercase().as_str() {
        "USD" => format!("${:.2}", amount),
        "OGT" | "OMC" => format!("{:.4} {}", amount, currency),
        _ => format!("{:.8} {}", amount, currency),
    }
}

/// Color styles for console output
pub struct Styles {
    pub success: Style,
    pub error: Style,
    pub warning: Style,
    pub info: Style,
    pub highlight: Style,
}

impl Default for Styles {
    fn default() -> Self {
        Self {
            success: Style::new().green().bold(),
            error: Style::new().red().bold(),
            warning: Style::new().yellow().bold(),
            info: Style::new().blue(),
            highlight: Style::new().cyan().bold(),
        }
    }
}

/// Emojis for console output
pub struct Emojis;

impl Emojis {
    pub const ROCKET: Emoji<'_, '_> = Emoji("🚀 ", "");
    pub const CHECK: Emoji<'_, '_> = Emoji("✅ ", "✓ ");
    pub const ERROR: Emoji<'_, '_> = Emoji("❌ ", "✗ ");
    pub const WARNING: Emoji<'_, '_> = Emoji("⚠️ ", "! ");
    pub const INFO: Emoji<'_, '_> = Emoji("ℹ️ ", "i ");
    pub const MONEY: Emoji<'_, '_> = Emoji("💰 ", "$");
    pub const COMPUTER: Emoji<'_, '_> = Emoji("💻 ", "");
    pub const GEAR: Emoji<'_, '_> = Emoji("⚙️ ", "");
    pub const CHART: Emoji<'_, '_> = Emoji("📊 ", "");
    pub const FIRE: Emoji<'_, '_> = Emoji("🔥 ", "");
}

/// Validate network names
pub fn validate_network(network: &str) -> Result<()> {
    match network {
        "mainnet" | "testnet" | "devnet" => Ok(()),
        _ => Err(anyhow::anyhow!(
            "Invalid network '{}'. Valid options: mainnet, testnet, devnet",
            network
        )),
    }
}

/// Validate service names
pub fn validate_service(service: &str) -> Result<()> {
    match service {
        "omp" | "orc20" | "paymaster" => Ok(()),
        _ => Err(anyhow::anyhow!(
            "Invalid service '{}'. Valid options: omp, orc20, paymaster",
            service
        )),
    }
}

/// Parse duration string (e.g., "1h30m", "45s", "2d")
pub fn parse_duration(duration_str: &str) -> Result<Duration> {
    // Simple duration parsing - in a real implementation this would be more robust
    let duration_str = duration_str.to_lowercase();

    if let Some(hours) = duration_str.strip_suffix('h') {
        let hours: u64 = hours.parse()?;
        return Ok(Duration::from_secs(hours * 3600));
    }

    if let Some(minutes) = duration_str.strip_suffix('m') {
        let minutes: u64 = minutes.parse()?;
        return Ok(Duration::from_secs(minutes * 60));
    }

    if let Some(seconds) = duration_str.strip_suffix('s') {
        let seconds: u64 = seconds.parse()?;
        return Ok(Duration::from_secs(seconds));
    }

    // Default to seconds if no suffix
    let seconds: u64 = duration_str.parse()?;
    Ok(Duration::from_secs(seconds))
}

static RPC_TOKEN_CACHE: OnceLock<Option<String>> = OnceLock::new();
static RPC_WARN_ONCE: OnceLock<()> = OnceLock::new();

/// Cache the RPC token derived from the active configuration so subsequent
/// requests can reuse it without re-reading environment variables.
pub fn prime_rpc_auth(config: &Config) {
    let _ = RPC_TOKEN_CACHE.set(resolve_rpc_token(config));
    if RPC_TOKEN_CACHE
        .get()
        .and_then(|token| token.as_ref())
        .is_none()
    {
        warn_missing_rpc_token();
    }
}

/// Build a POST request with Omne RPC headers (Authorization + nonce).
pub fn rpc_post(client: &Client, endpoint: &str, config: &Config) -> RequestBuilder {
    apply_rpc_headers(client.post(endpoint), config)
}

/// Build a GET request with Omne RPC headers (Authorization + nonce).
pub fn rpc_get(client: &Client, endpoint: &str, config: &Config) -> RequestBuilder {
    apply_rpc_headers(client.get(endpoint), config)
}

/// Attach RPC guardrail headers to an existing request builder.
pub fn apply_rpc_headers(builder: RequestBuilder, config: &Config) -> RequestBuilder {
    let builder = builder.header("X-Omne-Nonce", generate_rpc_nonce());

    if let Some(token) = cached_rpc_token(config) {
        builder.header(AUTHORIZATION, token)
    } else {
        warn_missing_rpc_token();
        builder
    }
}

fn cached_rpc_token(config: &Config) -> Option<String> {
    if let Some(token) = RPC_TOKEN_CACHE.get() {
        return token.clone();
    }
    resolve_rpc_token(config)
}

fn resolve_rpc_token(config: &Config) -> Option<String> {
    rpc_token_from_env()
        .or_else(|| {
            config
                .network
                .auth_token
                .as_deref()
                .and_then(normalise_bearer_token)
        })
        .or_else(|| {
            std::env::var("OMNE_AUTH_TOKEN")
                .ok()
                .and_then(|value| normalise_bearer_token(value.trim()))
        })
}

fn rpc_token_from_env() -> Option<String> {
    std::env::var("OMNE_RPC_TOKEN")
        .ok()
        .and_then(|value| normalise_bearer_token(value.trim()))
}

fn normalise_bearer_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    const PREFIX: &str = "bearer";
    if trimmed.eq_ignore_ascii_case(PREFIX) {
        return None;
    }

    if trimmed.len() > PREFIX.len() {
        let (prefix, remainder) = trimmed.split_at(PREFIX.len());
        if prefix.eq_ignore_ascii_case(PREFIX)
            && remainder
                .chars()
                .next()
                .map(|ch| ch.is_whitespace())
                .unwrap_or(false)
        {
            let token = remainder.trim();
            if token.is_empty() {
                return None;
            }
            return Some(format!("Bearer {}", token));
        }
    }

    Some(format!("Bearer {}", trimmed))
}

fn generate_rpc_nonce() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn warn_missing_rpc_token() {
    if RPC_WARN_ONCE.set(()).is_ok() {
        warn!(
            "OMNE_RPC_TOKEN or network.auth_token not configured; RPC requests will likely be rejected once guardrails are enforced"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(2_500_000_000), "2.33 GB");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3661), "1h 1m 1s");
    }

    #[test]
    fn test_validate_network() {
        assert!(validate_network("mainnet").is_ok());
        assert!(validate_network("testnet").is_ok());
        assert!(validate_network("devnet").is_ok());
        assert!(validate_network("invalid").is_err());
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
    }
}

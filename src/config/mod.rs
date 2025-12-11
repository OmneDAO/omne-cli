//! Configuration management for Omne CLI

use anyhow::{Context, Result};
use deploy_guardrails::{compiler_signers_vec_for_network, signers_vec_for_network};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::fs;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub network: NetworkConfig,
    pub node: NodeConfig,
    pub validator: ValidatorConfig,
    pub infrastructure: InfrastructureConfig,
    pub development: DevelopmentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub name: String,
    pub chain_id: u64,
    pub rpc_endpoint: String,
    pub ws_endpoint: String,
    pub explorer_url: String,
    #[serde(default)]
    pub allowed_services: Vec<String>,
    #[serde(default)]
    pub allowed_signers: Vec<String>,
    #[serde(default)]
    pub allowed_compiler_signers: Vec<String>,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub rate_limit_per_minute: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub data_dir: PathBuf,
    pub log_level: String,
    pub p2p_port: u16,
    pub rpc_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorConfig {
    pub enabled: bool,
    pub stake_amount: Option<u64>,
    pub auto_optimize: bool,
    pub earnings_tracking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfrastructureConfig {
    pub omp: OmpConfig,
    pub orc20: Orc20Config,
    pub paymaster: PaymasterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmpConfig {
    pub enabled: bool,
    pub storage_quota_gb: u32,
    pub price_per_mb_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Orc20Config {
    pub enabled: bool,
    pub gas_price_multiplier: f64,
    pub max_sponsored_gas: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymasterConfig {
    pub enabled: bool,
    pub monthly_budget_usd: u32,
    pub min_reputation_score: f64,
    pub max_operations_per_hour: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevelopmentConfig {
    pub local_network_validators: u32,
    pub auto_start_services: bool,
    pub sdk_versions: SdkVersions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkVersions {
    pub python: String,
    pub typescript: String,
    pub go: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            network: NetworkConfig {
                name: "testnet".to_string(),
                chain_id: 1338,
                rpc_endpoint: "https://testnet-rpc.omne.network".to_string(),
                ws_endpoint: "wss://testnet-ws.omne.network".to_string(),
                explorer_url: "https://testnet-explorer.omne.network".to_string(),
                allowed_services: vec!["orchestrator".to_string(), "analytics".to_string()],
                allowed_signers: signers_vec_for_network("testnet"),
                allowed_compiler_signers: compiler_signers_vec_for_network("testnet"),
                auth_token: None,
                rate_limit_per_minute: Some(60),
            },
            node: NodeConfig {
                data_dir: dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".omne"),
                log_level: "info".to_string(),
                p2p_port: 30303,
                rpc_port: 8545,
            },
            validator: ValidatorConfig {
                enabled: false,
                stake_amount: None,
                auto_optimize: true,
                earnings_tracking: true,
            },
            infrastructure: InfrastructureConfig {
                omp: OmpConfig {
                    enabled: true,
                    storage_quota_gb: 100,
                    price_per_mb_usd: 0.01,
                },
                orc20: Orc20Config {
                    enabled: true,
                    gas_price_multiplier: 1.2,
                    max_sponsored_gas: 100000,
                },
                paymaster: PaymasterConfig {
                    enabled: true,
                    monthly_budget_usd: 1000,
                    min_reputation_score: 0.7,
                    max_operations_per_hour: 1000,
                },
            },
            development: DevelopmentConfig {
                local_network_validators: 3,
                auto_start_services: true,
                sdk_versions: SdkVersions {
                    python: "latest".to_string(),
                    typescript: "latest".to_string(),
                    go: "latest".to_string(),
                },
            },
        }
    }
}

pub async fn load_config(config_path: Option<&str>, network: &str) -> Result<Config> {
    let mut config = Config::default();

    // Apply network preset before loading any custom configuration so overrides can mutate it.
    apply_network_preset(&mut config, network);

    if let Some(path) = config_path {
        if let Some(custom_value) = load_config_value(path).await? {
            let mut base_value = serde_json::to_value(&config)
                .context("failed to serialise default configuration")?;
            merge_values(&mut base_value, custom_value, path);

            config = serde_json::from_value(base_value)
                .context("failed to apply custom configuration overrides")?;

            info!("Configuration merged from {}", path);
        }
    }

    if let Err(err) = validate_network_endpoints(&config).await {
        warn!(
            "Configuration RPC endpoint validation failed for {}: {}",
            config.network.rpc_endpoint, err
        );
    }

    info!("Configuration loaded for {} network", config.network.name);
    Ok(config)
}

/// Get the configuration directory for OMNE CLI
#[allow(dead_code)]
pub fn get_config_dir() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("omne-cli");

    std::fs::create_dir_all(&config_dir)?;
    Ok(config_dir)
}

#[allow(dead_code)]
pub fn get_data_dir() -> Result<PathBuf> {
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("omne-cli");

    std::fs::create_dir_all(&data_dir)?;
    Ok(data_dir)
}

fn apply_network_preset(config: &mut Config, network: &str) {
    match network {
        "mainnet" => {
            config.network.name = "mainnet".to_string();
            config.network.chain_id = 1337;
            config.network.rpc_endpoint = "https://rpc.omne.network".to_string();
            config.network.ws_endpoint = "wss://ws.omne.network".to_string();
            config.network.explorer_url = "https://explorer.omne.network".to_string();
            config.network.allowed_services = vec![
                "orchestrator".to_string(),
                "analytics".to_string(),
                "security".to_string(),
            ];
            config.network.allowed_signers = signers_vec_for_network("mainnet");
            config.network.allowed_compiler_signers = compiler_signers_vec_for_network("mainnet");
            config.network.rate_limit_per_minute = Some(120);
        }
        "devnet" => {
            config.network.name = "devnet".to_string();
            config.network.chain_id = 1339;
            config.network.rpc_endpoint = "http://localhost:8545".to_string();
            config.network.ws_endpoint = "ws://localhost:8546".to_string();
            config.network.explorer_url = "http://localhost:3000".to_string();
            config.network.allowed_services = vec!["orchestrator".to_string()];
            config.network.allowed_signers = signers_vec_for_network("devnet");
            config.network.allowed_compiler_signers = compiler_signers_vec_for_network("devnet");
            config.network.rate_limit_per_minute = None;
        }
        _ => {
            // Default testnet configuration is already set
        }
    }
}

async fn load_config_value(path: &str) -> Result<Option<JsonValue>> {
    let content = match fs::read_to_string(path).await {
        Ok(data) => data,
        Err(err) => {
            warn!("Failed to read config file {}: {}", path, err);
            return Ok(None);
        }
    };

    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let value = match extension.as_str() {
        "json" => {
            serde_json::from_str::<JsonValue>(&content).context("invalid JSON configuration")?
        }
        "yaml" | "yml" => {
            let yaml: serde_yaml::Value =
                serde_yaml::from_str(&content).context("invalid YAML configuration")?;
            serde_json::to_value(yaml).context("failed to convert YAML to JSON value")?
        }
        "toml" | "" => {
            let toml_value: toml::Value =
                toml::from_str(&content).context("invalid TOML configuration")?;
            serde_json::to_value(toml_value).context("failed to convert TOML to JSON value")?
        }
        other => {
            warn!(
                "Unsupported config extension '{}' for {}, expected toml/yaml/json",
                other, path
            );
            return Ok(None);
        }
    };

    Ok(Some(value))
}

fn merge_values(base: &mut JsonValue, patch: JsonValue, origin: &str) {
    match (base, patch) {
        (JsonValue::Object(base_map), JsonValue::Object(patch_map)) => {
            report_unknown_keys(base_map, &patch_map, origin);
            for (key, value) in patch_map {
                merge_values(
                    base_map.entry(key).or_insert(JsonValue::Null),
                    value,
                    origin,
                );
            }
        }
        (base_slot, patch_value) => {
            *base_slot = patch_value;
        }
    }
}

fn report_unknown_keys(
    base: &JsonMap<String, JsonValue>,
    patch: &JsonMap<String, JsonValue>,
    origin: &str,
) {
    for key in patch.keys() {
        if !base.contains_key(key) {
            warn!("Ignoring unknown configuration key '{}' in {}", key, origin);
        }
    }
}

async fn validate_network_endpoints(config: &Config) -> Result<()> {
    probe_rpc_endpoint(&config.network.rpc_endpoint).await
}

/// Probe the configured RPC endpoint and return an error if it is unreachable.
pub async fn probe_rpc_endpoint(endpoint: &str) -> Result<()> {
    if !endpoint.starts_with("http") {
        return Ok(());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .context("unable to build HTTP client for validation")?;

    let response = client
        .get(endpoint)
        .send()
        .await
        .with_context(|| format!("failed to reach RPC endpoint {}", endpoint))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "RPC endpoint {} responded with status {}",
            endpoint,
            response.status()
        );
    }

    Ok(())
}

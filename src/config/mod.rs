//! Configuration management for Omne CLI

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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

    // Update network configuration based on network parameter
    match network {
        "mainnet" => {
            config.network.name = "mainnet".to_string();
            config.network.chain_id = 1337;
            config.network.rpc_endpoint = "https://rpc.omne.network".to_string();
            config.network.ws_endpoint = "wss://ws.omne.network".to_string();
            config.network.explorer_url = "https://explorer.omne.network".to_string();
        }
        "devnet" => {
            config.network.name = "devnet".to_string();
            config.network.chain_id = 1339;
            config.network.rpc_endpoint = "http://localhost:8545".to_string();
            config.network.ws_endpoint = "ws://localhost:8546".to_string();
            config.network.explorer_url = "http://localhost:3000".to_string();
        }
        _ => {
            // Default testnet configuration is already set
        }
    }

    if let Some(path) = config_path {
        info!("Loading configuration from: {}", path);

        // In a real implementation, we would load and merge the configuration file
        // For now, we'll just log that we would load it
        warn!("Custom configuration loading not implemented yet");
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

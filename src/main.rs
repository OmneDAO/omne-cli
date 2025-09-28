//! Omne CLI - Unified Blockchain Ecosystem Orchestration Tool
//!
//! The Omne CLI provides a single, powerful interface for managing the entire
//! Omne blockchain ecosystem including network operations, validator management,
//! infrastructure services, and developer tools.

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use std::io;
use tracing::{error, info};

mod commands;
mod config;
mod utils;

use commands::{
    dev::DevCommands, infrastructure::InfrastructureCommands, network::NetworkCommands,
    ops::OpsCommands, validator::ValidatorCommands,
};

/// Omne CLI - Unified blockchain ecosystem orchestration tool
#[derive(Parser)]
#[command(
    name = "omne",
    about = "Unified command-line orchestration tool for the Omne blockchain ecosystem",
    long_about = "The Omne CLI provides comprehensive management for the Omne blockchain ecosystem, \
                  including network operations, validator coordination, infrastructure services, \
                  developer tools, and operational monitoring.",
    version = env!("CARGO_PKG_VERSION"),
    author = "Omne Network <dev@omne.network>"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Configuration file path
    #[arg(short, long, global = true)]
    config: Option<String>,

    /// Network environment (mainnet, testnet, devnet)
    #[arg(long, global = true, default_value = "testnet")]
    network: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Network-level operations and management
    #[command(name = "network", alias = "net")]
    Network {
        #[command(subcommand)]
        command: NetworkCommands,
    },

    /// Validator operations and service management
    #[command(name = "validator", alias = "val")]
    Validator {
        #[command(subcommand)]
        command: ValidatorCommands,
    },

    /// Developer tools and project management
    #[command(name = "dev", alias = "develop")]
    Dev {
        #[command(subcommand)]
        command: DevCommands,
    },

    /// Infrastructure service management
    #[command(name = "infrastructure", alias = "infra")]
    Infrastructure {
        #[command(subcommand)]
        command: InfrastructureCommands,
    },

    /// Operations, monitoring, and maintenance
    #[command(name = "ops", alias = "operations")]
    Ops {
        #[command(subcommand)]
        command: OpsCommands,
    },

    /// Generate shell completions
    #[command(name = "completion")]
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        "debug,reqwest=info,hyper=info"
    } else {
        "info,reqwest=warn,hyper=warn"
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    info!("🚀 Omne CLI v{} starting...", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config = config::load_config(cli.config.as_deref(), &cli.network).await?;

    // Execute command
    let result = match cli.command {
        Commands::Network { command } => commands::network::execute(command, &config).await,
        Commands::Validator { command } => commands::validator::execute(command, &config).await,
        Commands::Dev { command } => commands::dev::execute(command, &config).await,
        Commands::Infrastructure { command } => {
            commands::infrastructure::execute(command, &config).await
        }
        Commands::Ops { command } => commands::ops::execute(command, &config).await,
        Commands::Completion { shell } => {
            let mut app = Cli::command();
            generate(shell, &mut app, "omne", &mut io::stdout());
            Ok(())
        }
    };

    match result {
        Ok(_) => {
            info!("✅ Command completed successfully");
            Ok(())
        }
        Err(e) => {
            error!("❌ Command failed: {}", e);
            std::process::exit(1);
        }
    }
}

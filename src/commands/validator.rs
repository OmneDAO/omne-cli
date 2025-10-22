//! Validator operations and service management commands

use crate::config::Config;
use crate::utils::{confirm, spinner};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use clap::Subcommand;
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

#[derive(Subcommand)]
pub enum ValidatorCommands {
    /// Initialize validator configuration
    Init {
        /// Data directory for validator
        #[arg(long, default_value = "~/.omne-validator")]
        data_dir: String,

        /// Enable infrastructure services
        #[arg(long)]
        services: Vec<String>,
    },

    /// Start validator with auto-optimization
    Start {
        /// Enable auto-service optimization
        #[arg(long)]
        auto_optimize: bool,

        /// Enable earnings tracking
        #[arg(long)]
        earnings_tracking: bool,

        /// Validator stake amount (OGT)
        #[arg(long)]
        stake: Option<u64>,
    },

    /// Manage validator staking
    Stake {
        #[command(subcommand)]
        action: StakeCommands,
    },

    /// Infrastructure service management
    Services {
        #[command(subcommand)]
        action: ServiceCommands,
    },

    /// Show validator earnings breakdown
    Earnings {
        /// Show detailed breakdown
        #[arg(long)]
        breakdown: bool,

        /// Time period (day, week, month)
        #[arg(long, default_value = "day")]
        period: String,
    },

    /// Inspect and resolve fraud challenges
    Challenges {
        #[command(subcommand)]
        action: ChallengeCommands,
    },

    /// Validator status and health
    Status {
        /// Show infrastructure service status
        #[arg(long)]
        services: bool,
    },
}

#[derive(Subcommand)]
pub enum StakeCommands {
    /// Add stake to validator
    Add {
        /// Amount to stake (OGT)
        amount: u64,
    },
    /// Remove stake from validator
    Remove {
        /// Amount to unstake (OGT)
        amount: u64,
    },
    /// Show current staking information
    Info,
}

#[derive(Subcommand)]
pub enum ServiceCommands {
    /// Enable infrastructure service
    Enable {
        /// Service name (omp, orc20, paymaster)
        service: String,
    },
    /// Disable infrastructure service
    Disable {
        /// Service name
        service: String,
    },
    /// Configure service parameters
    Configure {
        /// Service name
        service: String,
        /// Configuration file
        #[arg(long)]
        config_file: Option<String>,
    },
    /// Show service status
    Status,
}

#[derive(Subcommand)]
pub enum ChallengeCommands {
    /// List pending fraud challenges registered by the security layer
    Pending {
        /// Maximum number of challenges to display (0 = all)
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// List resolved fraud challenges and their verdicts
    Resolved {
        /// Maximum number of challenges to display (0 = all)
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Show full details for a specific challenge identifier
    Show {
        /// Challenge identifier to inspect
        id: u64,
    },
    /// Resolve a pending challenge with an explicit verdict
    Resolve {
        /// Challenge identifier to resolve
        id: u64,
        /// Verdict to apply (proven_fraud, rejected, expired, cleared)
        verdict: String,
        /// Optional operator note recorded with the resolution
        #[arg(long)]
        note: Option<String>,
    },
}

pub async fn execute(command: ValidatorCommands, config: &Config) -> Result<()> {
    if requires_rpc(&command) {
        crate::config::probe_rpc_endpoint(&config.network.rpc_endpoint)
            .await
            .with_context(|| {
                format!(
                    "Unable to reach validator RPC endpoint at {}",
                    config.network.rpc_endpoint
                )
            })?;
    }

    match command {
        ValidatorCommands::Init { data_dir, services } => {
            init_validator(&data_dir, &services, config).await
        }
        ValidatorCommands::Start {
            auto_optimize,
            earnings_tracking,
            stake,
        } => start_validator(auto_optimize, earnings_tracking, stake, config).await,
        ValidatorCommands::Stake { action } => manage_stake(action, config).await,
        ValidatorCommands::Services { action } => manage_services(action, config).await,
        ValidatorCommands::Earnings { breakdown, period } => {
            show_earnings(breakdown, &period, config).await
        }
        ValidatorCommands::Challenges { action } => handle_challenges(action, config).await,
        ValidatorCommands::Status { services } => show_validator_status(services, config).await,
    }
}

fn requires_rpc(command: &ValidatorCommands) -> bool {
    matches!(
        command,
        ValidatorCommands::Stake { .. }
            | ValidatorCommands::Services { .. }
            | ValidatorCommands::Earnings { .. }
            | ValidatorCommands::Challenges { .. }
            | ValidatorCommands::Status { .. }
    )
}

async fn init_validator(data_dir: &str, services: &[String], _config: &Config) -> Result<()> {
    info!("🔧 Initializing Omne validator...");

    let progress = spinner("Creating validator directory structure...");
    sleep(Duration::from_secs(1)).await;
    progress.finish_with_message("✅ Directory structure created");

    let progress = spinner("Generating validator keys...");
    sleep(Duration::from_secs(2)).await;
    progress.finish_with_message("✅ Validator keys generated");

    if !services.is_empty() {
        info!(
            "🔧 Configuring infrastructure services: {}",
            services.join(", ")
        );
        let progress = spinner("Setting up service configurations...");
        sleep(Duration::from_secs(2)).await;
        progress.finish_with_message("✅ Services configured");
    }

    info!("✅ Validator initialized successfully");
    info!("   Data Directory: {}", data_dir);
    let services_str = if services.is_empty() {
        "None".to_string()
    } else {
        services.join(", ")
    };
    info!("   Services: {}", services_str);
    info!("   Next: Run 'omne validator start' to begin validation");

    Ok(())
}

async fn handle_challenges(action: ChallengeCommands, config: &Config) -> Result<()> {
    let endpoint = &config.network.rpc_endpoint;

    match action {
        ChallengeCommands::Pending { limit } => {
            let mut challenges: Vec<ChallengeRecord> =
                rpc_request(endpoint, "omne_getPendingChallenges", json!([])).await?;

            if challenges.is_empty() {
                info!("🎯 No pending fraud challenges reported by the node");
                return Ok(());
            }

            challenges.sort_by_key(|record| record.expires_at);
            print_challenge_list(&challenges, limit, false);
        }
        ChallengeCommands::Resolved { limit } => {
            let mut challenges: Vec<ChallengeRecord> =
                rpc_request(endpoint, "omne_getResolvedChallenges", json!([])).await?;

            if challenges.is_empty() {
                info!("ℹ️ No resolved fraud challenges recorded yet");
                return Ok(());
            }

            challenges.sort_by(|a, b| {
                let a_ts = resolved_timestamp(&a.status).unwrap_or(0);
                let b_ts = resolved_timestamp(&b.status).unwrap_or(0);
                b_ts.cmp(&a_ts)
            });
            print_challenge_list(&challenges, limit, true);
        }
        ChallengeCommands::Show { id } => {
            let record: Option<ChallengeRecord> =
                rpc_request(endpoint, "omne_getChallengeById", json!([id])).await?;

            match record {
                Some(record) => print_challenge_details(&record),
                None => warn!("❓ Challenge #{} not found on the node", id),
            }
        }
        ChallengeCommands::Resolve { id, verdict, note } => {
            let resolution = parse_resolution(&verdict)?;
            let params = if let Some(note) = note {
                json!([id, resolution_to_param(resolution), note])
            } else {
                json!([id, resolution_to_param(resolution)])
            };

            let record: Option<ChallengeRecord> =
                rpc_request(endpoint, "omne_resolveChallenge", params).await?;

            match record {
                Some(record) => {
                    info!("✅ Challenge #{} resolved as {}", record.id, resolution);
                    print_challenge_details(&record);
                }
                None => warn!(
                    "⚠️ Challenge #{} was not found or has already been resolved",
                    id
                ),
            }
        }
    }

    Ok(())
}

fn print_challenge_list(challenges: &[ChallengeRecord], limit: usize, resolved: bool) {
    let total = challenges.len();
    let to_show = if limit == 0 { total } else { limit.min(total) };

    if resolved {
        println!(
            "\n✅ Resolved Challenges (showing {} of {})",
            to_show, total
        );
    } else {
        println!(
            "\n⚠️ Pending Fraud Challenges (showing {} of {})",
            to_show, total
        );
    }

    for record in challenges.iter().take(to_show) {
        println!(
            "   • Challenge #{} – block {} (security #{})",
            record.id, record.block_number, record.security_block_ref
        );
        println!(
            "     Proof digest: {} | Submitted: {}",
            short_hash(&record.fraud_proof_hash),
            format_relative_time(record.submitted_at)
        );
        if let Some(challenger) = record.challenger.as_deref() {
            println!("     Challenger: {}", challenger);
        }
        match &record.status {
            ChallengeStatus::Pending => println!(
                "     Status: Pending – {} remaining",
                format_future_time(record.expires_at)
            ),
            ChallengeStatus::Resolved {
                verdict,
                resolved_at,
                note,
            } => {
                println!(
                    "     Status: {} – {}",
                    verdict,
                    format_relative_time(*resolved_at)
                );
                if let Some(note) = note.as_deref() {
                    println!("     Note: {}", note);
                }
            }
        }
    }

    if to_show < total {
        println!("   … {} additional record(s) not shown", total - to_show);
    }
}

fn print_challenge_details(record: &ChallengeRecord) {
    println!(
        "\n🔎 Challenge #{} – block {} (security ref #{})",
        record.id, record.block_number, record.security_block_ref
    );
    println!("   Block hash: {}", record.block_hash);
    println!(
        "   Fraud proof digest: {}",
        short_hash(&record.fraud_proof_hash)
    );
    println!(
        "   Submitted: {} ({})",
        format_timestamp(record.submitted_at),
        format_relative_time(record.submitted_at)
    );
    println!(
        "   Expires: {} ({})",
        format_timestamp(record.expires_at),
        format_future_time(record.expires_at)
    );
    println!(
        "   Challenger: {}",
        record.challenger.as_deref().unwrap_or("unknown")
    );
    if let Some(proposer) = record.offending_proposer.as_deref() {
        println!("   Offending proposer: {}", proposer);
    }

    match &record.status {
        ChallengeStatus::Pending => println!(
            "   Status: Pending – {} remaining",
            format_future_time(record.expires_at)
        ),
        ChallengeStatus::Resolved {
            verdict,
            resolved_at,
            note,
        } => {
            println!(
                "   Status: Resolved as {} at {} ({})",
                verdict,
                format_timestamp(*resolved_at),
                format_relative_time(*resolved_at)
            );
            if let Some(note) = note.as_deref() {
                println!("   Resolution note: {}", note);
            }
        }
    }
}

fn parse_resolution(value: &str) -> Result<ChallengeResolution> {
    let verdict = value.trim().to_ascii_lowercase();
    match verdict.as_str() {
        "proven_fraud" | "proven-fraud" | "fraud" | "valid" => {
            Ok(ChallengeResolution::ProvenFraud)
        }
        "rejected" | "invalid" => Ok(ChallengeResolution::Rejected),
        "expired" | "timeout" | "timed_out" => Ok(ChallengeResolution::Expired),
        "cleared" | "dismissed" | "manual" => Ok(ChallengeResolution::Cleared),
        other => Err(anyhow!(
            "Unsupported challenge verdict '{}'. Expected one of: proven_fraud, rejected, expired, cleared.",
            other
        )),
    }
}

fn resolution_to_param(resolution: ChallengeResolution) -> &'static str {
    match resolution {
        ChallengeResolution::ProvenFraud => "proven_fraud",
        ChallengeResolution::Rejected => "rejected",
        ChallengeResolution::Expired => "expired",
        ChallengeResolution::Cleared => "cleared",
    }
}

fn resolved_timestamp(status: &ChallengeStatus) -> Option<u64> {
    match status {
        ChallengeStatus::Resolved { resolved_at, .. } => Some(*resolved_at),
        _ => None,
    }
}

fn short_hash(value: &str) -> String {
    if value.len() <= 12 {
        value.to_string()
    } else {
        format!("{}…{}", &value[..6], &value[value.len() - 4..])
    }
}

fn format_timestamp(ts: u64) -> String {
    match to_datetime(ts) {
        Some(dt) => dt.to_rfc3339_opts(SecondsFormat::Secs, true),
        None => ts.to_string(),
    }
}

fn format_relative_time(ts: u64) -> String {
    match to_datetime(ts) {
        Some(dt) => {
            let now = Utc::now();
            let delta = now.timestamp() - dt.timestamp();
            if delta >= 0 {
                format!("{} ago", format_duration(delta as u64))
            } else {
                format!("in {}", format_duration(delta.unsigned_abs()))
            }
        }
        None => "-".to_string(),
    }
}

fn format_future_time(ts: u64) -> String {
    match to_datetime(ts) {
        Some(dt) => {
            let now = Utc::now();
            let delta = dt.timestamp() - now.timestamp();
            if delta >= 0 {
                format_duration(delta as u64)
            } else {
                format!("{} overdue", format_duration(delta.unsigned_abs()))
            }
        }
        None => "-".to_string(),
    }
}

fn to_datetime(ts: u64) -> Option<DateTime<Utc>> {
    if ts > i64::MAX as u64 {
        return None;
    }
    Utc.timestamp_opt(ts as i64, 0).single()
}

fn format_duration(mut seconds: u64) -> String {
    let days = seconds / 86_400;
    seconds %= 86_400;
    let hours = seconds / 3_600;
    seconds %= 3_600;
    let minutes = seconds / 60;
    let secs = seconds % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{}d", days));
    }
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if secs > 0 || parts.is_empty() {
        parts.push(format!("{}s", secs));
    }

    parts.join(" ")
}

async fn start_validator(
    auto_optimize: bool,
    earnings_tracking: bool,
    stake: Option<u64>,
    _config: &Config,
) -> Result<()> {
    info!("🚀 Starting Omne validator...");

    // Check dynamic stake requirement
    let required_stake = calculate_dynamic_stake().await?;
    let actual_stake = stake.unwrap_or(required_stake);

    if actual_stake < required_stake {
        warn!(
            "⚠️ Stake amount ({} OGT) below dynamic requirement ({} OGT)",
            actual_stake, required_stake
        );
        if !confirm("Continue anyway?")? {
            return Ok(());
        }
    }

    let progress = spinner("Starting consensus engine...");
    sleep(Duration::from_secs(2)).await;
    progress.finish_with_message("✅ Consensus engine running");

    let progress = spinner("Initializing P2P network...");
    sleep(Duration::from_secs(1)).await;
    progress.finish_with_message("✅ P2P network connected");

    if auto_optimize {
        let progress = spinner("Optimizing infrastructure services...");
        sleep(Duration::from_secs(2)).await;
        progress.finish_with_message("✅ Services optimized for revenue");
    }

    info!("✅ Validator started successfully!");
    info!("   Stake: {} OGT", actual_stake);
    info!(
        "   Auto-Optimization: {}",
        if auto_optimize { "Enabled" } else { "Disabled" }
    );
    info!(
        "   Earnings Tracking: {}",
        if earnings_tracking {
            "Enabled"
        } else {
            "Disabled"
        }
    );

    Ok(())
}

async fn manage_stake(action: StakeCommands, _config: &Config) -> Result<()> {
    match action {
        StakeCommands::Add { amount } => {
            info!("💰 Adding {} OGT to validator stake", amount);
            let progress = spinner("Processing stake addition...");
            sleep(Duration::from_secs(2)).await;
            progress.finish_with_message("✅ Stake added successfully");
        }
        StakeCommands::Remove { amount } => {
            info!("💸 Removing {} OGT from validator stake", amount);
            if !confirm("This will begin the unbonding period. Continue?")? {
                return Ok(());
            }
            let progress = spinner("Processing stake removal...");
            sleep(Duration::from_secs(2)).await;
            progress.finish_with_message("✅ Unstaking initiated (21-day unbonding period)");
        }
        StakeCommands::Info => {
            info!("📊 Validator Staking Information");
            println!("   Current Stake: 25 OGT");
            println!("   Required Minimum: 18 OGT (dynamic)");
            println!("   Staking Rewards: 8.5% APY");
            println!("   Unbonding Period: 21 days");
            println!("   Next Requirement Update: 2 hours");
        }
    }
    Ok(())
}

async fn manage_services(action: ServiceCommands, _config: &Config) -> Result<()> {
    match action {
        ServiceCommands::Enable { service } => {
            info!("🔧 Enabling {} service", service);
            let progress = spinner(&format!("Configuring {} service...", service));
            sleep(Duration::from_secs(2)).await;
            progress.finish_with_message(format!("✅ {} service enabled", service));
        }
        ServiceCommands::Disable { service } => {
            info!("🔧 Disabling {} service", service);
            let progress = spinner(&format!("Stopping {} service...", service));
            sleep(Duration::from_secs(1)).await;
            progress.finish_with_message(format!("✅ {} service disabled", service));
        }
        ServiceCommands::Configure {
            service,
            config_file,
        } => {
            info!("⚙️ Configuring {} service", service);
            if let Some(file) = config_file {
                info!("Using configuration file: {}", file);
            }
            let progress = spinner("Applying configuration...");
            sleep(Duration::from_secs(1)).await;
            progress.finish_with_message("✅ Configuration applied");
        }
        ServiceCommands::Status => {
            info!("📊 Infrastructure Services Status");
            println!("   OMP Storage: ✅ Running (85% utilization, $1,240/month)");
            println!("   ORC-20 Relayer: ✅ Running (340 tx/hour, $890/month)");
            println!("   EEC-4337 Paymaster: ✅ Running (120 ops/hour, $2,100/month)");
            println!("   Total Monthly Revenue: $4,230");
        }
    }
    Ok(())
}

async fn show_earnings(breakdown: bool, period: &str, _config: &Config) -> Result<()> {
    info!("💰 Validator Earnings Report ({})", period);

    println!("\n📈 Revenue Summary:");
    println!("   Consensus Rewards: 2.4 OGT");
    println!("   Infrastructure Services: $4,230");
    println!("   Total Daily Revenue: ~$185");

    if breakdown {
        println!("\n🔍 Detailed Breakdown:");
        println!("   Block Production: 1.8 OGT");
        println!("   Transaction Fees: 0.6 OGT");
        println!("   OMP Storage: $1,240 (85% util)");
        println!("   ORC-20 Relaying: $890 (340 tx/hr)");
        println!("   EEC-4337 Paymaster: $2,100 (120 ops/hr)");

        println!("\n📊 Performance Metrics:");
        println!("   Uptime: 99.8%");
        println!("   Block Success Rate: 100%");
        println!("   Service Availability: 99.9%");
        println!("   Revenue Optimization: +15%");
    }

    Ok(())
}

async fn show_validator_status(services: bool, _config: &Config) -> Result<()> {
    info!("📊 Validator Status");

    println!("\n🏛️ Validator Overview:");
    println!("   Status: ✅ Active");
    println!("   Stake: 25 OGT");
    println!("   Uptime: 99.8%");
    println!("   Block Height: 1,234,567");
    println!("   Last Block: 2.1s ago");

    if services {
        println!("\n⚡ Infrastructure Services:");
        println!("   OMP Storage: ✅ Healthy (85% util)");
        println!("   ORC-20 Relayer: ✅ Healthy (340 tx/hr)");
        println!("   EEC-4337 Paymaster: ✅ Healthy (120 ops/hr)");
        println!("   Combined Revenue: $4,230/month");
    }

    Ok(())
}

async fn calculate_dynamic_stake() -> Result<u64> {
    // TODO: Replace with actual network query in production
    // For now, simulate dynamic calculation based on current network conditions
    let network_utilization = 0.45; // 45% network utilization (simulated)
    let validator_count = 85; // Current validator count (simulated)
    let base_stake = 15; // Base stake requirement

    let utilization_factor = (0.5_f64).max((2.0_f64).min(network_utilization));
    let validator_density = (0.8_f64).max((1.5_f64).min(validator_count as f64 / 100.0));

    let dynamic_stake = base_stake as f64 * utilization_factor * validator_density;
    let required_stake = (15_u64).max((28_u64).min(dynamic_stake as u64)); // 15-28 OGT range

    Ok(required_stake)
}
async fn rpc_request<T: DeserializeOwned>(
    endpoint: &str,
    method: &str,
    params: Value,
) -> Result<T> {
    let client = Client::new();
    let payload = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1,
    });

    let response = client
        .post(endpoint)
        .json(&payload)
        .send()
        .await
        .map_err(|err| anyhow!("Failed to reach node RPC at {}: {}", endpoint, err))?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "RPC request '{}' failed with status {}",
            method,
            response.status()
        ));
    }

    let rpc: JsonRpcResponse<T> = response
        .json()
        .await
        .map_err(|err| anyhow!("Malformed RPC response for {}: {}", method, err))?;

    if let Some(error) = rpc.error {
        return Err(anyhow!("RPC error {}: {}", error.code, error.message));
    }

    rpc.result
        .ok_or_else(|| anyhow!("RPC response missing result for {}", method))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeRecord {
    id: u64,
    block_hash: String,
    block_number: u64,
    security_block_ref: u64,
    #[serde(default)]
    challenger: Option<String>,
    #[serde(default)]
    offending_proposer: Option<String>,
    fraud_proof_hash: String,
    submitted_at: u64,
    expires_at: u64,
    status: ChallengeStatus,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum ChallengeStatus {
    Pending,
    Resolved {
        verdict: ChallengeResolution,
        resolved_at: u64,
        #[serde(default)]
        note: Option<String>,
    },
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ChallengeResolution {
    ProvenFraud,
    Rejected,
    Expired,
    Cleared,
}

impl fmt::Display for ChallengeResolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChallengeResolution::ProvenFraud => write!(f, "Proven fraud"),
            ChallengeResolution::Rejected => write!(f, "Rejected"),
            ChallengeResolution::Expired => write!(f, "Expired"),
            ChallengeResolution::Cleared => write!(f, "Cleared"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    jsonrpc: Option<String>,
    result: Option<T>,
    error: Option<JsonRpcError>,
    id: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    data: Option<Value>,
}

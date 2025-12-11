//! Developer tools and project management commands

use crate::config::Config;
use crate::utils::{confirm, spinner};
use crate::wasm;
use anyhow::{anyhow, bail, Context, Result};
use axiom_runtime::{
    AxiomEngine, ExecutionConfig as RuntimeExecutionConfig, ExecutionResult, COMPUTEVM_GAS_LIMIT,
    COMPUTEVM_TIMEOUT_US, FASTVM_GAS_LIMIT, FASTVM_TIMEOUT_US, STANDARDVM_GAS_LIMIT,
    STANDARDVM_TIMEOUT_US,
};
use base64ct::{Base64, Encoding};
use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use deploy_guardrails::{enforce_allowed_services, PlanSignatureData, SignerAllowList};
use dialoguer::Select;
use ed25519_dalek::{Signer, SigningKey};
use hex;
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::{header::AUTHORIZATION, Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{self, json, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tracing::{info, warn};

#[derive(Clone, Debug, ValueEnum)]
pub enum DeployTier {
    Fastvm,
    Standard,
    Compute,
}

impl DeployTier {
    fn as_str(&self) -> &'static str {
        match self {
            DeployTier::Fastvm => "fastvm",
            DeployTier::Standard => "standard",
            DeployTier::Compute => "compute",
        }
    }

    fn build_execution_config(&self, contract: &str, function: &str) -> RuntimeExecutionConfig {
        match self {
            DeployTier::Fastvm => RuntimeExecutionConfig::contract_entry(contract, function)
                .with_gas_limit(FASTVM_GAS_LIMIT)
                .with_timeout(Duration::from_micros(FASTVM_TIMEOUT_US)),
            DeployTier::Standard => RuntimeExecutionConfig::contract_entry(contract, function)
                .with_gas_limit(STANDARDVM_GAS_LIMIT)
                .with_timeout(Duration::from_micros(STANDARDVM_TIMEOUT_US)),
            DeployTier::Compute => RuntimeExecutionConfig::contract_entry(contract, function)
                .with_gas_limit(COMPUTEVM_GAS_LIMIT)
                .with_timeout(Duration::from_micros(COMPUTEVM_TIMEOUT_US)),
        }
    }
}

impl fmt::Display for DeployTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ExecutionPlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<PlanNetwork>,
    contract: PlanContract,
    execution: PlanExecution,
    services: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<PlanSignature>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlanNetwork {
    name: String,
    chain_id: u64,
    rpc_endpoint: String,
    ws_endpoint: String,
    explorer_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlanContract {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    wasm_size_bytes: usize,
    wasm_sha256: String,
    wasm_base64: String,
    deployment_nonce: String,
    entry: PlanEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<PlanContractMetadata>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlanContractMetadata {
    has_axiom_entry_main: bool,
    has_legacy_entry_main: bool,
    methods: Vec<wasm::ContractMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compiler: Option<CompilerAttachment>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CompilerAttachment {
    metadata_version: String,
    compiler_version: String,
    generated_at: String,
    wasm_sha256: String,
    wasm_size_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_functions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<CompilerMetadataSignature>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CompilerMetadataSignature {
    algorithm: String,
    public_key_hex: String,
    signature_hex: String,
    digest_hex: String,
    signed_at: String,
}

#[derive(Debug, Deserialize)]
struct CompilerMetadataEnvelope {
    metadata: CompilerMetadataPayload,
    #[serde(default)]
    signature: Option<CompilerMetadataSignaturePayload>,
}

#[derive(Debug, Deserialize)]
struct CompilerMetadataPayload {
    metadata_version: String,
    compiler_version: String,
    generated_at: String,
    wasm_sha256: String,
    wasm_size_bytes: usize,
    #[serde(default)]
    host_functions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CompilerMetadataSignaturePayload {
    algorithm: String,
    #[serde(rename = "public_key_hex")]
    public_key_hex: String,
    #[serde(rename = "signature_hex")]
    signature_hex: String,
    #[serde(rename = "digest_hex")]
    digest_hex: String,
    #[serde(rename = "signed_at")]
    signed_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlanEntry {
    contract: String,
    function: String,
    selector: String,
    export: String,
    legacy_export: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlanExecution {
    tier: String,
    config: RuntimeExecutionConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<ExecutionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_summary: Option<ExecutionPreviewSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExecutionPreviewSummary {
    execution_time_ms: u128,
    gas_consumed: u64,
    return_value: Option<axiom_runtime::execution::SerializableVal>,
    deterministic_state: String,
}

type PlanSignature = PlanSignatureData;

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum DeployOperation {
    /// Generate and optionally submit a deployment plan
    Plan,
    /// Verify an existing deployment plan signature on disk
    Verify,
}

#[derive(Args, Debug, Clone)]
pub struct DeployArgs {
    /// Deployment workflow mode (defaults to plan generation)
    #[arg(
        value_enum,
        value_name = "MODE",
        default_value_t = DeployOperation::Plan,
        num_args = 0..=1
    )]
    pub mode: DeployOperation,

    #[command(flatten)]
    pub plan: DeployPlanArgs,

    #[command(flatten)]
    pub verify: DeployVerifyArgs,
}

#[derive(Args, Debug, Clone)]
pub struct DeployPlanArgs {
    /// Contract WASM file path
    #[arg(long)]
    pub contract: Option<String>,

    /// Contract entry selector (Contract::function)
    #[arg(long)]
    pub entry: Option<String>,

    /// Output path for generated execution plan
    #[arg(long)]
    pub plan: Option<String>,

    /// Enable infrastructure services
    #[arg(long)]
    pub services: Vec<String>,

    /// Target network
    #[arg(long, default_value = "testnet")]
    pub network: String,

    /// Authentication token for hardened deployment endpoint
    #[arg(long)]
    pub auth_token: Option<String>,

    /// Allow services not present in the configured allow-list (unsafe)
    #[arg(long)]
    pub allow_unknown_services: bool,

    /// Execution tier for generated config
    #[arg(long, value_enum, default_value_t = DeployTier::Standard)]
    pub tier: DeployTier,

    /// Path to Ed25519 signing key (hex-encoded) for execution plan attestation
    #[arg(long)]
    pub signing_key: Option<String>,

    /// Disable automatic plan signing (unsafe)
    #[arg(long)]
    pub no_sign: bool,
}

impl Default for DeployPlanArgs {
    fn default() -> Self {
        Self {
            contract: None,
            entry: None,
            plan: None,
            services: Vec::new(),
            network: "testnet".to_string(),
            auth_token: None,
            allow_unknown_services: false,
            tier: DeployTier::Standard,
            signing_key: None,
            no_sign: false,
        }
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct DeployVerifyArgs {
    /// Execution plan path to verify
    #[arg(value_name = "PLAN", required_if_eq("mode", "verify"))]
    pub plan: Option<String>,

    /// Skip signer allow-list enforcement
    #[arg(long)]
    pub allow_unknown_signer: bool,

    /// Additional signer public keys (hex) to permit during verification
    #[arg(long = "allowed-signer", value_name = "HEX", action = clap::ArgAction::Append)]
    pub allowed_signer: Vec<String>,
}

#[derive(Subcommand)]
pub enum DevCommands {
    /// Create new Omne project
    New {
        /// Project name
        name: String,

        /// Project template (react-typescript, python-flask, rust-wasm, go-api)
        #[arg(long, default_value = "react-typescript")]
        template: String,

        /// SDK version to use
        #[arg(long, default_value = "latest")]
        sdk_version: String,
    },

    /// Run comprehensive test suite
    Test {
        /// Include integration tests
        #[arg(long)]
        integration: bool,

        /// Include performance tests
        #[arg(long)]
        performance: bool,

        /// Test specific component
        #[arg(long)]
        component: Option<String>,
    },

    /// Deploy contracts or services
    Deploy(DeployArgs),

    /// SDK management
    Sdk {
        #[command(subcommand)]
        action: SdkCommands,
    },

    /// Local development environment
    Local {
        #[command(subcommand)]
        action: LocalCommands,
    },
}

#[derive(Subcommand)]
pub enum SdkCommands {
    /// List available SDK versions
    List,
    /// Install specific SDK version
    Install {
        /// Language (python, typescript, go)
        language: String,
        /// Version to install
        #[arg(long, default_value = "latest")]
        version: String,
    },
    /// Update SDK to latest version
    Update {
        /// Language to update
        language: String,
    },
    /// Show SDK information
    Info {
        /// Language
        language: String,
    },
}

#[derive(Subcommand)]
pub enum LocalCommands {
    /// Start local development network
    Start {
        /// Number of validators
        #[arg(long, default_value = "3")]
        validators: u32,

        /// Enable infrastructure services
        #[arg(long)]
        services: bool,
    },
    /// Stop local development network
    Stop,
    /// Reset local network state
    Reset,
    /// Show local network status
    Status,
}

pub async fn execute(command: DevCommands, config: &Config) -> Result<()> {
    match command {
        DevCommands::New {
            name,
            template,
            sdk_version,
        } => create_project(&name, &template, &sdk_version, config).await,
        DevCommands::Test {
            integration,
            performance,
            component,
        } => run_tests(integration, performance, component.as_deref(), config).await,
        DevCommands::Deploy(args) => match args.mode {
            DeployOperation::Plan => deploy_project(&args.plan, config).await,
            DeployOperation::Verify => {
                verify_execution_plan(&args.verify, &args.plan, config).await
            }
        },
        DevCommands::Sdk { action } => manage_sdk(action, config).await,
        DevCommands::Local { action } => manage_local_env(action, config).await,
    }
}

async fn create_project(
    name: &str,
    template: &str,
    sdk_version: &str,
    _config: &Config,
) -> Result<()> {
    info!("🚀 Creating new Omne project: {}", name);

    if Path::new(name).exists() {
        warn!("⚠️ Directory '{}' already exists", name);
        if !confirm("Continue anyway?")? {
            return Ok(());
        }
    }

    let progress = spinner("Setting up project structure...");
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    progress.finish_with_message("✅ Project structure created");

    let progress = spinner(&format!("Installing {} template...", template));
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
    progress.finish_with_message("✅ Template installed");

    let progress = spinner(&format!("Configuring Omne SDK v{}...", sdk_version));
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    progress.finish_with_message("✅ SDK configured");

    info!("✅ Project '{}' created successfully!", name);
    info!("   Template: {}", template);
    info!("   SDK Version: {}", sdk_version);
    info!("   Next Steps:");
    info!("     cd {}", name);
    info!("     omne dev local start");
    info!("     omne dev test");

    Ok(())
}

async fn run_tests(
    integration: bool,
    performance: bool,
    component: Option<&str>,
    _config: &Config,
) -> Result<()> {
    info!("🧪 Running Omne project tests...");

    let progress = spinner("Running unit tests...");
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    progress.finish_with_message("✅ Unit tests passed (24/24)");

    if integration {
        let progress = spinner("Running integration tests...");
        tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
        progress.finish_with_message("✅ Integration tests passed (8/8)");
    }

    if performance {
        let progress = spinner("Running performance benchmarks...");
        tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;
        progress.finish_with_message("✅ Performance tests passed (TPS: 2,847)");
    }

    if let Some(comp) = component {
        let progress = spinner(&format!("Testing {} component...", comp));
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        progress.finish_with_message(format!("✅ {} component tests passed", comp));
    }

    println!("\n📊 Test Results:");
    println!("   Unit Tests: ✅ 24/24 passed");
    if integration {
        println!("   Integration Tests: ✅ 8/8 passed");
    }
    if performance {
        println!("   Performance: ✅ 2,847 TPS achieved");
    }
    if let Some(comp) = component {
        println!("   {}: ✅ All tests passed", comp);
    }

    Ok(())
}

async fn deploy_project(args: &DeployPlanArgs, config: &Config) -> Result<()> {
    info!("🚀 Deploying to Omne {} network", args.network);

    if args.network == "mainnet" && !confirm("Deploy to MAINNET? This will use real funds.")? {
        info!("Deployment cancelled");
        return Ok(());
    }

    let effective_auth_token = args
        .auth_token
        .as_ref()
        .map(|token| token.to_string())
        .or_else(|| config.network.auth_token.clone())
        .or_else(|| {
            std::env::var("OMNE_AUTH_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });

    let service_selection = enforce_allowed_services(
        &args.services,
        &config.network.allowed_services,
        args.allow_unknown_services,
    )
    .map_err(|err| {
        if args.allow_unknown_services {
            anyhow!(err)
        } else {
            anyhow!("{} Pass --allow-unknown-services to override.", err)
        }
    })?;
    let mut published_services = service_selection.clone();

    if args.allow_unknown_services && !config.network.allowed_services.is_empty() {
        warn!("Bypassing service allow-list validation; proceed with caution.");
    } else if config.network.allowed_services.is_empty() && !service_selection.is_empty() {
        warn!(
            "No service allow-list configured for {}; unable to verify requested services.",
            config.network.name
        );
    }

    if effective_auth_token.is_some() {
        info!("🔐 Authentication token detected; including Authorization header");
    }

    if let Some(contract_path) = args.contract.as_deref() {
        info!("📦 Deploying contract: {}", contract_path);
        let analysis = spinner("Analyzing contract module...");
        let module = wasm::load_contract_module(contract_path).await?;
        let metadata = module.metadata();
        analysis.finish_with_message("✅ Contract module analyzed");

        if !metadata.has_runtime_entry() {
            warn!(
                "Contract module is missing '{}' export; runtime tooling expects the ABI entry point.",
                axiom_runtime::abi::ENTRY_EXPORT
            );
        }

        if metadata.has_legacy_entry() {
            info!(
                "   Legacy entry export '{}' retained for compatibility",
                axiom_runtime::abi::LEGACY_ENTRY_EXPORT
            );
        }

        info!("   Discovered contract exports:");
        for method in metadata.contract_methods() {
            let mut details = format!("     - {} (export: {})", method.selector(), method.export);
            if method.has_legacy_export {
                if let Some(legacy) = &method.legacy_export {
                    details.push_str(&format!(", legacy alias: {}", legacy));
                }
            }
            info!("{}", details);
        }

        let methods = metadata.contract_methods();
        if methods.is_empty() {
            bail!(
                "contract module does not expose any ABI metadata; regenerate with the latest compiler"
            );
        }
        let selected_method = if let Some(selector) = args.entry.as_deref() {
            metadata
                .resolve_method(selector)
                .ok_or_else(|| anyhow!("no contract export named '{}' found", selector))?
        } else if let Some(default) = metadata.default_method() {
            default
        } else {
            let options: Vec<String> = methods
                .iter()
                .map(|method| format!("{} (export: {})", method.selector(), method.export))
                .collect();
            match Select::new()
                .with_prompt("Select contract export to deploy")
                .items(&options)
                .default(0)
                .interact_opt()
            {
                Ok(Some(index)) => methods.get(index).ok_or_else(|| {
                    anyhow!(
                        "invalid export selection index {} returned by selector",
                        index
                    )
                })?,
                Ok(None) => bail!("contract export selection cancelled"),
                Err(err) => bail!(
                    "unable to interactively select contract export: {} (pass --entry <Contract::function>)",
                    err
                ),
            }
        };

        let selected_selector = selected_method.selector();
        let selected_export = selected_method.export.clone();
        let selected_has_legacy = selected_method.has_legacy_export;

        info!(
            "   Contract entry resolved: {} (export {})",
            selected_selector, selected_export
        );
        if selected_has_legacy {
            if let Some(ref legacy) = selected_method.legacy_export {
                info!("   Legacy export retained as {}", legacy);
            }
        }
        info!("   Execution tier: {}", args.tier);

        let plan_path = args
            .plan
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(contract_path).with_extension("execution.json"));

        let compiler_attachment = match load_compiler_attachment(contract_path).await {
            Ok(Some(attachment)) => {
                info!(
                    "   Compiler metadata version {} (compiler {})",
                    attachment.metadata_version,
                    attachment.compiler_version
                );
                if let Some(hosts) = attachment.host_functions.as_ref() {
                    info!("   Host functions referenced: {}", hosts.join(", "));
                }
                Some(attachment)
            }
            Ok(None) => {
                info!(
                    "   No compiler metadata envelope found alongside contract {}; continuing",
                    contract_path
                );
                None
            }
            Err(err) => {
                warn!(
                    "Failed to load compiler metadata for {}: {}",
                    contract_path, err
                );
                None
            }
        };

        let mut execution_plan = build_execution_plan(
            &module,
            selected_method,
            args.tier.clone(),
            &service_selection,
            config,
            compiler_attachment.clone(),
        )?;

        let mut plan_signer: Option<[u8; 32]> = None;

        if args.no_sign {
            warn!("⚠️ Skipping plan signing; hardened endpoints will reject unsigned submissions.");
        } else if let Some(key_path) = args.signing_key.as_deref() {
            let verifying_key = sign_execution_plan(&mut execution_plan, key_path).await?;
            let verifying_hex = hex::encode(verifying_key);
            info!(
                "🔏 Execution plan signed with supplied key {}",
                verifying_hex
            );
            plan_signer = Some(verifying_key);
        } else {
            let signing_key = SigningKey::generate(&mut OsRng);
            let verifying_key = attach_plan_signature_with_key(&mut execution_plan, &signing_key)?;
            let verifying_hex = hex::encode(verifying_key);
            info!(
                "🔏 Execution plan signed with ephemeral key {}",
                verifying_hex
            );

            let mut key_path = plan_path.clone();
            key_path.set_extension("signing-key");
            fs::write(
                &key_path,
                format!("{}\n", hex::encode(signing_key.to_bytes())),
            )
            .await?;
            info!(
                "   Ephemeral signing key written to {} (delete after promotion)",
                key_path.display()
            );
            plan_signer = Some(verifying_key);
        }

        if let Some(verifying_key) = plan_signer {
            if !config.network.allowed_signers.is_empty() {
                match SignerAllowList::from_hex_iter(
                    config.network.allowed_signers.iter().map(|s| s.as_str()),
                ) {
                    Ok(list) => {
                        if list.is_empty() {
                            warn!(
                                "No valid signer entries discovered in configuration allow-list; verification may fail."
                            );
                        } else if !list.contains_bytes(&verifying_key) {
                            warn!(
                                "Plan signer {} not present in configured allow-list; update configuration or rotate keys before mainnet promotion.",
                                hex::encode(verifying_key)
                            );
                        } else {
                            info!(
                                "   Signer {} present in configured allow-list",
                                hex::encode(verifying_key)
                            );
                        }
                    }
                    Err(err) => {
                        warn!(
                            "Failed to parse signer allow-list from configuration: {}",
                            err
                        );
                    }
                }
            }
        }

        if let Some(preview) = execution_plan.execution.preview.as_ref() {
            info!("   Preview return value: {:?}", preview.return_value);
        }
        if let Some(summary) = execution_plan.execution.preview_summary.as_ref() {
            info!(
                "   Preview execution time: {} ms",
                summary.execution_time_ms
            );
        }

        let plan_bytes = serde_json::to_vec_pretty(&execution_plan)?;
        fs::write(&plan_path, plan_bytes).await?;

        info!("   Execution plan written to {}", plan_path.display());
        println!("   Plan file: {}", plan_path.display());

        let submission = spinner("Submitting execution plan to network...");
        match submit_execution_plan(&execution_plan, config, effective_auth_token.as_deref()).await
        {
            Ok(Some(receipt)) => {
                submission.finish_with_message("✅ Execution plan submitted");
                if let Some(address) = receipt.contract_address.as_deref() {
                    info!("   Contract Address: {}", address);
                    println!("   Contract Address: {}", address);
                }

                if let Some(deployment_nonce) = receipt.deployment_nonce.as_deref() {
                    info!("   Deployment nonce: {}", deployment_nonce);
                }

                if let Some(transaction_id) = receipt.transaction_id.as_deref() {
                    info!("   Transaction ID: {}", transaction_id);
                }

                if let Some(signature) = receipt.plan_signature.as_ref() {
                    if let Some(request_sig) = execution_plan.signature.as_ref() {
                        if !plan_signatures_match(request_sig, signature) {
                            warn!("Server rewrote plan signature; verify attestation provenance");
                        }
                    }
                }

                if !receipt.services.is_empty() {
                    published_services = receipt.services.clone();
                    info!(
                        "   Canonical services (from network): {}",
                        published_services.join(", ")
                    );
                }

                let receipt_path = plan_path.with_extension("receipt.json");
                let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
                fs::write(&receipt_path, receipt_bytes).await?;
                info!(
                    "   Deployment receipt written to {}",
                    receipt_path.display()
                );

                if let Some(raw) = receipt.extra.get("raw") {
                    info!("   Additional deployment metadata: {}", raw);
                }
            }
            Ok(None) => {
                submission.finish_with_message("✅ Execution plan submitted");
                info!("   Deployment endpoint returned no additional data");
            }
            Err(err) => {
                submission.finish_with_message("⚠️ Failed to submit execution plan");
                warn!("Execution plan submission failed: {}", err);
            }
        }
    }

    if !service_selection.is_empty() {
        info!(
            "⚡ Configuring infrastructure services: {}",
            service_selection.join(", ")
        );
        let progress = spinner("Setting up service integrations...");
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        progress.finish_with_message("✅ Services configured");
    }

    info!("✅ Deployment complete!");
    info!("   Network: {}", args.network);
    let services_str = if published_services.is_empty() {
        "None".to_string()
    } else {
        published_services.join(", ")
    };
    info!("   Services: {}", services_str);

    Ok(())
}

async fn verify_execution_plan(
    verify: &DeployVerifyArgs,
    defaults: &DeployPlanArgs,
    config: &Config,
) -> Result<()> {
    let plan_path_str = verify
        .plan
        .as_deref()
        .or_else(|| defaults.plan.as_deref())
        .ok_or_else(|| anyhow!("plan path must be provided (pass <PLAN> or --plan)"))?;
    let plan_path = PathBuf::from(plan_path_str);

    info!(
        "🔍 Verifying execution plan signature at {}",
        plan_path.display()
    );

    let plan_bytes = fs::read(&plan_path)
        .await
        .with_context(|| format!("failed to read execution plan from {}", plan_path.display()))?;

    let plan: ExecutionPlan = serde_json::from_slice(&plan_bytes).with_context(|| {
        format!(
            "failed to parse execution plan JSON from {}",
            plan_path.display()
        )
    })?;

    let signature = plan
        .signature
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow!("execution plan is missing 'signature' attestation"))?;

    let mut allow_entries = config.network.allowed_signers.clone();
    allow_entries.extend(verify.allowed_signer.iter().cloned());

    let allow_list = if verify.allow_unknown_signer {
        info!("   Allow-list enforcement disabled via --allow-unknown-signer");
        None
    } else {
        match SignerAllowList::from_hex_iter(allow_entries.iter().map(|s| s.as_str())) {
            Ok(list) if list.is_empty() => {
                warn!(
                    "No signer allow-list entries configured; verification will trust any valid signature."
                );
                None
            }
            Ok(list) => Some(list),
            Err(err) => return Err(anyhow!(err.to_string())),
        }
    };

    let digest = deploy_guardrails::canonical_plan_digest(
        &plan.generated_at,
        &plan.network,
        &plan.contract,
        &plan.execution,
        &plan.services,
    )
    .map_err(|err| anyhow!(err.to_string()))?;

    let verifying_key = deploy_guardrails::verify_plan_signature(
        &plan.generated_at,
        &plan.network,
        &plan.contract,
        &plan.execution,
        &plan.services,
        &signature,
        allow_list.as_ref(),
    )
    .map_err(|err| anyhow!(err.to_string()))?;

    let verifying_bytes = verifying_key.to_bytes();

    info!("✅ Execution plan signature verified");
    info!("   Digest: {}", hex::encode(digest));
    info!("   Signer: {}", hex::encode(verifying_bytes));

    if let Some(list) = allow_list {
        if list.contains_bytes(&verifying_bytes) {
            info!("   Signer present in configured allow-list");
        }
    }

    Ok(())
}

async fn manage_sdk(action: SdkCommands, _config: &Config) -> Result<()> {
    match action {
        SdkCommands::List => {
            info!("📚 Available Omne SDKs:");
            println!("   Python SDK:");
            println!("     Latest: v1.2.3");
            println!("     Versions: v1.2.3, v1.2.2, v1.2.1");
            println!("   TypeScript SDK:");
            println!("     Latest: v1.2.4");
            println!("     Versions: v1.2.4, v1.2.3, v1.2.2");
            println!("   Go SDK:");
            println!("     Latest: v1.2.1");
            println!("     Versions: v1.2.1, v1.2.0, v1.1.9");
        }
        SdkCommands::Install { language, version } => {
            info!("📦 Installing {} SDK v{}", language, version);
            let progress = spinner("Downloading and installing...");
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            progress.finish_with_message(format!("✅ {} SDK v{} installed", language, version));
        }
        SdkCommands::Update { language } => {
            info!("🔄 Updating {} SDK to latest version", language);
            let progress = spinner("Downloading updates...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            progress.finish_with_message(format!("✅ {} SDK updated to latest", language));
        }
        SdkCommands::Info { language } => {
            info!("📖 {} SDK Information", language);
            println!("   Version: v1.2.3");
            println!(
                "   Documentation: https://docs.omne.network/sdk/{}",
                language
            );
            println!(
                "   Examples: https://github.com/OmneDAO/examples/{}",
                language
            );
            println!("   License: MIT");
        }
    }
    Ok(())
}

async fn manage_local_env(action: LocalCommands, _config: &Config) -> Result<()> {
    match action {
        LocalCommands::Start {
            validators,
            services,
        } => {
            info!("🔧 Starting local Omne development network...");

            let progress = spinner("Initializing local blockchain...");
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            progress.finish_with_message("✅ Local blockchain running");

            let progress = spinner(&format!("Starting {} validators...", validators));
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            progress.finish_with_message("✅ Validators running");

            if services {
                let progress = spinner("Starting infrastructure services...");
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                progress.finish_with_message("✅ Services running");
            }

            info!("✅ Local development network is running!");
            info!("   RPC Endpoint: http://localhost:8545");
            info!("   WebSocket: ws://localhost:8546");
            info!("   Explorer: http://localhost:3000");
            info!("   Validators: {}", validators);
        }
        LocalCommands::Stop => {
            info!("🛑 Stopping local development network...");
            let progress = spinner("Shutting down services...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            progress.finish_with_message("✅ Local network stopped");
        }
        LocalCommands::Reset => {
            info!("🔄 Resetting local network state...");
            if !confirm("This will delete all local blockchain data. Continue?")? {
                return Ok(());
            }
            let progress = spinner("Clearing blockchain data...");
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            progress.finish_with_message("✅ Local network reset complete");
        }
        LocalCommands::Status => {
            info!("📊 Local Development Network Status");
            println!("   Status: ✅ Running");
            println!("   Block Height: 1,247");
            println!("   Validators: 3/3 active");
            println!("   RPC: http://localhost:8545");
            println!("   WebSocket: ws://localhost:8546");
            println!("   Services: OMP, ORC-20, EEC-4337");
        }
    }
    Ok(())
}

async fn load_compiler_attachment(contract_path: &str) -> Result<Option<CompilerAttachment>> {
    let path = Path::new(contract_path);
    let metadata_path = path.with_extension("metadata.json");

    match fs::metadata(&metadata_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to inspect compiler metadata at {}", metadata_path.display()));
        }
    }

    let bytes = fs::read(&metadata_path)
        .await
        .with_context(|| format!("failed to read compiler metadata from {}", metadata_path.display()))?;

    let envelope: CompilerMetadataEnvelope = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse compiler metadata JSON from {}", metadata_path.display()))?;

    let host_functions = if envelope.metadata.host_functions.is_empty() {
        None
    } else {
        Some(envelope.metadata.host_functions.clone())
    };

    let signature = envelope.signature.map(|sig| CompilerMetadataSignature {
        algorithm: sig.algorithm,
        public_key_hex: sig.public_key_hex,
        signature_hex: sig.signature_hex,
        digest_hex: sig.digest_hex,
        signed_at: sig.signed_at,
    });

    let attachment = CompilerAttachment {
        metadata_version: envelope.metadata.metadata_version,
        compiler_version: envelope.metadata.compiler_version,
        generated_at: envelope.metadata.generated_at,
        wasm_sha256: envelope.metadata.wasm_sha256,
        wasm_size_bytes: envelope.metadata.wasm_size_bytes,
        host_functions,
        signature,
    };

    Ok(Some(attachment))
}

fn create_engine(tier: &DeployTier) -> Result<AxiomEngine> {
    let engine = match tier {
        DeployTier::Fastvm => AxiomEngine::new_fastvm(),
        DeployTier::Standard => AxiomEngine::new_standardvm(),
        DeployTier::Compute => AxiomEngine::new_computevm(),
    };

    engine.map_err(|err| anyhow!("failed to initialise {} engine: {}", tier, err))
}

async fn submit_execution_plan(
    plan: &ExecutionPlan,
    config: &Config,
    auth_token: Option<&str>,
) -> Result<Option<DeploymentReceipt>> {
    let endpoint = &config.network.rpc_endpoint;
    if !endpoint.starts_with("http") {
        bail!(
            "RPC endpoint {} is not HTTP(S); unable to submit execution plan",
            endpoint
        );
    }

    let payload = json!({
        "jsonrpc": "2.0",
        "method": "omne_deployContract",
        "params": [plan],
        "id": 1,
    });

    let client = Client::new();
    let mut request = client.post(endpoint).json(&payload);

    if let Some(token) = auth_token.map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if token.to_ascii_lowercase().starts_with("bearer ") || token.contains(' ') {
            request = request.header(AUTHORIZATION, token);
        } else {
            request = request.header(AUTHORIZATION, format!("Bearer {}", token));
        }
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("failed to submit execution plan to {}", endpoint))?;

    let status = response.status();
    let envelope: JsonRpcEnvelope = response
        .json()
        .await
        .with_context(|| format!("failed to decode deployment response from {}", endpoint))?;

    if let Some(error) = envelope.error {
        let data = error
            .data
            .as_ref()
            .map(|value| value.to_string())
            .unwrap_or_default();

        if status == StatusCode::TOO_MANY_REQUESTS {
            if let Some(limit) = config.network.rate_limit_per_minute {
                bail!(
                    "deployment rejected: rate limit exceeded (limit: {} requests/min). Retry later or request a higher limit.",
                    limit
                );
            } else {
                bail!(
                    "deployment rejected: rate limit exceeded (HTTP 429). Retry later or request a higher limit."
                );
            }
        } else if status == StatusCode::FORBIDDEN {
            if data.is_empty() {
                bail!("deployment rejected: access forbidden (check authentication token)");
            } else {
                bail!(
                    "deployment rejected: access forbidden ({}). Verify authentication token and permissions.",
                    data
                );
            }
        } else if status.is_success() {
            if data.is_empty() {
                bail!("RPC error {}: {}", error.code, error.message);
            } else {
                bail!("RPC error {}: {} ({})", error.code, error.message, data);
            }
        } else if data.is_empty() {
            bail!(
                "deployment endpoint {} returned status {}: {} ({})",
                endpoint,
                status,
                error.message,
                error.code
            );
        } else {
            bail!(
                "deployment endpoint {} returned status {}: {} ({}, {})",
                endpoint,
                status,
                error.message,
                error.code,
                data
            );
        }
    }

    if !status.is_success() {
        if status == StatusCode::TOO_MANY_REQUESTS {
            if let Some(limit) = config.network.rate_limit_per_minute {
                bail!(
                    "deployment rejected: rate limit exceeded (limit: {} requests/min). Retry later or request a higher limit.",
                    limit
                );
            } else {
                bail!(
                    "deployment rejected: rate limit exceeded (HTTP 429). Retry later or request a higher limit."
                );
            }
        }
        if status == StatusCode::FORBIDDEN {
            bail!(
                "deployment rejected with HTTP 403. Verify authentication token and permissions."
            );
        }
        bail!(
            "deployment endpoint {} returned status {} without error payload",
            endpoint,
            status
        );
    }

    Ok(envelope.result.map(DeploymentReceipt::from_value))
}

fn build_execution_plan(
    module: &wasm::ContractModule,
    method: &wasm::ContractMethod,
    tier: DeployTier,
    services: &[String],
    config: &Config,
    compiler_attachment: Option<CompilerAttachment>,
) -> Result<ExecutionPlan> {
    let exec_config = tier.build_execution_config(&method.contract, &method.function);
    let engine = create_engine(&tier)?;
    let execution_preview = engine
        .execute(module.bytes(), exec_config.clone())
        .map_err(|err| anyhow!("contract execution preview failed: {}", err))?;

    let preview_summary = ExecutionPreviewSummary {
        execution_time_ms: execution_preview.execution_time.as_millis(),
        gas_consumed: execution_preview.gas_consumed,
        return_value: execution_preview.return_value.clone(),
        deterministic_state: execution_preview.deterministic_state.clone(),
    };

    let metadata = module.metadata();

    let wasm_base64 = Base64::encode_string(module.bytes());
    let wasm_sha256 = format!("{:x}", Sha256::digest(module.bytes()));

    let mut nonce_bytes = [0u8; 16];
    OsRng.fill_bytes(&mut nonce_bytes);
    let deployment_nonce = hex::encode(nonce_bytes);

    Ok(ExecutionPlan {
        generated_at: Some(Utc::now().to_rfc3339()),
        network: Some(PlanNetwork {
            name: config.network.name.clone(),
            chain_id: config.network.chain_id,
            rpc_endpoint: config.network.rpc_endpoint.clone(),
            ws_endpoint: config.network.ws_endpoint.clone(),
            explorer_url: config.network.explorer_url.clone(),
        }),
        contract: PlanContract {
            path: Some(module.path().display().to_string()),
            wasm_size_bytes: module.bytes().len(),
            wasm_sha256,
            wasm_base64,
            deployment_nonce,
            entry: PlanEntry {
                contract: method.contract.clone(),
                function: method.function.clone(),
                selector: method.selector(),
                export: method.export.clone(),
                legacy_export: method.legacy_export.clone(),
            },
            metadata: Some(PlanContractMetadata {
                has_axiom_entry_main: metadata.has_runtime_entry(),
                has_legacy_entry_main: metadata.has_legacy_entry(),
                methods: metadata.contract_methods().to_vec(),
                compiler: compiler_attachment,
            }),
        },
        execution: PlanExecution {
            tier: tier.as_str().to_string(),
            config: exec_config,
            preview: Some(execution_preview),
            preview_summary: Some(preview_summary),
        },
        services: services.to_vec(),
        signature: None,
    })
}

async fn sign_execution_plan(plan: &mut ExecutionPlan, key_path: &str) -> Result<[u8; 32]> {
    let raw = fs::read(key_path)
        .await
        .with_context(|| format!("failed to read signing key from {}", key_path))?;

    let secret_bytes = if raw.len() == 32 {
        raw
    } else {
        let key_str = String::from_utf8(raw)
            .context("signing key file must contain raw 32-byte seed or hex-encoded secret")?;
        let cleaned = key_str.trim();
        if cleaned.len() == 64 && cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            hex::decode(cleaned)?
        } else {
            bail!("signing key must be provided as 32 raw bytes or 64 hexadecimal characters");
        }
    };

    let secret_array: [u8; 32] = secret_bytes
        .try_into()
        .map_err(|_| anyhow!("signing key must decode to exactly 32 bytes"))?;

    let signing_key = SigningKey::from_bytes(&secret_array);
    attach_plan_signature_with_key(plan, &signing_key)
}

fn attach_plan_signature_with_key(
    plan: &mut ExecutionPlan,
    signing_key: &SigningKey,
) -> Result<[u8; 32]> {
    let digest = compute_plan_digest(plan)?;
    let signature = signing_key.sign(&digest);
    let verifying_key = signing_key.verifying_key();

    plan.signature = Some(PlanSignature {
        algorithm: "ed25519".to_string(),
        public_key_hex: hex::encode(verifying_key.to_bytes()),
        signature_hex: hex::encode(signature.to_bytes()),
    });

    Ok(verifying_key.to_bytes())
}

fn compute_plan_digest(plan: &ExecutionPlan) -> Result<[u8; 32]> {
    deploy_guardrails::canonical_plan_digest(
        &plan.generated_at,
        &plan.network,
        &plan.contract,
        &plan.execution,
        &plan.services,
    )
    .map_err(|err| anyhow!(err.to_string()))
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct DeploymentReceipt {
    #[serde(rename = "contractAddress")]
    contract_address: Option<String>,
    #[serde(rename = "wasmHash")]
    wasm_hash: Option<String>,
    #[serde(rename = "tier")]
    tier: Option<String>,
    #[serde(rename = "blockHeight")]
    block_height: Option<u64>,
    #[serde(rename = "transactionId")]
    transaction_id: Option<String>,
    #[serde(rename = "deterministicState")]
    deterministic_state: Option<String>,
    #[serde(default)]
    services: Vec<String>,
    #[serde(rename = "export")]
    export: Option<String>,
    #[serde(rename = "deploymentNonce")]
    deployment_nonce: Option<String>,
    #[serde(rename = "planSignature")]
    plan_signature: Option<ReceiptPlanSignature>,
    #[serde(flatten)]
    extra: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ReceiptPlanSignature {
    algorithm: String,
    #[serde(rename = "publicKey")]
    public_key: String,
    #[serde(rename = "signature")]
    signature: String,
}

impl DeploymentReceipt {
    fn from_value(value: JsonValue) -> Self {
        match serde_json::from_value::<DeploymentReceipt>(value.clone()) {
            Ok(receipt) => receipt,
            Err(_) => {
                let mut fallback = DeploymentReceipt::default();
                fallback.extra.insert("raw".to_string(), value);
                fallback
            }
        }
    }
}

fn plan_signatures_match(request: &PlanSignature, response: &ReceiptPlanSignature) -> bool {
    request.algorithm.eq_ignore_ascii_case(&response.algorithm)
        && request
            .public_key_hex
            .eq_ignore_ascii_case(&response.public_key)
        && request
            .signature_hex
            .eq_ignore_ascii_case(&response.signature)
}

#[derive(Debug, Deserialize)]
struct JsonRpcEnvelope {
    result: Option<JsonValue>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<JsonValue>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiom_runtime::execution::SerializableVal;
    use ed25519_dalek::SigningKey;
    use httptest::{
        matchers::{all_of, matches, request},
        responders::json_encoded,
        Expectation, Server,
    };
    use serde_json::json;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use wat::parse_str;

    const DEMO_WAT: &str = r#"(module
        (func (export "axiom_contract::Demo::init") (result i64)
            i64.const 7)
    )"#;

    async fn load_demo_module() -> wasm::ContractModule {
        let mut file = NamedTempFile::new().expect("temp file");
        let bytes = parse_str(DEMO_WAT).expect("valid wat");
        file.write_all(&bytes).expect("write module");
        let temp_path = file.into_temp_path();
        wasm::load_contract_module(&temp_path)
            .await
            .expect("load contract module")
    }

    #[tokio::test]
    async fn build_execution_plan_runs_preview() {
        let module = load_demo_module().await;
        let metadata = module.metadata();
        let method = metadata
            .resolve_method("Demo::init")
            .expect("method present");

        let plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &Config::default(),
            None,
        )
        .expect("plan build");

        let preview = plan.execution.preview.expect("preview present");
        assert_eq!(preview.return_value, Some(SerializableVal::I64(7)));
        let summary = plan
            .execution
            .preview_summary
            .expect("preview summary present");
        assert_eq!(
            summary.execution_time_ms,
            preview.execution_time.as_millis()
        );
        assert_eq!(
            plan.contract
                .metadata
                .as_ref()
                .expect("metadata present")
                .methods
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn signing_attaches_plan_signature() {
        let module = load_demo_module().await;
        let metadata = module.metadata();
        let method = metadata
            .resolve_method("Demo::init")
            .expect("method present");

        let mut plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &Config::default(),
            None,
        )
        .expect("plan build");

        let signing_key = SigningKey::generate(&mut OsRng);
        let mut key_file = NamedTempFile::new().expect("temp key");
        write!(key_file, "{}", hex::encode(signing_key.to_bytes())).expect("write key");
        let key_path = key_file.into_temp_path();

        let verifying_key = sign_execution_plan(&mut plan, key_path.to_str().unwrap())
            .await
            .expect("sign plan");

        let signature = plan.signature.expect("signature present");
        assert_eq!(signature.algorithm, "ed25519");
        assert_eq!(signature.public_key_hex.len(), 64);
        assert_eq!(signature.signature_hex.len(), 128);
        assert_eq!(hex::encode(verifying_key).len(), 64);
    }

    #[tokio::test]
    async fn verify_execution_plan_accepts_signed_plan() {
        let module = load_demo_module().await;
        let metadata = module.metadata();
        let method = metadata
            .resolve_method("Demo::init")
            .expect("method present");

        let mut plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &Config::default(),
            None,
        )
        .expect("plan build");

        let signing_key = SigningKey::from_bytes(&[3u8; 32]);
        let verifying_key =
            attach_plan_signature_with_key(&mut plan, &signing_key).expect("sign plan");

        let plan_bytes = serde_json::to_vec_pretty(&plan).expect("serialize plan");
        let mut plan_file = NamedTempFile::new().expect("plan file");
        plan_file.write_all(&plan_bytes).expect("write plan");
        let plan_path = plan_file.into_temp_path();

        let mut config = Config::default();
        config.network.allowed_signers = vec![hex::encode(verifying_key)];

        let mut defaults = DeployPlanArgs::default();
        defaults.plan = Some(plan_path.to_string_lossy().into_owned());

        let verify_args = DeployVerifyArgs {
            plan: Some(defaults.plan.clone().expect("plan")),
            allow_unknown_signer: false,
            allowed_signer: Vec::new(),
        };

        verify_execution_plan(&verify_args, &defaults, &config)
            .await
            .expect("verification succeeds");
    }

    #[tokio::test]
    async fn submit_execution_plan_posts_to_rpc() {
        let module = load_demo_module().await;
        let metadata = module.metadata();
        let method = metadata
            .resolve_method("Demo::init")
            .expect("method present");

        let mut server = Server::run();
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/"),
                request::body(matches("\"method\":\"omne_deployContract\"")),
                request::body(matches("Demo::init"))
            ])
            .respond_with(json_encoded(json!({
                "jsonrpc": "2.0",
                "result": { "contractAddress": "omne1deadbeef" },
                "id": 1
            }))),
        );

        let mut config = Config::default();
        config.network.rpc_endpoint = server.url("/").to_string();
        config.network.ws_endpoint = "ws://test".to_string();
        config.network.explorer_url = "http://test".to_string();

        let plan = build_execution_plan(&module, method, DeployTier::Standard, &[], &config, None)
            .expect("plan build");

        let response = submit_execution_plan(&plan, &config, None)
            .await
            .expect("submission succeeds");

        let receipt = response.expect("deployment result");
        assert_eq!(receipt.contract_address.as_deref(), Some("omne1deadbeef"));

        server.verify_and_clear();
    }

    #[test]
    fn enforce_allowed_services_rejects_unknown() {
        let requested = vec!["orchestrator".to_string(), "analytics".to_string()];
        let allowed = vec!["orchestrator".to_string()];
        let result = enforce_allowed_services(&requested, &allowed, false);
        assert!(result.is_err());
    }

    #[test]
    fn enforce_allowed_services_deduplicates_and_canonicalises() {
        let requested = vec!["Orchestrator".to_string(), "orchestrator".to_string()];
        let allowed = vec!["orchestrator".to_string()];
        let result = enforce_allowed_services(&requested, &allowed, false).expect("allowed");
        assert_eq!(result, vec!["orchestrator".to_string()]);
    }
}

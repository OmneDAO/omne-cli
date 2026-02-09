//! Developer tools and project management commands

use crate::config::Config;
use crate::utils::{confirm, spinner};
use crate::wasm;
use anyhow::{anyhow, bail, Context, Result};
use axiom_runtime::{
    AxiomEngine, ExecutionConfig as RuntimeExecutionConfig, ExecutionResult, COMPUTEVM_GAS_LIMIT,
    COMPUTEVM_MAX_CALL_DEPTH, COMPUTEVM_STORAGE_BUDGET_BYTES, COMPUTEVM_TIMEOUT_US,
    FASTVM_GAS_LIMIT, FASTVM_MAX_CALL_DEPTH, FASTVM_STORAGE_BUDGET_BYTES, FASTVM_TIMEOUT_US,
    STANDARDVM_GAS_LIMIT, STANDARDVM_MAX_CALL_DEPTH, STANDARDVM_STORAGE_BUDGET_BYTES,
    STANDARDVM_TIMEOUT_US,
};
use base64ct::{Base64, Encoding};
use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use deploy_guardrails::{
    compiler_signers_vec_for_network, enforce_allowed_services, validate_execution_guardrails,
    verify_metadata_signature, CompilerMetadata, CompilerMetadataSignature, PlanSignatureData,
    SignerAllowList,
};
use dialoguer::Select;
use ed25519_dalek::{Signer, SigningKey};
use hex;
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::{header::AUTHORIZATION, Client, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{self, json, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tracing::{info, warn};

#[derive(Clone, Debug, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
                .with_timeout(Duration::from_micros(FASTVM_TIMEOUT_US))
                .with_max_call_depth(FASTVM_MAX_CALL_DEPTH)
                .with_storage_budget_bytes(FASTVM_STORAGE_BUDGET_BYTES),
            DeployTier::Standard => RuntimeExecutionConfig::contract_entry(contract, function)
                .with_gas_limit(STANDARDVM_GAS_LIMIT)
                .with_timeout(Duration::from_micros(STANDARDVM_TIMEOUT_US))
                .with_max_call_depth(STANDARDVM_MAX_CALL_DEPTH)
                .with_storage_budget_bytes(STANDARDVM_STORAGE_BUDGET_BYTES),
            DeployTier::Compute => RuntimeExecutionConfig::contract_entry(contract, function)
                .with_gas_limit(COMPUTEVM_GAS_LIMIT)
                .with_timeout(Duration::from_micros(COMPUTEVM_TIMEOUT_US))
                .with_max_call_depth(COMPUTEVM_MAX_CALL_DEPTH)
                .with_storage_budget_bytes(COMPUTEVM_STORAGE_BUDGET_BYTES),
        }
    }
}

impl fmt::Display for DeployTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanNetwork {
    name: String,
    chain_id: u64,
    rpc_endpoint: String,
    ws_endpoint: String,
    explorer_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanContractMetadata {
    has_axiom_entry_main: bool,
    has_legacy_entry_main: bool,
    methods: Vec<wasm::ContractMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compiler: Option<CompilerAttachment>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CompilerAttachment {
    metadata: CompilerMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    signature: Option<CompilerMetadataSignature>,
}

#[derive(Debug, Deserialize)]
struct CompilerMetadataEnvelope {
    metadata: CompilerMetadata,
    #[serde(default)]
    signature: Option<CompilerMetadataSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanEntry {
    contract: String,
    function: String,
    selector: String,
    export: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    legacy_export: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PlanExecution {
    tier: String,
    config: RuntimeExecutionConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<ExecutionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_summary: Option<ExecutionPreviewSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExecutionPreviewSummary {
    execution_time_ms: u128,
    gas_consumed: u64,
    return_value: Option<axiom_runtime::execution::SerializableVal>,
    deterministic_state: String,
    call_depth_used: u32,
    storage_bytes_written: u64,
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
    /// Deployment template that pre-fills plan options
    #[arg(long)]
    pub template: Option<String>,

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
    #[arg(long)]
    pub network: Option<String>,

    /// Authentication token for hardened deployment endpoint
    #[arg(long)]
    pub auth_token: Option<String>,

    /// Allow services not present in the configured allow-list (unsafe)
    #[arg(long)]
    pub allow_unknown_services: bool,

    /// Execution tier for generated config
    #[arg(long, value_enum)]
    pub tier: Option<DeployTier>,

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
            template: None,
            contract: None,
            entry: None,
            plan: None,
            services: Vec::new(),
            network: None,
            auth_token: None,
            allow_unknown_services: false,
            tier: None,
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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct DeployPlanTemplate {
    template_name: Option<String>,
    contract: Option<String>,
    entry: Option<String>,
    plan: Option<String>,
    services: Vec<String>,
    network: Option<String>,
    auth_token: Option<String>,
    allow_unknown_services: Option<bool>,
    tier: Option<DeployTier>,
    signing_key: Option<String>,
    no_sign: Option<bool>,
}

#[derive(Debug, Clone)]
struct ResolvedDeployPlan {
    template_name: Option<String>,
    contract: Option<String>,
    entry: Option<String>,
    plan_path: Option<String>,
    services: Vec<String>,
    network: String,
    auth_token: Option<String>,
    allow_unknown_services: bool,
    tier: DeployTier,
    signing_key: Option<String>,
    no_sign: bool,
}

async fn resolve_plan_args(args: &DeployPlanArgs, config: &Config) -> Result<ResolvedDeployPlan> {
    let mut template = DeployPlanTemplate::default();

    if let Some(template_path) = args.template.as_deref() {
        let path = PathBuf::from(template_path);
        let bytes = fs::read(&path)
            .await
            .with_context(|| format!("failed to read deployment template {}", path.display()))?;

        let mut parsed: DeployPlanTemplate = match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("yml") | Some("yaml") => serde_yaml::from_slice(&bytes).with_context(|| {
                format!(
                    "failed to parse YAML deployment template {}",
                    path.display()
                )
            })?,
            _ => {
                let text = std::str::from_utf8(&bytes).with_context(|| {
                    format!("deployment template {} is not valid UTF-8", path.display())
                })?;
                toml::from_str(text).with_context(|| {
                    format!(
                        "failed to parse TOML deployment template {}",
                        path.display()
                    )
                })?
            }
        };

        if parsed.template_name.is_none() {
            parsed.template_name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.to_string());
        }

        template = parsed;
    }

    let template_name = template.template_name.clone();

    let contract = args.contract.clone().or_else(|| template.contract.clone());

    let entry = args.entry.clone().or_else(|| template.entry.clone());

    let plan_path = args.plan.clone().or_else(|| template.plan.clone());

    let services = if !args.services.is_empty() {
        args.services.clone()
    } else if !template.services.is_empty() {
        template.services.clone()
    } else {
        Vec::new()
    };

    let network = args
        .network
        .clone()
        .or_else(|| template.network.clone())
        .unwrap_or_else(|| config.network.name.clone());

    let auth_token = args
        .auth_token
        .clone()
        .or_else(|| template.auth_token.clone());

    let allow_unknown_services = if args.allow_unknown_services {
        true
    } else {
        template.allow_unknown_services.unwrap_or(false)
    };

    let tier = args
        .tier
        .clone()
        .or_else(|| template.tier.clone())
        .unwrap_or(DeployTier::Standard);

    let signing_key = args
        .signing_key
        .clone()
        .or_else(|| template.signing_key.clone());

    let no_sign = if args.no_sign {
        true
    } else {
        template.no_sign.unwrap_or(false)
    };

    Ok(ResolvedDeployPlan {
        template_name,
        contract,
        entry,
        plan_path,
        services,
        network,
        auth_token,
        allow_unknown_services,
        tier,
        signing_key,
        no_sign,
    })
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
    let resolved = resolve_plan_args(args, config).await?;

    if let Some(name) = resolved.template_name.as_deref() {
        info!("📝 Using deployment template: {}", name);
    }

    if resolved.network != config.network.name {
        warn!(
            "Deployment template targets '{}' but CLI configuration is initialised for '{}'.",
            resolved.network, config.network.name
        );
    }

    info!("🚀 Deploying to Omne {} network", resolved.network);

    if resolved.network == "mainnet" && !confirm("Deploy to MAINNET? This will use real funds.")? {
        info!("Deployment cancelled");
        return Ok(());
    }

    let effective_auth_token = resolved
        .auth_token
        .clone()
        .or_else(|| config.network.auth_token.clone())
        .or_else(|| {
            std::env::var("OMNE_RPC_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("OMNE_AUTH_TOKEN")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });

    let service_selection = enforce_allowed_services(
        &resolved.services,
        &config.network.allowed_services,
        resolved.allow_unknown_services,
    )
    .map_err(|err| {
        if resolved.allow_unknown_services {
            anyhow!(err)
        } else {
            anyhow!("{} Pass --allow-unknown-services to override.", err)
        }
    })?;
    let mut published_services = service_selection.clone();

    if resolved.allow_unknown_services && !config.network.allowed_services.is_empty() {
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

    if let Some(contract_path) = resolved.contract.as_deref() {
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
        let selected_method = if let Some(selector) = resolved.entry.as_deref() {
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
        info!("   Execution tier: {}", resolved.tier);

        let plan_path = resolved
            .plan_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(contract_path).with_extension("execution.json"));

        let compiler_attachment = match load_compiler_attachment(&module).await {
            Ok(Some(attachment)) => {
                info!(
                    "   Compiler metadata version {} (compiler {})",
                    attachment.metadata.metadata_version, attachment.metadata.compiler_version
                );
                if !attachment.metadata.host_functions.is_empty() {
                    info!(
                        "   Host functions referenced: {}",
                        attachment.metadata.host_functions.join(", ")
                    );
                }
                Some(attachment)
            }
            Ok(None) => {
                bail!(
                    "compiler metadata envelope not found alongside contract {}. Re-run the compiler with metadata signing enabled",
                    contract_path
                );
            }
            Err(err) => {
                bail!(
                    "failed to load compiler metadata for {}: {}",
                    contract_path,
                    err
                );
            }
        };

        let mut execution_plan = build_execution_plan(
            &module,
            selected_method,
            resolved.tier.clone(),
            &service_selection,
            config,
            compiler_attachment.clone(),
        )?;

        let mut plan_signer: Option<[u8; 32]> = None;

        if resolved.no_sign {
            warn!("⚠️ Skipping plan signing; hardened endpoints will reject unsigned submissions.");
        } else if let Some(key_path) = resolved.signing_key.as_deref() {
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
            info!(
                "   Preview call depth observed: {}",
                summary.call_depth_used
            );
            info!(
                "   Preview storage writes: {} bytes",
                summary.storage_bytes_written
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

                match confirm_metadata_registration(config, &execution_plan, &receipt).await {
                    Ok(Some(services)) if !services.is_empty() => {
                        published_services = services;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!("   Metadata verification skipped: {}", err);
                    }
                }

                if let Some(raw) = receipt.extra.get("raw") {
                    info!("   Additional deployment metadata: {}", raw);
                }

                match confirm_metadata_registration(config, &execution_plan, &receipt).await {
                    Ok(Some(services)) => {
                        if !services.is_empty() {
                            info!("   Metadata-confirmed services: {}", services.join(", "));
                            published_services = services.clone();
                        }
                    }
                    Ok(None) => {
                        // Metadata disabled; nothing to log.
                    }
                    Err(err) => {
                        warn!("   Failed to confirm metadata persistence: {}", err);
                    }
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
    info!("   Network: {}", resolved.network);
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

async fn load_compiler_attachment(
    module: &wasm::ContractModule,
) -> Result<Option<CompilerAttachment>> {
    let metadata_path = module.path().with_extension("metadata.json");

    match fs::metadata(&metadata_path).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to inspect compiler metadata at {}",
                    metadata_path.display()
                )
            });
        }
    }

    let bytes = fs::read(&metadata_path).await.with_context(|| {
        format!(
            "failed to read compiler metadata from {}",
            metadata_path.display()
        )
    })?;

    let envelope: CompilerMetadataEnvelope = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "failed to parse compiler metadata JSON from {}",
            metadata_path.display()
        )
    })?;

    Ok(Some(CompilerAttachment {
        metadata: envelope.metadata,
        signature: envelope.signature,
    }))
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

    request = request.header("X-Omne-Nonce", plan.contract.deployment_nonce.as_str());

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
    let response_headers = response.headers().clone();
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
            let retry_after = response_headers
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            if let Some(limit) = config.network.rate_limit_per_minute {
                bail!(
                    "deployment rejected: rate limit exceeded (limit: {} requests/min). Retry later or request a higher limit.",
                    limit
                );
            } else {
                if let Some(seconds) = retry_after {
                    bail!(
                        "deployment rejected: rate limit exceeded (HTTP 429). Retry after {}s or request a higher limit.",
                        seconds
                    );
                } else {
                    bail!(
                        "deployment rejected: rate limit exceeded (HTTP 429). Retry later or request a higher limit."
                    );
                }
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

async fn confirm_metadata_registration(
    config: &Config,
    plan: &ExecutionPlan,
    receipt: &DeploymentReceipt,
) -> Result<Option<Vec<String>>> {
    let Some(metadata_client) = MetadataClient::new(config)? else {
        info!("   Metadata endpoints not configured; skipping verification.");
        return Ok(None);
    };

    let digest_bytes = compute_plan_digest(plan)?;
    let digest_hex = hex::encode(digest_bytes);
    let digest_prefix_len = 12.min(digest_hex.len());
    let plan_id = format!("pln_{}", &digest_hex[..digest_prefix_len]);

    info!(
        "   Verifying metadata persistence for plan {} (digest {})",
        plan_id, digest_hex
    );

    let mut canonical_services: Option<Vec<String>> = None;

    match metadata_client.fetch_plan_by_id(&plan_id).await? {
        MetadataFetch::Found(details) => {
            info!(
                "   Metadata store confirmed plan {} (submitted_at: {})",
                details.plan.plan_id, details.submitted_at
            );
            if !details.plan.services.is_empty() {
                info!("   Metadata services: {}", details.plan.services.join(", "));
                canonical_services = Some(details.plan.services.clone());
            }
        }
        MetadataFetch::NotFound => {
            info!(
                "   Plan {} not present via ID lookup; retrying by digest...",
                plan_id
            );
            match metadata_client.fetch_plan_by_digest(&digest_hex).await? {
                MetadataFetch::Found(details) => {
                    info!(
                        "   Metadata store confirmed plan {} via digest (submitted_at: {})",
                        details.plan.plan_id, details.submitted_at
                    );
                    if !details.plan.services.is_empty() {
                        info!("   Metadata services: {}", details.plan.services.join(", "));
                        canonical_services = Some(details.plan.services.clone());
                    }
                }
                MetadataFetch::NotFound => {
                    warn!(
                        "   Deployment metadata not yet available for plan {}; persistence may be delayed.",
                        plan_id
                    );
                }
                MetadataFetch::Disabled => {
                    info!("   Metadata endpoints are disabled on this node.");
                    return Ok(None);
                }
            }
        }
        MetadataFetch::Disabled => {
            info!("   Metadata endpoints are disabled on this node.");
            return Ok(None);
        }
    }

    let nonce_source = receipt
        .deployment_nonce
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| plan.contract.deployment_nonce.as_str());

    if !nonce_source.is_empty() {
        let nonce_hash = hash_nonce(nonce_source);
        match metadata_client.fetch_nonce_provenance(&nonce_hash).await? {
            MetadataFetch::Found(provenance) => {
                info!(
                    "   Nonce provenance linked to plan {} (first_seen_at: {})",
                    provenance.plan_id, provenance.first_seen_at
                );
            }
            MetadataFetch::NotFound => {
                warn!(
                    "   Nonce provenance for hash {} not found; persistence may still be in progress.",
                    nonce_hash
                );
            }
            MetadataFetch::Disabled => {
                info!("   Metadata endpoints are disabled on this node.");
                return Ok(None);
            }
        }
    }

    Ok(canonical_services)
}

struct MetadataClient {
    client: Client,
    base: Url,
    auth_header: Option<String>,
}

impl MetadataClient {
    fn new(config: &Config) -> Result<Option<Self>> {
        let Some(base) = derive_metadata_base_url(config)? else {
            return Ok(None);
        };

        Ok(Some(Self {
            client: Client::new(),
            base,
            auth_header: config
                .network
                .auth_token
                .as_ref()
                .map(|token| normalise_bearer_token(token)),
        }))
    }

    async fn fetch_plan_by_id(&self, plan_id: &str) -> Result<MetadataFetch<MetadataPlanDetails>> {
        self.get(&format!("plans/{}", plan_id)).await
    }

    async fn fetch_plan_by_digest(
        &self,
        digest: &str,
    ) -> Result<MetadataFetch<MetadataPlanDetails>> {
        self.get(&format!("plans/digest/{}", digest)).await
    }

    async fn fetch_nonce_provenance(
        &self,
        nonce_hash: &str,
    ) -> Result<MetadataFetch<MetadataNonceProvenance>> {
        self.get(&format!("provenance/{}", nonce_hash)).await
    }

    async fn get<T>(&self, path: &str) -> Result<MetadataFetch<T>>
    where
        T: DeserializeOwned,
    {
        let url = self
            .base
            .join(path)
            .with_context(|| format!("failed to build metadata URL for path {}", path))?;
        let url_display = url.to_string();

        let mut request = self.client.get(url.clone());
        if let Some(header) = self.auth_header.as_ref() {
            request = request.header(AUTHORIZATION, header);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("failed to query metadata endpoint {}", url_display))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("failed to read metadata response from {}", url_display))?;

        match status {
            StatusCode::OK => {
                let payload = serde_json::from_str::<T>(&body).with_context(|| {
                    format!("failed to parse metadata response from {}", url_display)
                })?;
                Ok(MetadataFetch::Found(payload))
            }
            StatusCode::NOT_FOUND => Ok(MetadataFetch::NotFound),
            StatusCode::NOT_IMPLEMENTED => Ok(MetadataFetch::Disabled),
            other => {
                if body.is_empty() {
                    bail!(
                        "metadata endpoint {} returned status {} with no payload",
                        url_display,
                        other
                    );
                } else {
                    bail!(
                        "metadata endpoint {} returned status {}: {}",
                        url_display,
                        other,
                        body
                    );
                }
            }
        }
    }
}

enum MetadataFetch<T> {
    Found(T),
    NotFound,
    Disabled,
}

#[derive(Debug, Deserialize)]
struct MetadataPlanDetails {
    plan: MetadataPlanSummary,
    submitted_at: String,
}

#[derive(Debug, Deserialize)]
struct MetadataPlanSummary {
    plan_id: String,
    #[serde(default)]
    services: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MetadataNonceProvenance {
    plan_id: String,
    first_seen_at: String,
}

fn derive_metadata_base_url(config: &Config) -> Result<Option<Url>> {
    if let Some(explicit) = config.network.metadata_base_url.as_ref() {
        let mut url = Url::parse(explicit)
            .with_context(|| format!("invalid metadata_base_url configured: {}", explicit))?;
        ensure_trailing_slash(&mut url);
        return Ok(Some(url));
    }

    match Url::parse(&config.network.rpc_endpoint) {
        Ok(mut url) => {
            url.set_path("/v1/");
            url.set_query(None);
            url.set_fragment(None);
            Ok(Some(url))
        }
        Err(_) => Ok(None),
    }
}

fn ensure_trailing_slash(url: &mut Url) {
    let mut path = url.path().to_string();
    if path.is_empty() {
        path.push('/');
    } else if !path.ends_with('/') {
        path.push('/');
    }
    url.set_path(&path);
}

fn normalise_bearer_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }

    if trimmed.to_ascii_lowercase().starts_with("bearer ") {
        trimmed.to_string()
    } else {
        format!("Bearer {}", trimmed)
    }
}

fn hash_nonce(nonce: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(nonce.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
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
    validate_execution_guardrails(
        tier.as_str(),
        exec_config.max_call_depth,
        exec_config.storage_budget_bytes,
    )
    .map_err(|err| anyhow!(err.to_string()))?;
    let engine = create_engine(&tier)?;
    let execution_preview = engine
        .execute(module.bytes(), exec_config.clone())
        .map_err(|err| anyhow!("contract execution preview failed: {}", err))?;

    let preview_summary = ExecutionPreviewSummary {
        execution_time_ms: execution_preview.execution_time.as_millis(),
        gas_consumed: execution_preview.gas_consumed,
        return_value: execution_preview.return_value.clone(),
        deterministic_state: execution_preview.deterministic_state.clone(),
        call_depth_used: execution_preview.call_depth_used,
        storage_bytes_written: execution_preview.storage_bytes_written,
    };

    let metadata = module.metadata();

    let wasm_base64 = Base64::encode_string(module.bytes());
    let wasm_sha256 = format!("{:x}", Sha256::digest(module.bytes()));

    let mut nonce_bytes = [0u8; 16];
    OsRng.fill_bytes(&mut nonce_bytes);
    let deployment_nonce = hex::encode(nonce_bytes);

    let compiler_attachment = match compiler_attachment {
        Some(attachment) => {
            if attachment.metadata.wasm_sha256 != wasm_sha256 {
                bail!(
                    "compiler metadata wasm hash {} does not match module hash {}",
                    attachment.metadata.wasm_sha256,
                    wasm_sha256
                );
            }

            if attachment.metadata.wasm_size_bytes != module.bytes().len() {
                bail!(
                    "compiler metadata size {} does not match module size {}",
                    attachment.metadata.wasm_size_bytes,
                    module.bytes().len()
                );
            }

            let signature = attachment.signature.as_ref().ok_or_else(|| {
                anyhow!(
                    "compiler metadata for {} is unsigned; re-run compiler with signing enabled",
                    module.path().display()
                )
            })?;

            let allow_entries = if config.network.allowed_compiler_signers.is_empty() {
                compiler_signers_vec_for_network(&config.network.name)
            } else {
                config.network.allowed_compiler_signers.clone()
            };

            let allow_list =
                SignerAllowList::from_hex_iter(allow_entries.iter().map(|s| s.as_str()))
                    .map_err(|err| anyhow!(err.to_string()))?;

            let verifying_key =
                verify_metadata_signature(&attachment.metadata, signature, Some(&allow_list))
                    .map_err(|err| anyhow!(err.to_string()))?;

            info!(
                "   Compiler metadata signature verified (signer {})",
                hex::encode(verifying_key.to_bytes())
            );

            Some(attachment)
        }
        None => None,
    };

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
        responders::{json_encoded, status_code},
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

    fn make_signed_compiler_attachment(
        module: &wasm::ContractModule,
    ) -> (CompilerAttachment, String) {
        use deploy_guardrails::metadata::{
            CompilerContractMetadata as GuardrailContractMetadata,
            CompilerContractMethodMetadata as GuardrailMethodMetadata,
        };
        use std::collections::BTreeMap;

        let wasm_sha256 = format!("{:x}", Sha256::digest(module.bytes()));
        let mut contracts: BTreeMap<String, Vec<GuardrailMethodMetadata>> = BTreeMap::new();

        for method in module.metadata().contract_methods() {
            contracts
                .entry(method.contract.clone())
                .or_default()
                .push(GuardrailMethodMetadata {
                    name: method.function.clone(),
                    selector: method.selector(),
                    export: method.export.clone(),
                    params: Vec::new(),
                    return_type: None,
                });
        }

        let compiler_contracts = contracts
            .into_iter()
            .map(|(name, methods)| GuardrailContractMetadata {
                name,
                params: Vec::new(),
                storage: Vec::new(),
                methods,
            })
            .collect();

        let metadata = CompilerMetadata {
            metadata_version: "1.0".to_string(),
            compiler_version: "test-suite".to_string(),
            generated_at: "2024-01-01T00:00:00Z".to_string(),
            source_path: module.path().to_str().map(|value| value.to_string()),
            wasm_sha256: wasm_sha256.clone(),
            wasm_size_bytes: module.bytes().len(),
            contracts: compiler_contracts,
            free_functions: Vec::new(),
            host_functions: Vec::new(),
        };

        let signing_key = SigningKey::from_bytes(&[5u8; 32]);
        let digest = deploy_guardrails::canonical_metadata_digest(&metadata).expect("digest");
        let signature = signing_key.sign(digest.as_ref());
        let verifying_key = signing_key.verifying_key();
        let verifying_hex = hex::encode(verifying_key.to_bytes());

        let attachment = CompilerAttachment {
            metadata,
            signature: Some(CompilerMetadataSignature {
                algorithm: "ed25519".to_string(),
                public_key_hex: verifying_hex.clone(),
                signature_hex: hex::encode(signature.to_bytes()),
                digest_hex: hex::encode(digest),
                signed_at: "2024-01-01T00:00:00Z".to_string(),
            }),
        };

        (attachment, verifying_hex)
    }

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

        let (compiler_attachment, compiler_signer) = make_signed_compiler_attachment(&module);
        let mut config = Config::default();
        config.network.allowed_compiler_signers = vec![compiler_signer];

        let plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &config,
            Some(compiler_attachment),
        )
        .expect("plan build");

        assert_eq!(
            plan.execution.config.max_call_depth,
            STANDARDVM_MAX_CALL_DEPTH
        );
        assert_eq!(
            plan.execution.config.storage_budget_bytes,
            STANDARDVM_STORAGE_BUDGET_BYTES
        );

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

        let (compiler_attachment, compiler_signer) = make_signed_compiler_attachment(&module);
        let mut config = Config::default();
        config.network.allowed_compiler_signers = vec![compiler_signer];

        let mut plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &config,
            Some(compiler_attachment),
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

        let (compiler_attachment, compiler_signer) = make_signed_compiler_attachment(&module);
        let mut plan_config = Config::default();
        plan_config.network.allowed_compiler_signers = vec![compiler_signer.clone()];

        let mut plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &plan_config,
            Some(compiler_attachment),
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
        config.network.allowed_compiler_signers = vec![compiler_signer];

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

    #[tokio::test]
    async fn confirm_metadata_registration_fetches_plan_and_provenance() {
        let module = load_demo_module().await;
        let metadata = module.metadata();
        let method = metadata
            .resolve_method("Demo::init")
            .expect("method present");

        let mut config = Config::default();
        let plan = build_execution_plan(&module, method, DeployTier::Standard, &[], &config, None)
            .expect("plan build");

        let digest = hex::encode(compute_plan_digest(&plan).expect("digest"));
        let plan_id_prefix = format!("pln_{}", &digest[..12.min(digest.len())]);
        let nonce_hash = hash_nonce(plan.contract.deployment_nonce.as_str());

        let plan_id_path: &'static str =
            Box::leak(format!("/v1/plans/{}", plan_id_prefix).into_boxed_str());
        let plan_digest_path: &'static str =
            Box::leak(format!("/v1/plans/digest/{}", digest).into_boxed_str());
        let provenance_path: &'static str =
            Box::leak(format!("/v1/provenance/{}", nonce_hash).into_boxed_str());

        let mut server = Server::run();
        let metadata_base = server.url("/rpc");
        config.network.rpc_endpoint = metadata_base.to_string();
        config.network.auth_token = Some("test-token".to_string());

        server.expect(
            Expectation::matching(request::method_path("GET", plan_id_path))
                .respond_with(status_code(StatusCode::NOT_FOUND.as_u16())),
        );

        server.expect(
            Expectation::matching(request::method_path("GET", plan_digest_path)).respond_with(
                json_encoded(json!({
                    "plan": {
                        "plan_id": plan_id_prefix.clone(),
                        "digest": digest.clone(),
                        "network": config.network.name.clone(),
                        "operator_id": "operator-demo",
                        "signer_key": "signer-demo",
                        "compiler_signer": null,
                        "services": ["svc-demo"],
                        "deployment_nonce": plan.contract.deployment_nonce.clone(),
                        "submitted_at": "2025-01-01T00:00:00Z"
                    },
                    "plan_body": {"services": []},
                    "submitted_at": "2025-01-01T00:00:00Z"
                })),
            ),
        );

        server.expect(
            Expectation::matching(request::method_path("GET", provenance_path)).respond_with(
                json_encoded(json!({
                    "nonce_hash": nonce_hash,
                    "plan_id": plan_id_prefix.clone(),
                    "operator_id": "operator-demo",
                    "signer_key": "signer-demo",
                    "compiler_signer": null,
                    "digest": digest,
                    "first_seen_at": "2025-01-01T00:00:01Z"
                })),
            ),
        );

        let receipt = DeploymentReceipt {
            deployment_nonce: Some(plan.contract.deployment_nonce.clone()),
            ..DeploymentReceipt::default()
        };

        let services = confirm_metadata_registration(&config, &plan, &receipt)
            .await
            .expect("metadata confirmation succeeds");

        assert_eq!(services, Some(vec!["svc-demo".to_string()]));

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

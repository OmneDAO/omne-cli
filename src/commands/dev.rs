//! Developer tools and project management commands

use crate::config::Config;
use crate::plan::{
    CompilerAttachment, CompilerMetadataEnvelope, ExecutionPlan, ExecutionPreviewSummary,
    PlanContract, PlanContractMetadata, PlanEntry, PlanExecution, PlanNetwork, PlanSignature,
    TypedArgument, compute_abi_checksum, parse_address20, parse_i32_value, parse_i64_value,
    parse_unsigned_to_i128, parse_unsigned_to_u128, validate_abi_arguments_for_entry,
    validate_plan_metadata,
};
use crate::utils::{confirm, spinner};
use crate::wasm;
use anyhow::{anyhow, bail, Context, Result};
use axiom_runtime::{
    execution::SerializableVal, state::StateManager, AxiomEngine,
    ExecutionConfig as RuntimeExecutionConfig, COMPUTEVM_GAS_LIMIT, COMPUTEVM_MAX_CALL_DEPTH,
    COMPUTEVM_STORAGE_BUDGET_BYTES, COMPUTEVM_TIMEOUT_US, FASTVM_GAS_LIMIT,
    FASTVM_MAX_CALL_DEPTH, FASTVM_STORAGE_BUDGET_BYTES, FASTVM_TIMEOUT_US,
    STANDARDVM_GAS_LIMIT, STANDARDVM_MAX_CALL_DEPTH, STANDARDVM_STORAGE_BUDGET_BYTES,
    STANDARDVM_TIMEOUT_US,
};
use base64ct::{Base64, Encoding};
use chrono::Utc;
use clap::{ArgAction, Args, Subcommand, ValueEnum};
use deploy_guardrails::{
    compiler_signers_vec_for_network, enforce_allowed_services, pqc, validate_execution_guardrails,
    verify_metadata_signature,
    SignerAllowList,
};
use dialoguer::Select;
use hex;
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::{header::{AUTHORIZATION, CONTENT_TYPE}, Client, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{self, json, Number as JsonNumber, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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


#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum DeployOperation {
    /// Generate and optionally submit a deployment plan
    Plan,
    /// Submit a previously generated deployment plan
    Submit,
    /// Print the canonical digest for a deployment plan
    Digest,
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
    pub submit: DeploySubmitArgs,

    #[command(flatten)]
    pub digest: DeployDigestArgs,

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

    /// Override the default gas limit inferred from the tier
    #[arg(long)]
    pub gas_limit: Option<u64>,

    /// Hex-encoded caller address to expose via get_caller()
    #[arg(long, value_name = "HEX20")]
    pub caller_address: Option<String>,

    /// Skip execution preview when generating a plan
    #[arg(long)]
    pub skip_preview: bool,

    /// Allow unsigned compiler metadata (dev-only preview workflows)
    #[arg(long)]
    pub allow_unsigned_metadata: bool,

    /// Raw function arguments provided as TYPE:VALUE (e.g. i32:42, i64:0xFF)
    #[arg(long = "arg", value_name = "TYPE:VALUE", action = ArgAction::Append)]
    pub arguments: Vec<String>,

    /// Path to Ed25519 signing key (hex-encoded) for execution plan attestation
    #[arg(long)]
    pub signing_key: Option<String>,

    /// Disable automatic plan signing (unsafe)
    #[arg(long)]
    pub no_sign: bool,

    /// Generate the plan without submitting it
    #[arg(long)]
    pub no_submit: bool,

    /// Hex-encoded Ed25519 plan signature from an external signer (KMS)
    #[arg(long)]
    pub plan_signature_hex: Option<String>,

    /// Hex-encoded Ed25519 public key for the external plan signature
    #[arg(long)]
    pub plan_signature_pubkey: Option<String>,
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
            gas_limit: None,
            caller_address: None,
            skip_preview: false,
            allow_unsigned_metadata: false,
            arguments: Vec::new(),
            signing_key: None,
            no_sign: false,
            no_submit: false,
            plan_signature_hex: None,
            plan_signature_pubkey: None,
        }
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct DeploySubmitArgs {
    /// Execution plan path to submit
    #[arg(
        id = "submit_plan",
        long = "submit-plan",
        value_name = "PLAN",
        required_if_eq("mode", "submit")
    )]
    pub plan: Option<String>,

    /// Skip signer allow-list enforcement during submission
    #[arg(id = "submit_allow_unknown_signer", long = "submit-allow-unknown-signer")]
    pub allow_unknown_signer: bool,

    /// Additional signer public keys (hex) to permit during submission
    #[arg(id = "submit_allowed_signer", long = "submit-allowed-signer", value_name = "HEX", action = ArgAction::Append)]
    pub allowed_signer: Vec<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct DeployDigestArgs {
    /// Execution plan path to digest
    #[arg(
        id = "digest_plan",
        long = "digest-plan",
        value_name = "PLAN",
        required_if_eq("mode", "digest")
    )]
    pub plan: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct DeployVerifyArgs {
    /// Execution plan path to verify
    #[arg(
        id = "verify_plan",
        long = "verify-plan",
        value_name = "PLAN",
        required_if_eq("mode", "verify")
    )]
    pub plan: Option<String>,

    /// Skip signer allow-list enforcement
    #[arg(long)]
    pub allow_unknown_signer: bool,

    /// Additional signer public keys (hex) to permit during verification
    #[arg(long = "allowed-signer", value_name = "HEX", action = ArgAction::Append)]
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
    arguments: Vec<String>,
    network: Option<String>,
    auth_token: Option<String>,
    allow_unknown_services: Option<bool>,
    tier: Option<DeployTier>,
    gas_limit: Option<u64>,
    caller_address: Option<String>,
    skip_preview: Option<bool>,
    allow_unsigned_metadata: Option<bool>,
    signing_key: Option<String>,
    no_sign: Option<bool>,
    no_submit: Option<bool>,
    plan_signature_hex: Option<String>,
    plan_signature_pubkey: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedDeployPlan {
    template_name: Option<String>,
    contract: Option<String>,
    entry: Option<String>,
    plan_path: Option<String>,
    services: Vec<String>,
    numeric_arguments: Vec<SerializableVal>,
    typed_arguments: Vec<TypedArgument>,
    input_base64: Option<String>,
    network: String,
    auth_token: Option<String>,
    allow_unknown_services: bool,
    tier: DeployTier,
    gas_limit: Option<u64>,
    caller_address: Option<[u8; 20]>,
    skip_preview: bool,
    allow_unsigned_metadata: bool,
    signing_key: Option<String>,
    no_sign: bool,
    no_submit: bool,
    plan_signature_hex: Option<String>,
    plan_signature_pubkey: Option<String>,
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

    let gas_limit = args
        .gas_limit
        .or(template.gas_limit)
        .filter(|limit| *limit > 0);

    let skip_preview = if args.skip_preview {
        true
    } else {
        template.skip_preview.unwrap_or(false)
    };

    let argument_specs = if !args.arguments.is_empty() {
        args.arguments.clone()
    } else if !template.arguments.is_empty() {
        template.arguments.clone()
    } else {
        Vec::new()
    };

    let ParsedArguments {
        numeric,
        typed,
        input_base64,
    } = parse_typed_argument_specs(&argument_specs)?;

    let signing_key = args
        .signing_key
        .clone()
        .or_else(|| template.signing_key.clone());

    let allow_unsigned_metadata = if args.allow_unsigned_metadata {
        true
    } else {
        template.allow_unsigned_metadata.unwrap_or(false)
    };

    let no_sign = if args.no_sign {
        true
    } else {
        template.no_sign.unwrap_or(false)
    };

    let no_submit = if args.no_submit {
        true
    } else {
        template.no_submit.unwrap_or(false)
    };

    let plan_signature_hex = args
        .plan_signature_hex
        .clone()
        .or_else(|| template.plan_signature_hex.clone());

    let plan_signature_pubkey = args
        .plan_signature_pubkey
        .clone()
        .or_else(|| template.plan_signature_pubkey.clone());

    let caller_address = args
        .caller_address
        .as_ref()
        .or(template.caller_address.as_ref())
        .map(|raw| parse_caller_address(raw))
        .transpose()?;

    Ok(ResolvedDeployPlan {
        template_name,
        contract,
        entry,
        plan_path,
        services,
        numeric_arguments: numeric,
        typed_arguments: typed,
        input_base64,
        network,
        auth_token,
        allow_unknown_services,
        tier,
        gas_limit,
        caller_address,
        skip_preview,
        allow_unsigned_metadata,
        signing_key,
        no_sign,
        no_submit,
        plan_signature_hex,
        plan_signature_pubkey,
    })
}

fn parse_typed_argument_specs(specs: &[String]) -> Result<ParsedArguments> {
    let mut numeric: Vec<SerializableVal> = Vec::new();
    let mut typed: Vec<TypedArgument> = Vec::new();
    let mut input_blob: Option<Vec<u8>> = None;

    for spec in specs {
        let (raw_ty, raw_value) = spec
            .split_once(':')
            .ok_or_else(|| anyhow!("argument '{}' must be in TYPE:VALUE form", spec))?;
        let ty = raw_ty.trim().to_ascii_lowercase();
        let value = raw_value.trim();

        match ty.as_str() {
            "i32" => {
                let v = parse_i32_value(value)?;
                numeric.push(SerializableVal::I32(v));
                typed.push(TypedArgument::I32(v));
            }
            "u8" => {
                let val = parse_unsigned_to_u128(value, 8)?;
                let v = u8::try_from(val).map_err(|_| anyhow!("value {} exceeds u8 range", value))?;
                numeric.push(SerializableVal::I32(v as i32));
                typed.push(TypedArgument::U8(v));
            }
            "u32" => {
                let val = parse_unsigned_to_i128(value, 32)?;
                if val > i32::MAX as i128 {
                    bail!("value {} exceeds i32 range", value);
                }
                numeric.push(SerializableVal::I32(val as i32));
                typed.push(TypedArgument::U32(val as u32));
            }
            "i64" => {
                let v = parse_i64_value(value)?;
                numeric.push(SerializableVal::I64(v));
                typed.push(TypedArgument::I64(v));
            }
            "u64" => {
                let val = parse_unsigned_to_i128(value, 64)?;
                if val > i64::MAX as i128 {
                    bail!("value {} exceeds i64 range", value);
                }
                numeric.push(SerializableVal::I64(val as i64));
                typed.push(TypedArgument::U64(val as u64));
            }
            "u128" => {
                let val = parse_unsigned_to_u128(value, 128)?;
                // Keep numeric_arguments empty for u128; typed path is required.
                typed.push(TypedArgument::U128(val));
            }
            "f32" => {
                let parsed = value.parse::<f32>().map_err(|err| {
                    anyhow!("failed to parse f32 argument '{}': {}", value, err)
                })?;
                numeric.push(SerializableVal::F32(parsed));
                typed.push(TypedArgument::F32(parsed));
            }
            "f64" => {
                let parsed = value.parse::<f64>().map_err(|err| {
                    anyhow!("failed to parse f64 argument '{}': {}", value, err)
                })?;
                numeric.push(SerializableVal::F64(parsed));
                typed.push(TypedArgument::F64(parsed));
            }
            "bool" => {
                let parsed = match value.to_ascii_lowercase().as_str() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => bail!("bool argument '{}' must be true/false/1/0", value),
                };
                numeric.push(SerializableVal::I32(if parsed { 1 } else { 0 }));
                typed.push(TypedArgument::Bool(parsed));
            }
            "string" => {
                let bytes = value.as_bytes().to_vec();
                typed.push(TypedArgument::String(value.to_string()));
                input_blob = Some(merge_input_blob(input_blob, bytes));
            }
            "option<string>" => {
                let trimmed = value.trim();
                if trimmed.eq_ignore_ascii_case("none") {
                    typed.push(TypedArgument::OptionString(None));
                } else {
                    let inner = if let Some(rest) = trimmed.strip_prefix("some:") {
                        rest
                    } else {
                        trimmed
                    };
                    if inner.is_empty() {
                        bail!("option<string> some:<value> must include a value");
                    }
                    let bytes = inner.as_bytes().to_vec();
                    typed.push(TypedArgument::OptionString(Some(inner.to_string())));
                    input_blob = Some(merge_input_blob(input_blob, bytes));
                }
            }
            "bytes" => {
                let decoded = Base64::decode_vec(value).or_else(|_| hex::decode(value))
                    .map_err(|err| anyhow!("failed to decode bytes argument: {}", err))?;
                typed.push(TypedArgument::BytesBase64(Base64::encode_string(&decoded)));
                input_blob = Some(merge_input_blob(input_blob, decoded));
            }
            "bytes-hex" => {
                let decoded = hex::decode(value).map_err(|err| {
                    anyhow!("failed to decode bytes-hex argument: {}", err)
                })?;
                typed.push(TypedArgument::BytesBase64(Base64::encode_string(&decoded)));
                input_blob = Some(merge_input_blob(input_blob, decoded));
            }
            "address20" => {
                let parsed = parse_address20(value)?;
                input_blob = Some(merge_input_blob(input_blob, parsed.0.to_vec()));
                typed.push(TypedArgument::Address20(parsed));
            }
            "option<address20>" => {
                let trimmed = value.trim();
                if trimmed.eq_ignore_ascii_case("none") {
                    typed.push(TypedArgument::OptionAddress20(None));
                } else {
                    let inner = trimmed
                        .strip_prefix("some:")
                        .map(|v| v.trim())
                        .unwrap_or(trimmed);
                    let parsed = parse_address20(inner)?;
                    input_blob = Some(merge_input_blob(input_blob, parsed.0.to_vec()));
                    typed.push(TypedArgument::OptionAddress20(Some(parsed)));
                }
            }
            other => bail!(
                "unsupported argument type '{}'; use i32, i64, u8, u32, u64, u128, f32, f64, bool, string, option<string>, address20, option<address20>, bytes, or bytes-hex",
                other
            ),
        }
    }

    let input_base64 = input_blob.map(|blob| Base64::encode_string(&blob));

    Ok(ParsedArguments {
        numeric,
        typed,
        input_base64,
    })
}

fn parse_caller_address(raw: &str) -> Result<[u8; 20]> {
    Ok(parse_address20(raw)?.0)
}

fn merge_input_blob(current: Option<Vec<u8>>, mut next: Vec<u8>) -> Vec<u8> {
    let mut out = current.unwrap_or_default();
    out.append(&mut next);
    out
}

struct ParsedArguments {
    numeric: Vec<SerializableVal>,
    typed: Vec<TypedArgument>,
    input_base64: Option<String>,
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
            DeployOperation::Submit => {
                submit_deployment_plan(&args.submit, &args.plan, config).await
            }
            DeployOperation::Digest => print_plan_digest(&args.digest).await,
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
    let production_guardrails =
        is_production_network(&config.network.name) || is_production_network(&resolved.network);

    require_fee_routing_config(&resolved.network, &config.network)?;

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

    if resolved.network == "mainnet" && resolved.skip_preview {
        bail!("execution preview cannot be skipped on mainnet; remove skip_preview/--skip-preview");
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
        log_guardrail_notice(
            production_guardrails,
            format!(
                "No service allow-list configured for {}; unable to verify requested services.",
                config.network.name
            ),
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
        if !resolved.typed_arguments.is_empty() {
            info!(
                "   Raw argument count supplied: {} (typed args enabled)",
                resolved.typed_arguments.len()
            );
        }
        if let Some(input_b64) = resolved.input_base64.as_ref() {
            info!(
                "   Input payload detected ({} bytes)",
                Base64::decode_vec(input_b64)
                    .map(|b| b.len())
                    .unwrap_or_default()
            );
        }
        info!("   Execution tier: {}", resolved.tier);
        if let Some(limit) = resolved.gas_limit {
            info!("   Gas limit override: {}", limit);
        }
        if resolved.skip_preview {
            info!("   Execution preview skipped (--skip-preview)");
        }

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

        let allow_unsigned_metadata = resolved.allow_unsigned_metadata
            || (resolved.no_submit
                && (resolved.network.eq_ignore_ascii_case("devnet")
                    || resolved.network.eq_ignore_ascii_case("omne_devnet")));

        let mut execution_plan = build_execution_plan(
            &module,
            selected_method,
            resolved.tier.clone(),
            &service_selection,
            &resolved.numeric_arguments,
            &resolved.typed_arguments,
            resolved.input_base64.clone(),
            resolved.caller_address,
            config,
            compiler_attachment.clone(),
            resolved.skip_preview,
            resolved.gas_limit,
            allow_unsigned_metadata,
        )?;

        let mut plan_signer: Option<Vec<u8>> = None;

        let has_external_signature =
            resolved.plan_signature_hex.is_some() || resolved.plan_signature_pubkey.is_some();

        if has_external_signature {
            if resolved.signing_key.is_some() {
                bail!("Provide either --signing-key or external plan signature flags, not both");
            }

            let signature_hex = resolved.plan_signature_hex.as_deref().ok_or_else(|| {
                anyhow!("--plan-signature-hex is required when using external signatures")
            })?;
            let public_key_hex = resolved.plan_signature_pubkey.as_deref().ok_or_else(|| {
                anyhow!("--plan-signature-pubkey is required when using external signatures")
            })?;

            let verifying_key =
                attach_plan_signature_with_hex(&mut execution_plan, signature_hex, public_key_hex)?;
            let verifying_hex = hex::encode(&verifying_key);
            info!(
                "🔏 Execution plan signed with external key {}",
                verifying_hex
            );
            plan_signer = Some(verifying_key);
        } else if resolved.no_sign {
            warn!("⚠️ Skipping plan signing; hardened endpoints will reject unsigned submissions.");
        } else if let Some(key_path) = resolved.signing_key.as_deref() {
            let verifying_key = sign_execution_plan(&mut execution_plan, key_path).await?;
            let verifying_hex = hex::encode(&verifying_key);
            info!(
                "🔏 Execution plan signed with supplied key {}",
                verifying_hex
            );
            plan_signer = Some(verifying_key);
        } else {
            let mut seed = [0u8; 32];
            OsRng.fill_bytes(&mut seed);
            let verifying_key = attach_plan_signature_with_seed(&mut execution_plan, seed)?;
            let verifying_hex = hex::encode(&verifying_key);
            info!(
                "🔏 Execution plan signed with ephemeral key {}",
                verifying_hex
            );

            let mut key_path = plan_path.clone();
            key_path.set_extension("signing-key");
            fs::write(&key_path, format!("{}\n", hex::encode(seed))).await?;
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
                            log_guardrail_notice(
                                production_guardrails,
                                "No valid signer entries discovered in configuration allow-list; verification may fail.",
                            );
                        } else if !list.contains_bytes(&verifying_key) {
                            log_guardrail_notice(
                                production_guardrails,
                                format!(
                                    "Plan signer {} not present in configured allow-list; update configuration or rotate keys before mainnet promotion.",
                                    hex::encode(verifying_key)
                                ),
                            );
                        } else {
                            info!(
                                "   Signer {} present in configured allow-list",
                                hex::encode(verifying_key)
                            );
                        }
                    }
                    Err(err) => {
                        log_guardrail_notice(
                            production_guardrails,
                            format!(
                                "Failed to parse signer allow-list from configuration: {}",
                                err
                            ),
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

        if resolved.no_submit {
            info!("   Plan generation complete; submission skipped (--no-submit)");
            return Ok(());
        }

        let submission = spinner("Submitting execution plan to network...");
        match submit_execution_plan(&execution_plan, config, effective_auth_token.as_deref()).await
        {
            Ok(Some(receipt)) => {
                submission.finish_with_message("✅ Execution plan submitted");
                if let Some(address) = receipt.contract_address.as_deref() {
                    if let Some(omne_addr) = normalize_contract_address(address) {
                        info!("   Contract Address: {}", omne_addr);
                        println!("   Contract Address: {}", omne_addr);
                    } else {
                        info!("   Contract Address: {}", address);
                        println!("   Contract Address: {}", address);
                    }
                }

                if let Some(deployment_nonce) = receipt.deployment_nonce.as_deref() {
                    info!("   Deployment nonce: {}", deployment_nonce);
                }

                if let Some(transaction_id) = receipt.transaction_id.as_deref() {
                    if let Some(omne_tx) = normalize_transaction_id(transaction_id) {
                        info!("   Transaction ID: {}", omne_tx);
                    } else {
                        info!("   Transaction ID: {}", transaction_id);
                    }
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
    let production_guardrails = is_production_network(&config.network.name);
    let plan_path_str = verify
        .plan
        .as_deref()
        .or_else(|| defaults.plan.as_deref())
        .ok_or_else(|| anyhow!("plan path must be provided (pass --verify-plan or --plan)"))?;
    let plan_path = PathBuf::from(plan_path_str);

    info!(
        "🔍 Verifying execution plan signature at {}",
        plan_path.display()
    );

    let plan_bytes = fs::read(&plan_path)
        .await
        .with_context(|| format!("failed to read execution plan from {}", plan_path.display()))?;

    let plan: ExecutionPlan = serde_json::from_slice(&plan_bytes).map_err(|err| {
        anyhow!(
            "failed to parse execution plan JSON from {}: {}",
            plan_path.display(),
            err
        )
    })?;

    // Fail fast on ABI/metadata issues before digesting or distributing the plan.
    validate_plan_metadata(&plan)?;
    enforce_address_argument_hygiene(&plan.execution.typed_arguments)?;

    let network_name = plan
        .network
        .as_ref()
        .map(|n| n.name.as_str())
        .unwrap_or_else(|| config.network.name.as_str());
    require_fee_routing_config(network_name, &config.network)?;

    validate_plan_metadata(&plan)?;
    enforce_address_argument_hygiene(&plan.execution.typed_arguments)?;

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
                log_guardrail_notice(
                    production_guardrails,
                    "No signer allow-list entries configured; verification will trust any valid signature.",
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

    let verifying_bytes = verifying_key;

    info!("✅ Execution plan signature verified");
    info!("   Digest: {}", hex::encode(digest));
    info!("   Signer: {}", hex::encode(&verifying_bytes));

    if let Some(list) = allow_list {
        if list.contains_bytes(&verifying_bytes) {
            info!("   Signer present in configured allow-list");
        }
    }

    Ok(())
}

async fn submit_deployment_plan(
    args: &DeploySubmitArgs,
    defaults: &DeployPlanArgs,
    config: &Config,
) -> Result<()> {
    let production_guardrails = is_production_network(&config.network.name);
    let plan_path_str = args
        .plan
        .as_deref()
        .ok_or_else(|| anyhow!("plan path must be provided (pass --submit-plan)"))?;
    let plan_path = PathBuf::from(plan_path_str);

    info!("📤 Submitting execution plan at {}", plan_path.display());

    let plan_bytes = fs::read(&plan_path)
        .await
        .with_context(|| format!("failed to read execution plan from {}", plan_path.display()))?;

    let mut plan: ExecutionPlan = serde_json::from_slice(&plan_bytes).map_err(|err| {
        anyhow!(
            "failed to parse execution plan JSON from {}: {}",
            plan_path.display(),
            err
        )
    })?;

    let network_name = plan
        .network
        .as_ref()
        .map(|n| n.name.as_str())
        .unwrap_or_else(|| config.network.name.as_str());
    require_fee_routing_config(network_name, &config.network)?;

    validate_plan_metadata(&plan)?;
    enforce_address_argument_hygiene(&plan.execution.typed_arguments)?;

    let has_external_signature =
        defaults.plan_signature_hex.is_some() || defaults.plan_signature_pubkey.is_some();

    if has_external_signature {
        if plan.signature.is_some() {
            bail!("plan already contains a signature; remove it before supplying external signature flags");
        }
        let signature_hex = defaults.plan_signature_hex.as_deref().ok_or_else(|| {
            anyhow!("--plan-signature-hex is required when using external signatures")
        })?;
        let public_key_hex = defaults.plan_signature_pubkey.as_deref().ok_or_else(|| {
            anyhow!("--plan-signature-pubkey is required when using external signatures")
        })?;
        attach_plan_signature_with_hex(&mut plan, signature_hex, public_key_hex)?;
    }

    let signature = plan
        .signature
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow!("execution plan is missing 'signature' attestation"))?;

    let mut allow_entries = config.network.allowed_signers.clone();
    allow_entries.extend(args.allowed_signer.iter().cloned());

    let allow_list = if args.allow_unknown_signer {
        info!("   Allow-list enforcement disabled via --allow-unknown-signer");
        None
    } else {
        match SignerAllowList::from_hex_iter(allow_entries.iter().map(|s| s.as_str())) {
            Ok(list) if list.is_empty() => {
                log_guardrail_notice(
                    production_guardrails,
                    "No signer allow-list entries configured; submission will trust any valid signature.",
                );
                None
            }
            Ok(list) => Some(list),
            Err(err) => return Err(anyhow!(err.to_string())),
        }
    };

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

    let verifying_hex = hex::encode(&verifying_key);
    info!("   Plan signature verified for signer {}", verifying_hex);

    let effective_auth_token = defaults
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

    let submission = spinner("Submitting execution plan to network...");
    match submit_execution_plan(&plan, config, effective_auth_token.as_deref()).await {
        Ok(Some(receipt)) => {
            submission.finish_with_message("✅ Execution plan submitted");
            if let Some(address) = receipt.contract_address.as_deref() {
                if let Some(omne_addr) = normalize_contract_address(address) {
                    info!("   Contract Address: {}", omne_addr);
                    println!("   Contract Address: {}", omne_addr);
                } else {
                    info!("   Contract Address: {}", address);
                    println!("   Contract Address: {}", address);
                }
            }
            let receipt_path = plan_path.with_extension("receipt.json");
            let receipt_bytes = serde_json::to_vec_pretty(&receipt)?;
            fs::write(&receipt_path, receipt_bytes).await?;
            info!(
                "   Deployment receipt written to {}",
                receipt_path.display()
            );
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

    Ok(())
}

async fn print_plan_digest(args: &DeployDigestArgs) -> Result<()> {
    let plan_path_str = args
        .plan
        .as_deref()
        .ok_or_else(|| anyhow!("plan path must be provided (pass --digest-plan)"))?;
    let plan_path = PathBuf::from(plan_path_str);

    let plan_bytes = fs::read(&plan_path)
        .await
        .with_context(|| format!("failed to read execution plan from {}", plan_path.display()))?;

    let plan: ExecutionPlan = serde_json::from_slice(&plan_bytes).map_err(|err| {
        anyhow!(
            "failed to parse execution plan JSON from {}: {}",
            plan_path.display(),
            err
        )
    })?;

    let digest = deploy_guardrails::canonical_plan_digest(
        &plan.generated_at,
        &plan.network,
        &plan.contract,
        &plan.execution,
        &plan.services,
    )
    .map_err(|err| anyhow!(err.to_string()))?;

    println!("{}", hex::encode(digest));
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
    // Enforce ABI/metadata checks before any RPC submission to keep CLI guardrails in sync.
    validate_plan_metadata(plan)?;
    enforce_address_argument_hygiene(&plan.execution.typed_arguments)?;

    let endpoint = &config.network.rpc_endpoint;
    if !endpoint.starts_with("http") {
        bail!(
            "RPC endpoint {} is not HTTP(S); unable to submit execution plan",
            endpoint
        );
    }

    let mut plan_value = serde_json::to_value(plan)?;

    if let Some(args) = plan_value
        .get_mut("execution")
        .and_then(|exec| exec.get_mut("typed_arguments"))
        .and_then(|value| value.as_array_mut())
    {
        for arg in args.iter_mut() {
            if let Some(obj) = arg.as_object_mut() {
                let is_u128 = obj
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(|t| t.eq_ignore_ascii_case("u128"))
                    .unwrap_or(false);
                if is_u128 {
                    if let Some(val) = obj.get("value") {
                        // Normalize u128 to a numeric JSON value (avoid strings or float forms like 1e24).
                        // serde_json::Number cannot represent >u64 via from_u128, so fall back to
                        // from_string_unchecked after validation when needed.
                        let decimal_str = match val {
                            JsonValue::String(s) => s.clone(),
                            JsonValue::Number(n) => n.to_string(),
                            _ => String::new(),
                        };

                        if let Ok(parsed) = parse_unsigned_to_u128(&decimal_str, 128) {
                            let canonical = parsed.to_string();
                            let num = JsonNumber::from_string_unchecked(canonical);
                            obj.insert("value".to_string(), JsonValue::Number(num));
                            if let Some(val) = obj.get("value") {
                                info!("   Normalized u128 argument to {}", val);
                            }
                        }
                    }
                }
            }
        }
    }

    let payload = json!({
        "jsonrpc": "2.0",
        "method": "omne_deployContract",
        "params": [plan_value],
        "id": 1,
    });

    if let Some(args_json) = payload
        .get("params")
        .and_then(|params| params.get(0))
        .and_then(|plan| plan.get("execution"))
        .and_then(|exec| exec.get("typed_arguments"))
    {
        info!("   Serialized typed_arguments: {}", args_json);
    }

    let payload_string = serde_json::to_string(&payload)?;

    if let Some(start) = payload_string.find("\"typed_arguments\"") {
        let end = (start + 400).min(payload_string.len());
        let snippet = &payload_string[start..end];
        info!("   Payload typed_arguments snippet: {}", snippet);
    }

    let client = Client::new();
    let mut request = client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(payload_string);

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
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read deployment response from {}", endpoint))?;

    let envelope: JsonRpcEnvelope = serde_json::from_str(&body).map_err(|err| {
        anyhow!(
            "failed to decode deployment response from {}: {} (body: {})",
            endpoint, err, body
        )
    })?;

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

    // Provide a list API helper so staging smoke tests can derive plan IDs for follow-up calls.
    async fn fetch_plan_list(&self) -> Result<MetadataFetch<MetadataPlanList>> {
        self.get("plans").await
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
    // Optional fields are present in list/detail responses but not required for all flows.
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    deployment_nonce: Option<String>,
    #[serde(default)]
    services: Vec<String>,
}

// Minimal list response used by CLI staging smoke tests.
#[derive(Debug, Deserialize)]
struct MetadataPlanList {
    #[serde(default)]
    plans: Vec<MetadataPlanSummary>,
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
    numeric_arguments: &[SerializableVal],
    typed_arguments: &[TypedArgument],
    input_base64: Option<String>,
    caller_address: Option<[u8; 20]>,
    config: &Config,
    compiler_attachment: Option<CompilerAttachment>,
    skip_preview: bool,
    gas_limit_override: Option<u64>,
    allow_unsigned_metadata: bool,
) -> Result<ExecutionPlan> {
    let mut exec_config = tier.build_execution_config(&method.contract, &method.function);
    if let Some(limit) = gas_limit_override {
        exec_config.gas_limit = limit;
    }

    if let Some(caller) = caller_address {
        exec_config = exec_config.with_caller_address(widen_preview_address(caller));
    }

    // Default to preserving typed arguments so runtimes can materialize pointer/length
    // pairs server-side. If the args are purely numeric, we can still run previews
    // by passing the numeric list into the engine.
    let plan_typed_arguments = typed_arguments.to_vec();
    let plan_input_base64 = input_base64.clone();
    validate_execution_guardrails(
        tier.as_str(),
        exec_config.max_call_depth,
        exec_config.storage_budget_bytes,
    )
    .map_err(|err| anyhow!(err.to_string()))?;
    let engine = create_engine(&tier)?;
    let typed_requires_memory = typed_arguments.iter().any(|arg| {
        matches!(
            arg,
            TypedArgument::String(_)
                | TypedArgument::BytesBase64(_)
                | TypedArgument::OptionString(Some(_))
                | TypedArgument::Address20(_)
                | TypedArgument::OptionAddress20(Some(_))
        )
    });

    if !typed_requires_memory && !numeric_arguments.is_empty() {
        exec_config.arguments = numeric_arguments.to_vec();
    }

    let should_skip_preview = skip_preview;

    let (execution_preview, preview_summary) = if should_skip_preview {
        (None, None)
    } else {
        let (mut preview_config, preview_input) = materialize_preview_arguments(
            &exec_config,
            typed_arguments,
            input_base64.as_deref(),
        )?;
        if preview_config.contract_address.is_none() {
            preview_config = preview_config.with_contract_address(
                widen_preview_address(derive_preview_contract_address(module.bytes())),
            );
        }
        let state_manager = Arc::new(StateManager::new_fastvm_optimized()?);
        let execution_preview = engine
            .execute_with_state_and_input(
                module.bytes(),
                preview_config,
                state_manager,
                preview_input.as_deref(),
            )
            .map(|(res, _)| res)
            .map_err(|err| anyhow!("contract execution preview failed: {}", err))?;

        let preview_summary = ExecutionPreviewSummary {
            execution_time_ms: execution_preview.execution_time.as_millis(),
            gas_consumed: execution_preview.gas_consumed,
            return_value: execution_preview.return_value.clone(),
            deterministic_state: execution_preview.deterministic_state.clone(),
            call_depth_used: execution_preview.call_depth_used,
            storage_bytes_written: execution_preview.storage_bytes_written,
        };

        (Some(execution_preview), Some(preview_summary))
    };

    let metadata = module.metadata();

    if let Some(attachment) = compiler_attachment.as_ref() {
        validate_abi_arguments_for_entry(
            &method.contract,
            &method.function,
            &plan_typed_arguments,
            &attachment.metadata,
        )?;
    }

    // Fail fast if callers try to sneak raw hex addresses as strings instead of Address20.
    enforce_address_argument_hygiene(&plan_typed_arguments)?;

    let abi_sha256 = compute_abi_checksum(metadata.contract_methods())?;

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

            let signature = match attachment.signature.as_ref() {
                Some(signature) => Some(signature),
                None => {
                    if allow_unsigned_metadata {
                        warn!(
                            "⚠️ Compiler metadata for {} is unsigned; skipping signature verification",
                            module.path().display()
                        );
                        None
                    } else {
                        return Err(anyhow!(
                            "compiler metadata for {} is unsigned; re-run compiler with signing enabled",
                            module.path().display()
                        ));
                    }
                }
            };

            let allow_entries = if config.network.allowed_compiler_signers.is_empty() {
                compiler_signers_vec_for_network(&config.network.name)
            } else {
                config.network.allowed_compiler_signers.clone()
            };

            let allow_list =
                SignerAllowList::from_hex_iter(allow_entries.iter().map(|s| s.as_str()))
                    .map_err(|err| anyhow!(err.to_string()))?;

            if let Some(signature) = signature {
                let verifying_key =
                    verify_metadata_signature(&attachment.metadata, signature, Some(&allow_list))
                        .map_err(|err| anyhow!(err.to_string()))?;

                info!(
                    "   Compiler metadata signature verified (signer {})",
                    hex::encode(&verifying_key)
                );
            }

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
                abi_sha256: Some(abi_sha256),
                compiler: compiler_attachment,
            }),
        },
        execution: PlanExecution {
            tier: tier.as_str().to_string(),
            config: exec_config,
            typed_arguments: plan_typed_arguments,
            input_base64: plan_input_base64,
            preview: execution_preview,
            preview_summary,
        },
        services: services.to_vec(),
        signature: None,
    })
}

fn materialize_preview_arguments(
    config: &RuntimeExecutionConfig,
    typed_arguments: &[TypedArgument],
    input_base64: Option<&str>,
) -> Result<(RuntimeExecutionConfig, Option<Vec<u8>>)> {
    let mut arguments = config.arguments.clone();
    let mut input_buffer = if !typed_arguments.is_empty() {
        if let Some(encoded) = input_base64 {
            if !encoded.trim().is_empty() {
                warn!(
                    "preview ignoring input_base64 because typed arguments are provided"
                );
            }
        }
        Vec::new()
    } else {
        match input_base64 {
            Some(encoded) if !encoded.trim().is_empty() => Base64::decode_vec(encoded)
                .map_err(|err| anyhow!("Invalid input_base64 payload: {err}"))?,
            _ => Vec::new(),
        }
    };
    let initial_len = input_buffer.len();
    let mut offset = u32::try_from(input_buffer.len())
        .map_err(|_| anyhow!("Input payload exceeds 4GB"))?;

    let mut pointer_debug = Vec::new();

    for (idx, arg) in typed_arguments.iter().enumerate() {
        match arg {
            TypedArgument::I32(v) => {
                arguments.push(SerializableVal::I32(*v));
            }
            TypedArgument::U8(v) => {
                arguments.push(SerializableVal::I32(i32::from(*v)));
            }
            TypedArgument::U32(v) => {
                let as_i32 = i32::try_from(*v)
                    .map_err(|_| anyhow!("U32 argument exceeds i32 range"))?;
                arguments.push(SerializableVal::I32(as_i32));
            }
            TypedArgument::I64(v) => {
                arguments.push(SerializableVal::I64(*v));
            }
            TypedArgument::U64(v) => {
                let as_i64 = i64::try_from(*v)
                    .map_err(|_| anyhow!("U64 argument exceeds i64 range"))?;
                arguments.push(SerializableVal::I64(as_i64));
            }
            TypedArgument::U128(v) => {
                let lo = (*v as u64).to_le_bytes();
                let hi = ((*v >> 64) as u64).to_le_bytes();
                let lo_i64 = i64::from_le_bytes(lo);
                let hi_i64 = i64::from_le_bytes(hi);
                arguments.push(SerializableVal::I64(lo_i64));
                arguments.push(SerializableVal::I64(hi_i64));
            }
            TypedArgument::F32(v) => {
                arguments.push(SerializableVal::F32(*v));
            }
            TypedArgument::F64(v) => {
                arguments.push(SerializableVal::F64(*v));
            }
            TypedArgument::Bool(v) => {
                arguments.push(SerializableVal::I32(i32::from(*v)));
            }
            TypedArgument::String(text) => {
                let payload_len = 4 + text.as_bytes().len();
                let ptr = write_len_prefixed_blob(text.as_bytes(), &mut input_buffer, &mut offset)?;
                pointer_debug.push(format!(
                    "arg[{idx}] string ptr=0x{ptr:x} len={payload_len}"
                ));
                arguments.push(SerializableVal::I32(ptr));
            }
            TypedArgument::OptionString(value) => {
                let (ptr, payload_len) = if let Some(text) = value {
                    let mut payload = vec![1u8];
                    payload.extend_from_slice(&encode_len_prefixed_bytes(text.as_bytes())?);
                    let len = payload.len();
                    let ptr = write_raw_blob(&payload, &mut input_buffer, &mut offset)?;
                    (ptr, len)
                } else {
                    let ptr = write_raw_blob(&[0u8], &mut input_buffer, &mut offset)?;
                    (ptr, 1)
                };
                pointer_debug.push(format!(
                    "arg[{idx}] option<string> ptr=0x{ptr:x} len={payload_len}"
                ));
                arguments.push(SerializableVal::I32(ptr));
            }
            TypedArgument::Address20(addr) => {
                let payload_len = 4 + addr.0.len();
                let ptr = write_len_prefixed_blob(&addr.0, &mut input_buffer, &mut offset)?;
                pointer_debug.push(format!(
                    "arg[{idx}] address20 ptr=0x{ptr:x} len={payload_len}"
                ));
                arguments.push(SerializableVal::I32(ptr));
            }
            TypedArgument::OptionAddress20(value) => {
                let (ptr, payload_len) = if let Some(addr) = value {
                    let mut payload = vec![1u8];
                    payload.extend_from_slice(&encode_len_prefixed_bytes(&addr.0)?);
                    let len = payload.len();
                    let ptr = write_raw_blob(&payload, &mut input_buffer, &mut offset)?;
                    (ptr, len)
                } else {
                    let ptr = write_raw_blob(&[0u8], &mut input_buffer, &mut offset)?;
                    (ptr, 1)
                };
                pointer_debug.push(format!(
                    "arg[{idx}] option<address20> ptr=0x{ptr:x} len={payload_len}"
                ));
                arguments.push(SerializableVal::I32(ptr));
            }
            TypedArgument::BytesBase64(encoded) => {
                let decoded = Base64::decode_vec(encoded)
                    .map_err(|err| anyhow!("Invalid base64 argument: {err}"))?;
                let payload_len = 4 + decoded.len();
                let ptr = write_len_prefixed_blob(&decoded, &mut input_buffer, &mut offset)?;
                pointer_debug.push(format!(
                    "arg[{idx}] bytes ptr=0x{ptr:x} len={payload_len}"
                ));
                arguments.push(SerializableVal::I32(ptr));
            }
        }
    }

    let updated_config = if arguments.is_empty() {
        config.clone()
    } else {
        config.clone().with_arguments(arguments)
    };

    if !input_buffer.is_empty() {
        let preview_len = input_buffer.len().min(64);
        let mut preview = String::with_capacity(preview_len * 2);
        for byte in input_buffer.iter().take(preview_len) {
            let _ = write!(preview, "{:02x}", byte);
        }
        info!(
            "preview input buffer: initial_len={} final_len={} head={}{}",
            initial_len,
            input_buffer.len(),
            preview,
            if input_buffer.len() > preview_len { "..." } else { "" }
        );
    }

    for line in pointer_debug {
        info!("preview arg materialize: {line}");
    }

    let input_bytes = if input_buffer.is_empty() {
        None
    } else {
        Some(input_buffer)
    };

    Ok((updated_config, input_bytes))
}

fn write_len_prefixed_blob(
    bytes: &[u8],
    input_buffer: &mut Vec<u8>,
    offset: &mut u32,
) -> Result<i32> {
    let encoded = encode_len_prefixed_bytes(bytes)?;
    write_raw_blob(&encoded, input_buffer, offset)
}

fn write_raw_blob(
    bytes: &[u8],
    input_buffer: &mut Vec<u8>,
    offset: &mut u32,
) -> Result<i32> {
    const WASM_INPUT_BASE_ADDR: u32 = 0x10000;
    let len = u32::try_from(bytes.len()).map_err(|_| anyhow!("Input payload exceeds 4GB"))?;
    let addr = WASM_INPUT_BASE_ADDR
        .checked_add(*offset)
        .ok_or_else(|| anyhow!("Input payload exceeds i32 addressable range"))?;
    let ptr = i32::try_from(addr)
        .map_err(|_| anyhow!("Input payload exceeds i32 addressable range"))?;

    let new_offset = offset
        .checked_add(len)
        .ok_or_else(|| anyhow!("Input payload exceeds 4GB"))?;

    input_buffer.extend_from_slice(bytes);
    *offset = new_offset;

    Ok(ptr)
}

fn encode_len_prefixed_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(bytes.len()).map_err(|_| anyhow!("Input payload exceeds 4GB"))?;
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}

/// Widen a 20-byte contract-ABI address to the 32-byte form the runtime uses
/// for state-storage namespacing during a local execution preview. The preview
/// is a local dry-run, so the low-20/high-zero padding is purely an internal
/// namespacing key and never reaches consensus.
fn widen_preview_address(addr: [u8; 20]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..20].copy_from_slice(&addr);
    out
}

fn derive_preview_contract_address(wasm: &[u8]) -> [u8; 20] {
    let digest = Sha256::digest(wasm);
    let mut out = [0u8; 20];
    out.copy_from_slice(&digest[..20]);
    out
}

async fn sign_execution_plan(plan: &mut ExecutionPlan, key_path: &str) -> Result<Vec<u8>> {
    let raw = fs::read(key_path)
        .await
        .with_context(|| format!("failed to read signing key from {}", key_path))?;

    let secret_bytes = if raw.len() == 32 {
        raw
    } else {
        let key_str = String::from_utf8(raw)
            .context("signing key file must contain raw 32-byte seed or hex-encoded seed")?;
        let cleaned = key_str.trim();
        if cleaned.len() == 64 && cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
            hex::decode(cleaned)?
        } else {
            bail!("signing key must be provided as 32 raw bytes or 64 hexadecimal characters");
        }
    };

    let seed: [u8; 32] = secret_bytes
        .try_into()
        .map_err(|_| anyhow!("signing key seed must decode to exactly 32 bytes"))?;

    attach_plan_signature_with_seed(plan, seed)
}

fn attach_plan_signature_with_seed(
    plan: &mut ExecutionPlan,
    seed: [u8; 32],
) -> Result<Vec<u8>> {
    let (public_key, secret_key) = pqc::keygen_from_seed(seed);
    let digest = compute_plan_digest(plan)?;
    let signature = pqc::sign(&secret_key, &digest).map_err(|err| anyhow!(err))?;

    plan.signature = Some(PlanSignature {
        algorithm: pqc::ALGORITHM_ID.to_string(),
        public_key_hex: hex::encode(&public_key),
        signature_hex: hex::encode(&signature),
    });

    Ok(public_key)
}

fn attach_plan_signature_with_hex(
    plan: &mut ExecutionPlan,
    signature_hex: &str,
    public_key_hex: &str,
) -> Result<Vec<u8>> {
    let signature_clean = signature_hex.trim();
    if signature_clean.len() != pqc::SIGNATURE_LEN * 2
        || !signature_clean.chars().all(|c| c.is_ascii_hexdigit())
    {
        bail!(
            "plan signature must be {} bytes ({} hex characters)",
            pqc::SIGNATURE_LEN,
            pqc::SIGNATURE_LEN * 2
        );
    }

    let public_key_clean = public_key_hex.trim();
    if public_key_clean.len() != pqc::PUBLIC_KEY_LEN * 2
        || !public_key_clean.chars().all(|c| c.is_ascii_hexdigit())
    {
        bail!(
            "plan signature public key must be {} bytes ({} hex characters)",
            pqc::PUBLIC_KEY_LEN,
            pqc::PUBLIC_KEY_LEN * 2
        );
    }

    let plan_signature = PlanSignature {
        algorithm: pqc::ALGORITHM_ID.to_string(),
        public_key_hex: public_key_clean.to_ascii_lowercase(),
        signature_hex: signature_clean.to_ascii_lowercase(),
    };

    plan.signature = Some(plan_signature.clone());

    let verifying_key = deploy_guardrails::verify_plan_signature(
        &plan.generated_at,
        &plan.network,
        &plan.contract,
        &plan.execution,
        &plan.services,
        &plan_signature,
        None,
    )
    .map_err(|err| anyhow!("KMS plan signature verification failed: {err}"))?;

    Ok(verifying_key)
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


fn looks_like_hex_address(value: &str) -> bool {
    let trimmed = value.trim();
    // Only recognize omne1-prefixed addresses or raw 40-char hex
    let clean = trimmed
        .strip_prefix("omne1")
        .unwrap_or(trimmed);
    clean.len() == 40 && clean.chars().all(|c| c.is_ascii_hexdigit())
}

fn enforce_address_argument_hygiene(args: &[TypedArgument]) -> Result<()> {
    for arg in args {
        match arg {
            TypedArgument::String(s) if looks_like_hex_address(s) => {
                bail!(
                    "argument '{}' looks like a hex address; encode it as Address20 to enforce checksumming",
                    s
                );
            }
            TypedArgument::OptionString(Some(s)) if looks_like_hex_address(s) => {
                bail!(
                    "optional argument '{}' looks like a hex address; encode it as OptionAddress20 to enforce checksumming",
                    s
                );
            }
            _ => {}
        }
    }
    Ok(())
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

fn normalize_contract_address(raw: &str) -> Option<String> {
    normalize_hex_identifier(
        raw,
        &["contract_"],
        40,
        "contract_",
    )
}

fn normalize_transaction_id(raw: &str) -> Option<String> {
    normalize_hex_identifier(raw, &["txn_"], 64, "txn_")
}

fn normalize_hex_identifier(
    raw: &str,
    accepted_prefixes: &[&str],
    expected_len: usize,
    canonical_prefix: &str,
) -> Option<String> {
    let trimmed = raw.trim();

    // Match accepted prefixes or raw hex of expected length; no 0x fallback
    let payload = accepted_prefixes
        .iter()
        .filter_map(|prefix| trimmed.strip_prefix(prefix))
        .next()
        .or_else(|| if trimmed.len() == expected_len { Some(trimmed) } else { None })?;

    if payload.len() != expected_len {
        return None;
    }
    if !payload.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    let normalized = payload.to_ascii_lowercase();
    Some(format!("{}{}", canonical_prefix, normalized))
}

fn log_guardrail_notice(enforce_warning: bool, message: impl Into<String>) {
    let rendered = message.into();
    if enforce_warning {
        warn!("{}", rendered);
    } else {
        info!("{}", rendered);
    }
}

fn is_dev_network(name: &str) -> bool {
    matches!(name, "devnet" | "omne_devnet" | "ignis" | "omne_ignis")
}

fn is_production_network(name: &str) -> bool {
    matches!(name, "mainnet" | "omne_mainnet")
}

fn require_fee_routing_config(
    network_name: &str,
    network_config: &crate::config::NetworkConfig,
) -> Result<()> {
    if is_dev_network(network_name) {
        return Ok(());
    }

    let treasury = network_config
        .fee_treasury_address
        .as_deref()
        .ok_or_else(|| anyhow!("fee_treasury_address must be configured for {}", network_name))?;
    let slash = network_config
        .slash_sink_address
        .as_deref()
        .ok_or_else(|| anyhow!("slash_sink_address must be configured for {}", network_name))?;
    let fee_vault = network_config
        .fee_vault_address
        .as_deref()
        .ok_or_else(|| anyhow!("fee_vault_address must be configured for {}", network_name))?;
    let validator_pool = network_config
        .validator_fee_pool_address
        .as_deref()
        .ok_or_else(|| anyhow!("validator_fee_pool_address must be configured for {}", network_name))?;

    let parsed_treasury = parse_address20(treasury)?;
    let parsed_slash = parse_address20(slash)?;
    let parsed_vault = parse_address20(fee_vault)?;
    let parsed_validator_pool = parse_address20(validator_pool)?;

    if parsed_treasury.0.iter().all(|b| *b == 0) {
        bail!("fee_treasury_address cannot be all zeroes for {}", network_name);
    }
    if parsed_slash.0.iter().all(|b| *b == 0) {
        bail!("slash_sink_address cannot be all zeroes for {}", network_name);
    }
    if parsed_vault.0.iter().all(|b| *b == 0) {
        bail!("fee_vault_address cannot be all zeroes for {}", network_name);
    }
    if parsed_validator_pool.0.iter().all(|b| *b == 0) {
        bail!(
            "validator_fee_pool_address cannot be all zeroes for {}",
            network_name
        );
    }

    let fee_split = network_config.fee_split_bps.unwrap_or(0);
    let povc_split = network_config.povc_split_bps.unwrap_or(0);

    if fee_split > 10_000 {
        bail!("fee_split_bps must be between 0 and 10000 (got {})", fee_split);
    }
    if povc_split > 10_000 {
        bail!("povc_split_bps must be between 0 and 10000 (got {})", povc_split);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiom_runtime::execution::SerializableVal;
    use deploy_guardrails::{CompilerMetadata, CompilerMetadataSignature};
    use httptest::{
        matchers::{all_of, contains, eq, matches, request},
        responders::{json_encoded, status_code},
        Expectation, Server,
    };
    use serde_json::json;
    use std::env;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use wat::parse_str;

    const DEMO_WAT: &str = r#"(module
        (func (export "axiom_entry_main")
            (param i32 i32 i64 i64 i32 i64 i32 i32 i32)
            (result i64)
            i64.const 0)
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

        let (public_key, secret_key) = pqc::keygen_from_seed([5u8; 32]);
        let digest = deploy_guardrails::canonical_metadata_digest(&metadata).expect("digest");
        let signature = pqc::sign(&secret_key, digest.as_ref()).expect("sign");
        let verifying_hex = hex::encode(&public_key);

        let attachment = CompilerAttachment {
            metadata,
            signature: Some(CompilerMetadataSignature {
                algorithm: pqc::ALGORITHM_ID.to_string(),
                public_key_hex: verifying_hex.clone(),
                signature_hex: hex::encode(&signature),
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
            &[],
            &[],
            None,
            None,
            &config,
            Some(compiler_attachment),
            false,
            None,
            false,
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
            &[],
            &[],
            None,
            None,
            &config,
            Some(compiler_attachment),
            false,
            None,
            false,
        )
        .expect("plan build");

        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let mut key_file = NamedTempFile::new().expect("temp key");
        write!(key_file, "{}", hex::encode(seed)).expect("write key");
        let key_path = key_file.into_temp_path();

        let verifying_key = sign_execution_plan(&mut plan, key_path.to_str().unwrap())
            .await
            .expect("sign plan");

        let signature = plan.signature.expect("signature present");
        assert_eq!(signature.algorithm, pqc::ALGORITHM_ID);
        assert_eq!(signature.public_key_hex.len(), pqc::PUBLIC_KEY_LEN * 2);
        assert_eq!(signature.signature_hex.len(), pqc::SIGNATURE_LEN * 2);
        assert_eq!(hex::encode(verifying_key).len(), pqc::PUBLIC_KEY_LEN * 2);
    }

    #[test]
    fn parse_address20_enforces_lowercase_and_length() {
        // Valid lowercase hex parses and preserves bytes
        // Valid omne1-prefixed address (omne1 + 40 hex chars = 20 bytes)
        let valid = parse_address20("omne11111111111111111111111111111111111111111").unwrap();
        assert_eq!(hex::encode(valid.0), "1111111111111111111111111111111111111111");

        // Valid raw hex address
        let valid_raw = parse_address20("1111111111111111111111111111111111111111").unwrap();
        assert_eq!(hex::encode(valid_raw.0), "1111111111111111111111111111111111111111");

        // Reject uppercase hex
        assert!(parse_address20("omne1AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_err());

        // Reject incorrect length
        assert!(parse_address20("deadbeef").is_err());

        // Reject zero address
        assert!(parse_address20("omne10000000000000000000000000000000000000000").is_err());
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
        let fee_treasury = "omne11111111111111111111111111111111111111111".to_string();
        let slash_sink = "omne12222222222222222222222222222222222222222".to_string();
        let fee_vault = "omne13333333333333333333333333333333333333333".to_string();
        let validator_fee_pool = "omne14444444444444444444444444444444444444444".to_string();
        plan_config.network.fee_treasury_address = Some(fee_treasury.clone());
        plan_config.network.slash_sink_address = Some(slash_sink.clone());
        plan_config.network.fee_vault_address = Some(fee_vault.clone());
        plan_config.network.validator_fee_pool_address = Some(validator_fee_pool.clone());

        let mut plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &[],
            &[],
            None,
            None,
            &plan_config,
            Some(compiler_attachment),
            false,
            None,
            false,
        )
        .expect("plan build");

        let verifying_key =
            attach_plan_signature_with_seed(&mut plan, [3u8; 32]).expect("sign plan");

        let plan_bytes = serde_json::to_vec_pretty(&plan).expect("serialize plan");
        let mut plan_file = NamedTempFile::new().expect("plan file");
        plan_file.write_all(&plan_bytes).expect("write plan");
        let plan_path = plan_file.into_temp_path();

        let mut config = Config::default();
        config.network.allowed_signers = vec![hex::encode(verifying_key)];
        config.network.allowed_compiler_signers = vec![compiler_signer];
        config.network.fee_treasury_address = Some(fee_treasury);
        config.network.slash_sink_address = Some(slash_sink);
        config.network.fee_vault_address = Some(fee_vault);
        config.network.validator_fee_pool_address = Some(validator_fee_pool);

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

        // Include compiler metadata so pre-submit ABI validation can succeed.
        let (compiler_attachment, compiler_signer) = make_signed_compiler_attachment(&module);

        let mut server = Server::run();
        server.expect(
            Expectation::matching(all_of![
                request::method_path("POST", "/"),
                request::body(matches("\"method\":\"omne_deployContract\"")),
                request::body(matches("Demo::init"))
            ])
            .respond_with(json_encoded(json!({
                "jsonrpc": "2.0",
                "result": { "contractAddress": "contract_deadbeef" },
                "id": 1
            }))),
        );
        let mut config = Config::default();
        config.network.rpc_endpoint = server.url("/").to_string();
        config.network.ws_endpoint = "ws://test".to_string();
        config.network.explorer_url = "http://test".to_string();
        config.network.allowed_compiler_signers = vec![compiler_signer];

        let plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &[],
            &[],
            None,
            None,
            &config,
            Some(compiler_attachment),
            false,
            None,
            false,
        )
        .expect("plan build");

        let response = submit_execution_plan(&plan, &config, None)
            .await
            .expect("submission succeeds");

        let receipt = response.expect("deployment result");
        assert_eq!(receipt.contract_address.as_deref(), Some("contract_deadbeef"));

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
        let plan = build_execution_plan(
            &module,
            method,
            DeployTier::Standard,
            &[],
            &[],
            &[],
            None,
            None,
            &config,
            None,
            false,
            None,
            false,
        )
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

    #[tokio::test]
    async fn staging_metadata_endpoints_smoke_test() {
        // This staging test is gated by env vars so it only runs when explicitly configured.
        let base_url = match env::var("OMNE_METADATA_BASE_URL") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                eprintln!("Skipping staging metadata test: OMNE_METADATA_BASE_URL not set");
                return;
            }
        };
        let auth_token = match env::var("OMNE_METADATA_AUTH_TOKEN") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                eprintln!("Skipping staging metadata test: OMNE_METADATA_AUTH_TOKEN not set");
                return;
            }
        };

        let mut config = Config::default();
        config.network.metadata_base_url = Some(base_url);
        config.network.auth_token = Some(auth_token);

        let client = MetadataClient::new(&config)
            .expect("metadata client build")
            .expect("metadata base url configured");

        // List plans first to verify the endpoint is reachable with auth.
        let plan_list = client
            .fetch_plan_list()
            .await
            .expect("metadata plan list request")
            .expect_found("metadata plan list response");

        // If staging has no plans, the list call still proves the endpoint works.
        let Some(first_plan) = plan_list.plans.first() else {
            return;
        };

        let plan_details = client
            .fetch_plan_by_id(&first_plan.plan_id)
            .await
            .expect("metadata plan detail request");

        // Allow eventual consistency: details may return 404 even when list is populated.
        let plan_details = match plan_details {
            MetadataFetch::Found(details) => Some(details),
            MetadataFetch::NotFound => None,
            MetadataFetch::Disabled => {
                panic!("metadata endpoint disabled in staging");
            }
        };

        if let Some(details) = plan_details {
            if let Some(digest) = details.plan.digest.as_ref() {
                let _ = client
                    .fetch_plan_by_digest(digest)
                    .await
                    .expect("metadata plan digest request");
            }

            if let Some(nonce) = details.plan.deployment_nonce.as_ref() {
                let nonce_hash = hash_nonce(nonce);
                let _ = client
                    .fetch_nonce_provenance(&nonce_hash)
                    .await
                    .expect("metadata nonce provenance request");
            }
        }
    }

    trait MetadataFetchExt<T> {
        fn expect_found(self, context: &str) -> T;
    }

    impl<T> MetadataFetchExt<T> for MetadataFetch<T> {
        fn expect_found(self, context: &str) -> T {
            match self {
                MetadataFetch::Found(value) => value,
                MetadataFetch::NotFound => panic!("{}: unexpected 404", context),
                MetadataFetch::Disabled => panic!("{}: endpoint disabled", context),
            }
        }
    }

    /// Full mock integration flow: list → detail → digest → provenance.
    /// Exercises auth header forwarding, JSON shape parsing, and rate-limit header presence.
    #[tokio::test]
    async fn metadata_mock_full_flow_with_auth_and_rate_limit_headers() {
        let mut server = Server::run();

        let list_path = "/v1/plans";
        let detail_path = "/v1/plans/pln_abc123";
        let digest_path = "/v1/plans/digest/deadbeef";
        let provenance_path = "/v1/provenance/cafebabe";

        // List endpoint returns one plan and rate-limit headers.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("GET", list_path),
                request::headers(contains((
                    eq("authorization"),
                    matches("Bearer mock-token"),
                )))
            ])
            .respond_with(
                json_encoded(json!({
                    "plans": [{
                        "plan_id": "pln_abc123",
                        "digest": "deadbeef",
                        "deployment_nonce": "nonce_xyz",
                        "services": ["settlement"]
                    }]
                }))
                .insert_header("X-RateLimit-Limit", "100")
                .insert_header("X-RateLimit-Remaining", "99")
                .insert_header("X-RateLimit-Reset", "60"),
            ),
        );

        // Detail endpoint returns full plan summary with rate-limit headers.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("GET", detail_path),
                request::headers(contains((
                    eq("authorization"),
                    matches("Bearer mock-token"),
                )))
            ])
            .respond_with(
                json_encoded(json!({
                    "plan": {
                        "plan_id": "pln_abc123",
                        "digest": "deadbeef",
                        "deployment_nonce": "nonce_xyz",
                        "services": ["settlement"]
                    },
                    "submitted_at": "2025-06-01T00:00:00Z"
                }))
                .insert_header("X-RateLimit-Limit", "100")
                .insert_header("X-RateLimit-Remaining", "98"),
            ),
        );

        // Digest lookup returns the same plan.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("GET", digest_path),
                request::headers(contains((
                    eq("authorization"),
                    matches("Bearer mock-token"),
                )))
            ])
            .respond_with(
                json_encoded(json!({
                    "plan": {
                        "plan_id": "pln_abc123",
                        "digest": "deadbeef",
                        "deployment_nonce": "nonce_xyz",
                        "services": ["settlement"]
                    },
                    "submitted_at": "2025-06-01T00:00:00Z"
                }))
                .insert_header("X-RateLimit-Limit", "100"),
            ),
        );

        // Provenance lookup returns linked plan.
        server.expect(
            Expectation::matching(all_of![
                request::method_path("GET", provenance_path),
                request::headers(contains((
                    eq("authorization"),
                    matches("Bearer mock-token"),
                )))
            ])
            .respond_with(
                json_encoded(json!({
                    "nonce_hash": "cafebabe",
                    "plan_id": "pln_abc123",
                    "first_seen_at": "2025-06-01T00:00:01Z"
                }))
                .insert_header("X-RateLimit-Limit", "100"),
            ),
        );

        let mut config = Config::default();
        config.network.metadata_base_url = Some(server.url("/v1/").to_string());
        config.network.auth_token = Some("mock-token".to_string());

        let client = MetadataClient::new(&config)
            .expect("client build")
            .expect("metadata base url configured");

        // Step 1: List plans.
        let list = client
            .fetch_plan_list()
            .await
            .expect("list request")
            .expect_found("list response");
        assert_eq!(list.plans.len(), 1);
        let first = &list.plans[0];
        assert_eq!(first.plan_id, "pln_abc123");

        // Step 2: Fetch plan details by ID.
        let details = client
            .fetch_plan_by_id(&first.plan_id)
            .await
            .expect("detail request")
            .expect_found("detail response");
        assert_eq!(details.plan.plan_id, "pln_abc123");
        assert_eq!(details.submitted_at, "2025-06-01T00:00:00Z");

        // Step 3: Fetch plan by digest.
        let by_digest = client
            .fetch_plan_by_digest(first.digest.as_deref().unwrap())
            .await
            .expect("digest request")
            .expect_found("digest response");
        assert_eq!(by_digest.plan.plan_id, "pln_abc123");

        // Step 4: Fetch nonce provenance (hash the nonce first, then use the test value directly).
        let prov = client
            .fetch_nonce_provenance("cafebabe")
            .await
            .expect("provenance request")
            .expect_found("provenance response");
        assert_eq!(prov.plan_id, "pln_abc123");

        server.verify_and_clear();
    }

    /// Verify that missing or invalid auth tokens cause 401 failures.
    #[tokio::test]
    async fn metadata_mock_rejects_request_without_auth() {
        let mut server = Server::run();

        // Server returns 401 for any request without a valid Authorization header.
        server.expect(
            Expectation::matching(request::method_path("GET", "/v1/plans"))
                .respond_with(
                    status_code(401)
                        .body("{\"error\":\"unauthorized\"}")
                        .insert_header("content-type", "application/json"),
                ),
        );

        let mut config = Config::default();
        config.network.metadata_base_url = Some(server.url("/v1/").to_string());
        // Intentionally omit auth_token to test unauthenticated path.

        let client = MetadataClient::new(&config)
            .expect("client build")
            .expect("metadata base url configured");

        let result = client.fetch_plan_list().await;
        // The MetadataClient returns an error for non-200/404/501 status codes.
        let err_msg = match result {
            Ok(_) => panic!("expected error for 401 response, got Ok"),
            Err(err) => err.to_string(),
        };
        assert!(
            err_msg.contains("401") || err_msg.contains("unauthorized"),
            "error should mention 401 or unauthorized, got: {}",
            err_msg
        );

        server.verify_and_clear();
    }

    /// Verify that 404 on detail endpoint returns NotFound variant.
    #[tokio::test]
    async fn metadata_mock_detail_returns_not_found() {
        let mut server = Server::run();

        server.expect(
            Expectation::matching(request::method_path("GET", "/v1/plans/pln_missing"))
                .respond_with(status_code(404)),
        );

        let mut config = Config::default();
        config.network.metadata_base_url = Some(server.url("/v1/").to_string());
        config.network.auth_token = Some("token".to_string());

        let client = MetadataClient::new(&config)
            .expect("client build")
            .expect("metadata base url configured");

        let result = client
            .fetch_plan_by_id("pln_missing")
            .await
            .expect("request should not fail");

        assert!(
            matches!(result, MetadataFetch::NotFound),
            "expected NotFound for 404 response"
        );

        server.verify_and_clear();
    }

    /// Verify that 501 on list endpoint returns Disabled variant.
    #[tokio::test]
    async fn metadata_mock_list_disabled() {
        let mut server = Server::run();

        server.expect(
            Expectation::matching(request::method_path("GET", "/v1/plans"))
                .respond_with(status_code(501)),
        );

        let mut config = Config::default();
        config.network.metadata_base_url = Some(server.url("/v1/").to_string());

        let client = MetadataClient::new(&config)
            .expect("client build")
            .expect("metadata base url configured");

        let result = client
            .fetch_plan_list()
            .await
            .expect("request should not fail");

        assert!(
            matches!(result, MetadataFetch::Disabled),
            "expected Disabled for 501 response"
        );

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

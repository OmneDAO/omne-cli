use crate::wasm;
use anyhow::{anyhow, bail, Result};
use base64ct::{Base64, Encoding};
use deploy_guardrails::metadata::CompilerContractMethodMetadata;
use deploy_guardrails::{CompilerMetadata, CompilerMetadataSignature, PlanSignatureData};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<PlanNetwork>,
    pub contract: PlanContract,
    pub execution: PlanExecution,
    pub services: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<PlanSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanNetwork {
    pub name: String,
    pub chain_id: u64,
    pub rpc_endpoint: String,
    pub ws_endpoint: String,
    pub explorer_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanContract {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub wasm_size_bytes: usize,
    pub wasm_sha256: String,
    pub wasm_base64: String,
    pub deployment_nonce: String,
    pub entry: PlanEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<PlanContractMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanContractMetadata {
    pub has_axiom_entry_main: bool,
    pub has_legacy_entry_main: bool,
    pub methods: Vec<wasm::ContractMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abi_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler: Option<CompilerAttachment>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CompilerAttachment {
    pub metadata: CompilerMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub signature: Option<CompilerMetadataSignature>,
}

#[derive(Debug, Deserialize)]
pub struct CompilerMetadataEnvelope {
    pub metadata: CompilerMetadata,
    #[serde(default)]
    pub signature: Option<CompilerMetadataSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    pub contract: String,
    pub function: String,
    pub selector: String,
    pub export: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_export: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecution {
    pub tier: String,
    pub config: axiom_runtime::ExecutionConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub typed_arguments: Vec<TypedArgument>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<axiom_runtime::ExecutionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_summary: Option<ExecutionPreviewSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPreviewSummary {
    pub execution_time_ms: u128,
    pub gas_consumed: u64,
    pub return_value: Option<axiom_runtime::execution::SerializableVal>,
    pub deterministic_state: String,
    pub call_depth_used: u32,
    pub storage_bytes_written: u64,
}

pub type PlanSignature = PlanSignatureData;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address20(pub [u8; 20]);

impl Serialize for Address20 {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("omne1{}", hex::encode(self.0)))
    }
}

impl<'de> Deserialize<'de> for Address20 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value: String = Deserialize::deserialize(deserializer)?;
        parse_address20(&value).map_err(|err| de::Error::custom(err.to_string()))
    }
}

pub(crate) fn parse_address20(raw: &str) -> Result<Address20> {
    let trimmed = raw.trim();
    let payload = if let Some(rest) = trimmed.strip_prefix("omne1") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("0x") {
        rest
    } else {
        trimmed
    };

    if payload.len() != 40 {
        bail!("address must encode 20 bytes (found {} hex chars)", payload.len());
    }
    if payload.chars().any(|c| c.is_ascii_uppercase()) {
        bail!("address hex must be lowercase");
    }
    if !payload.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("address must be valid lowercase hex (got: {})", raw);
    }

    let mut out = [0u8; 20];
    hex::decode_to_slice(payload, &mut out)
        .map_err(|_| anyhow!("address must be valid lowercase hex (got: {})", raw))?;
    Ok(Address20(out))
}

fn serialize_u128_as_string<S>(value: &u128, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum TypedArgument {
    I32(i32),
    U8(u8),
    U32(u32),
    I64(i64),
    U64(u64),
    #[serde(serialize_with = "serialize_u128_as_string")]
    U128(u128),
    F32(f32),
    F64(f64),
    Bool(bool),
    String(String),
    BytesBase64(String),
    OptionString(Option<String>),
    Address20(Address20),
    OptionAddress20(Option<Address20>),
}

impl<'de> Deserialize<'de> for TypedArgument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTypedArgument {
            #[serde(rename = "type")]
            ty: String,
            #[serde(default)]
            value: JsonValue,
        }

        let raw = RawTypedArgument::deserialize(deserializer)?;
        let kind = raw.ty.to_lowercase();

        fn parse_via_str<T, F, E>(
            value: JsonValue,
            parser: F,
            desc: &str,
        ) -> Result<T, E>
        where
            F: Fn(&str) -> Result<T, anyhow::Error>,
            E: de::Error,
        {
            match value {
                JsonValue::String(s) => parser(&s).map_err(|err| E::custom(err.to_string())),
                JsonValue::Number(n) => parser(&n.to_string())
                    .map_err(|err| E::custom(err.to_string())),
                other => Err(E::custom(format!(
                    "expected {} as string or number, got {:?}",
                    desc, other
                ))),
            }
        }

        match kind.as_str() {
            "i32" => parse_via_str(raw.value, parse_i32_value, "i32").map(TypedArgument::I32),
            "u8" => parse_via_str(raw.value, |s| parse_unsigned_to_u128(s, 8).map(|v| v as u8), "u8")
                .map(TypedArgument::U8),
            "u32" => parse_via_str(raw.value, |s| parse_unsigned_to_u128(s, 32).map(|v| v as u32), "u32")
                .map(TypedArgument::U32),
            "i64" => parse_via_str(raw.value, |s| parse_signed_int(s, 64).map(|v| v as i64), "i64")
                .map(TypedArgument::I64),
            "u64" => parse_via_str(raw.value, |s| parse_unsigned_to_u128(s, 64).map(|v| v as u64), "u64")
                .map(TypedArgument::U64),
            "u128" => parse_via_str(raw.value, |s| parse_unsigned_to_u128(s, 128), "u128")
                .map(TypedArgument::U128),
            "f32" => match raw.value {
                JsonValue::Number(n) => n
                    .as_f64()
                    .map(|v| v as f32)
                    .ok_or_else(|| de::Error::custom("invalid f32 value"))
                    .map(TypedArgument::F32),
                JsonValue::String(s) => s
                    .parse::<f32>()
                    .map(TypedArgument::F32)
                    .map_err(|err| de::Error::custom(err.to_string())),
                other => Err(de::Error::custom(format!("expected f32 as string or number, got {:?}", other))),
            },
            "f64" => match raw.value {
                JsonValue::Number(n) => n
                    .as_f64()
                    .ok_or_else(|| de::Error::custom("invalid f64 value"))
                    .map(TypedArgument::F64),
                JsonValue::String(s) => s
                    .parse::<f64>()
                    .map(TypedArgument::F64)
                    .map_err(|err| de::Error::custom(err.to_string())),
                other => Err(de::Error::custom(format!("expected f64 as string or number, got {:?}", other))),
            },
            "bool" => match raw.value {
                JsonValue::Bool(v) => Ok(TypedArgument::Bool(v)),
                JsonValue::String(s) => s
                    .parse::<bool>()
                    .map(TypedArgument::Bool)
                    .map_err(|err| de::Error::custom(err.to_string())),
                other => Err(de::Error::custom(format!("expected bool as boolean or string, got {:?}", other))),
            },
            "string" => match raw.value {
                JsonValue::String(s) => Ok(TypedArgument::String(s)),
                other => Err(de::Error::custom(format!("expected string value, got {:?}", other))),
            },
            "bytesbase64" => match raw.value {
                JsonValue::String(s) => Ok(TypedArgument::BytesBase64(s)),
                other => Err(de::Error::custom(format!("expected base64 string value, got {:?}", other))),
            },
            "optionstring" => match raw.value {
                JsonValue::String(s) => Ok(TypedArgument::OptionString(Some(s))),
                JsonValue::Null => Ok(TypedArgument::OptionString(None)),
                other => Err(de::Error::custom(format!("expected string or null for optionstring, got {:?}", other))),
            },
            "address20" => match raw.value {
                JsonValue::String(s) => parse_address20(&s)
                    .map(TypedArgument::Address20)
                    .map_err(|err| de::Error::custom(err.to_string())),
                other => Err(de::Error::custom(format!("expected hex string for address20, got {:?}", other))),
            },
            "optionaddress20" | "option<address20>" => match raw.value {
                JsonValue::Null => Ok(TypedArgument::OptionAddress20(None)),
                JsonValue::String(s) => parse_address20(&s)
                    .map(|addr| TypedArgument::OptionAddress20(Some(addr)))
                    .map_err(|err| de::Error::custom(err.to_string())),
                other => Err(de::Error::custom(format!(
                    "expected hex string or null for optionaddress20, got {:?}",
                    other
                ))),
            },
            other => Err(de::Error::custom(format!("unknown typed argument type: {}", other))),
        }
    }
}

pub(crate) fn parse_i32_value(value: &str) -> Result<i32> {
    let parsed = parse_signed_int(value, 32)?;
    if parsed < i32::MIN as i128 || parsed > i32::MAX as i128 {
        bail!("value {} exceeds i32 range", value);
    }
    Ok(parsed as i32)
}

pub(crate) fn parse_i64_value(value: &str) -> Result<i64> {
    let parsed = parse_signed_int(value, 64)?;
    if parsed < i64::MIN as i128 || parsed > i64::MAX as i128 {
        bail!("value {} exceeds i64 range", value);
    }
    Ok(parsed as i64)
}

pub(crate) fn parse_unsigned_to_u128(value: &str, bits: u32) -> Result<u128> {
    let trimmed = value.trim();
    let cleaned = trimmed.trim_start_matches('+');
    let digits = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
        .unwrap_or(cleaned);
    let radix = if cleaned.starts_with("0x") || cleaned.starts_with("0X") { 16 } else { 10 };
    let parsed = u128::from_str_radix(digits, radix)
        .map_err(|err| anyhow!("failed to parse u{} argument '{}': {}", bits, value, err))?;
    if bits < 128 {
        let limit = 1u128 << bits;
        if parsed >= limit {
            bail!("value {} exceeds u{} range", value, bits);
        }
    }
    Ok(parsed)
}

pub(crate) fn parse_unsigned_to_i128(value: &str, bits: u32) -> Result<i128> {
    let trimmed = value.trim();
    let cleaned = trimmed.trim_start_matches('+');
    let digits = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
        .unwrap_or(cleaned);
    let radix = if cleaned.starts_with("0x") || cleaned.starts_with("0X") {
        16
    } else {
        10
    };
    let parsed = i128::from_str_radix(digits, radix).map_err(|err| {
        anyhow!(
            "failed to parse unsigned {}-bit integer '{}': {}",
            bits,
            value,
            err
        )
    })?;
    if bits < 128 {
        let limit = 1i128 << bits;
        if parsed >= limit {
            bail!("value {} exceeds u{} range", value, bits);
        }
    }
    Ok(parsed)
}

pub(crate) fn parse_signed_int(value: &str, bits: u32) -> Result<i128> {
    let trimmed = value.trim();
    let cleaned = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let digits = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
        .unwrap_or(cleaned);
    let radix = if cleaned.starts_with("0x") || cleaned.starts_with("0X") { 16 } else { 10 };
    let parsed = i128::from_str_radix(digits, radix)
        .map_err(|err| anyhow!("failed to parse signed {}-bit integer '{}': {}", bits, value, err))?;
    if bits < 128 {
        let limit = 1i128 << (bits - 1);
        if parsed >= limit || parsed < -limit {
            bail!("value {} exceeds signed {}-bit range", value, bits);
        }
    }
    Ok(parsed)
}

pub(crate) fn compute_abi_checksum(methods: &[wasm::ContractMethod]) -> Result<String> {
    let mut methods_sorted = methods.to_vec();
    methods_sorted.sort_by(|a, b| {
        a.contract
            .cmp(&b.contract)
            .then_with(|| a.function.cmp(&b.function))
            .then_with(|| a.export.cmp(&b.export))
    });
    let json = serde_json::to_vec(&methods_sorted)?;
    Ok(format!("{:x}", Sha256::digest(&json)))
}

pub(crate) fn validate_plan_metadata(plan: &ExecutionPlan) -> Result<()> {
    let metadata = plan
        .contract
        .metadata
        .as_ref()
        .ok_or_else(|| anyhow!("execution plan is missing contract metadata; regenerate the plan"))?;

    let wasm_bytes = Base64::decode_vec(&plan.contract.wasm_base64)
        .map_err(|err| anyhow!("execution plan wasm payload is not valid base64: {err}"))?;
    wasm::validate_entry_abi_signature(&wasm_bytes)
        .map_err(|err| anyhow!("execution plan entry export ABI mismatch: {err}"))?;

    if metadata.methods.is_empty() {
        bail!("execution plan metadata has no contract methods; regenerate with an updated compiler");
    }

    let recorded = metadata
        .abi_sha256
        .as_deref()
        .ok_or_else(|| anyhow!("execution plan is missing ABI checksum; regenerate the plan"))?;
    let computed = compute_abi_checksum(&metadata.methods)?;
    if recorded != computed {
        bail!("execution plan ABI checksum mismatch; regenerate the plan to refresh metadata");
    }

    let compiler = metadata
        .compiler
        .as_ref()
        .ok_or_else(|| anyhow!("execution plan is missing compiler metadata attachment"))?;
    validate_abi_arguments_for_entry(
        &plan.contract.entry.contract,
        &plan.contract.entry.function,
        &plan.execution.typed_arguments,
        &compiler.metadata,
    )?;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbiArgumentKind {
    I32,
    U8,
    U32,
    I64,
    U64,
    U128,
    F32,
    F64,
    Bool,
    String,
    OptionString,
    Address20,
    OptionAddress20,
    BytesBase64,
}

pub(crate) fn validate_abi_arguments_for_entry(
    contract: &str,
    function: &str,
    typed_arguments: &[TypedArgument],
    compiler: &CompilerMetadata,
) -> Result<()> {
    let method = resolve_compiler_method_metadata(compiler, contract, function).ok_or_else(|| {
        anyhow!(
            "compiler metadata missing ABI for {}::{}; recompile with metadata enabled",
            contract,
            function
        )
    })?;

    let expected_count = method.params.len();
    let (args_to_check, expected_count) = if typed_arguments.len() == expected_count + 1
        && matches!(typed_arguments.first(), Some(TypedArgument::BytesBase64(_)))
    {
        (&typed_arguments[1..], expected_count)
    } else {
        (typed_arguments, expected_count)
    };

    if args_to_check.len() != expected_count {
        bail!(
            "ABI argument count mismatch for {}::{}: expected {}, got {}",
            contract,
            function,
            expected_count,
            args_to_check.len()
        );
    }

    for (index, (param, arg)) in method.params.iter().zip(args_to_check).enumerate() {
        let expected_kinds = abi_expected_kinds(&param.ty).ok_or_else(|| {
            anyhow!(
                "unsupported ABI type '{}' for {}::{} parameter {}",
                param.ty,
                contract,
                function,
                index
            )
        })?;

        let actual_kind = abi_argument_kind(arg);
        if !expected_kinds.iter().any(|kind| *kind == actual_kind) {
            bail!(
                "ABI type mismatch for {}::{} argument {}: expected {}, got {}",
                contract,
                function,
                index,
                format_expected_kinds(&expected_kinds),
                format_argument_kind(actual_kind)
            );
        }
    }

    Ok(())
}

pub(crate) fn resolve_compiler_method_metadata<'a>(
    metadata: &'a CompilerMetadata,
    contract: &'a str,
    function: &'a str,
) -> Option<&'a CompilerContractMethodMetadata> {
    let selector = format!("{}::{}", contract, function);
    metadata.contracts.iter().find(|entry| entry.name == contract).and_then(|entry| {
        entry
            .methods
            .iter()
            .find(|method| method.selector == selector || method.name == function)
    })
}

fn abi_expected_kinds(raw: &str) -> Option<Vec<AbiArgumentKind>> {
    let mut normalized = raw.trim().to_ascii_lowercase();
    normalized.retain(|c| !c.is_whitespace());

    if let Some(inner) = normalized
        .strip_prefix("optional<")
        .and_then(|rest| rest.strip_suffix('>'))
        .or_else(|| {
            normalized
                .strip_prefix("option<")
                .and_then(|rest| rest.strip_suffix('>'))
        })
    {
        return match inner {
            "string" => Some(vec![AbiArgumentKind::OptionString]),
            "address" | "address20" => Some(vec![AbiArgumentKind::OptionAddress20]),
            _ if inner.starts_with("list<") || inner.starts_with("map<") => {
                Some(vec![AbiArgumentKind::OptionString])
            }
            _ if inner.starts_with("bytes") => Some(vec![AbiArgumentKind::OptionString]),
            _ => None,
        };
    }

    match normalized.as_str() {
        "string" => Some(vec![AbiArgumentKind::String]),
        "address" | "address20" => Some(vec![AbiArgumentKind::Address20]),
        "bool" => Some(vec![AbiArgumentKind::Bool]),
        "uint8" | "u8" => Some(vec![
            AbiArgumentKind::U8,
            AbiArgumentKind::I32,
            AbiArgumentKind::I64,
        ]),
        "uint32" | "u32" => Some(vec![
            AbiArgumentKind::U32,
            AbiArgumentKind::I32,
            AbiArgumentKind::I64,
        ]),
        "uint64" | "u64" => Some(vec![AbiArgumentKind::U64, AbiArgumentKind::I64]),
        "uint128" | "u128" => Some(vec![
            AbiArgumentKind::U128,
            AbiArgumentKind::U64,
            AbiArgumentKind::I64,
        ]),
        "int32" | "i32" => Some(vec![
            AbiArgumentKind::I32,
            AbiArgumentKind::Bool,
            AbiArgumentKind::String,
            AbiArgumentKind::OptionString,
            AbiArgumentKind::Address20,
            AbiArgumentKind::OptionAddress20,
            AbiArgumentKind::BytesBase64,
        ]),
        "int64" | "i64" => Some(vec![AbiArgumentKind::I64]),
        "float32" | "f32" => Some(vec![AbiArgumentKind::F32]),
        "float64" | "f64" => Some(vec![AbiArgumentKind::F64]),
        _ if normalized.starts_with("bytes") => Some(vec![AbiArgumentKind::BytesBase64]),
        _ if normalized.starts_with("list<") || normalized.starts_with("map<") => {
            Some(vec![AbiArgumentKind::String, AbiArgumentKind::BytesBase64])
        }
        _ => None,
    }
}

fn abi_argument_kind(arg: &TypedArgument) -> AbiArgumentKind {
    match arg {
        TypedArgument::I32(_) => AbiArgumentKind::I32,
        TypedArgument::U8(_) => AbiArgumentKind::U8,
        TypedArgument::U32(_) => AbiArgumentKind::U32,
        TypedArgument::I64(_) => AbiArgumentKind::I64,
        TypedArgument::U64(_) => AbiArgumentKind::U64,
        TypedArgument::U128(_) => AbiArgumentKind::U128,
        TypedArgument::F32(_) => AbiArgumentKind::F32,
        TypedArgument::F64(_) => AbiArgumentKind::F64,
        TypedArgument::Bool(_) => AbiArgumentKind::Bool,
        TypedArgument::String(_) => AbiArgumentKind::String,
        TypedArgument::OptionString(_) => AbiArgumentKind::OptionString,
        TypedArgument::Address20(_) => AbiArgumentKind::Address20,
        TypedArgument::OptionAddress20(_) => AbiArgumentKind::OptionAddress20,
        TypedArgument::BytesBase64(_) => AbiArgumentKind::BytesBase64,
    }
}

fn format_expected_kinds(kinds: &[AbiArgumentKind]) -> String {
    kinds
        .iter()
        .map(|kind| format_argument_kind(*kind))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn format_argument_kind(kind: AbiArgumentKind) -> String {
    match kind {
        AbiArgumentKind::I32 => "i32",
        AbiArgumentKind::U8 => "u8",
        AbiArgumentKind::U32 => "u32",
        AbiArgumentKind::I64 => "i64",
        AbiArgumentKind::U64 => "u64",
        AbiArgumentKind::U128 => "u128",
        AbiArgumentKind::F32 => "f32",
        AbiArgumentKind::F64 => "f64",
        AbiArgumentKind::Bool => "bool",
        AbiArgumentKind::String => "string",
        AbiArgumentKind::OptionString => "option<string>",
        AbiArgumentKind::Address20 => "address20",
        AbiArgumentKind::OptionAddress20 => "option<address20>",
        AbiArgumentKind::BytesBase64 => "bytes",
    }
    .to_string()
}

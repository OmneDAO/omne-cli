use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use deploy_guardrails::canonical_metadata_digest;
use deploy_guardrails::metadata::{
    CompilerContractMetadata, CompilerContractMethodMetadata, CompilerMetadata,
    CompilerFunctionParamMetadata, CompilerMetadataSignature,
};
use ed25519_dalek::{Signer, SigningKey};
use hex::FromHex;
use serde::Serialize;
use sha2::Digest;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use wasmparser::{Parser, Payload};

#[derive(Debug, Serialize)]
struct MetadataEnvelope {
    metadata: CompilerMetadata,
    signature: CompilerMetadataSignature,
}

#[derive(Debug)]
struct ContractMethod {
    contract: String,
    function: String,
    export: String,
}

#[derive(Debug, Clone)]
struct FunctionSignature {
    params: Vec<CompilerFunctionParamMetadata>,
    return_type: Option<String>,
}

fn main() -> Result<()> {
    let mut modules_path: Option<PathBuf> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--modules" {
            let path = args.next().ok_or_else(|| {
                anyhow!(
                    "usage: omne-metadata [--modules DIR] <wasm-path> <signing-key-path> <output-path>"
                )
            })?;
            modules_path = Some(PathBuf::from(path));
        } else {
            positional.push(arg);
        }
    }

    if positional.len() != 3 {
        bail!(
            "usage: omne-metadata [--modules DIR] <wasm-path> <signing-key-path> <output-path>"
        );
    }

    let wasm_path = PathBuf::from(&positional[0]);
    let key_path = PathBuf::from(&positional[1]);
    let output_path = PathBuf::from(&positional[2]);

    let wasm_bytes = fs::read(&wasm_path)
        .with_context(|| format!("failed to read wasm at {}", wasm_path.display()))?;

    let methods = extract_methods(&wasm_bytes)?;
    if methods.is_empty() {
        bail!("no contract exports matched Omne ABI conventions");
    }

    let signatures = if let Some(path) = modules_path {
        Some(parse_pysub_functions(&path)?)
    } else {
        None
    };

    let metadata = build_metadata(&wasm_path, &wasm_bytes, &methods, signatures.as_ref())?;
    let signature = sign_metadata(&metadata, &key_path)?;

    let envelope = MetadataEnvelope {
        metadata,
        signature,
    };
    let json = serde_json::to_string_pretty(&envelope)?;
    fs::write(&output_path, json)
        .with_context(|| format!("failed to write metadata to {}", output_path.display()))?;

    println!("{}", envelope.signature.public_key_hex);
    Ok(())
}

fn extract_methods(bytes: &[u8]) -> Result<Vec<ContractMethod>> {
    let mut methods: BTreeMap<(String, String), ContractMethod> = BTreeMap::new();

    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ExportSection(section) => {
                for export in section {
                    let export = export?;
                    let name = export.name;

                    if name == axiom_runtime::abi::ENTRY_EXPORT
                        || name == axiom_runtime::abi::LEGACY_ENTRY_EXPORT
                    {
                        continue;
                    }

                    if let Some(stripped) =
                        name.strip_prefix(axiom_runtime::abi::CONTRACT_EXPORT_PREFIX)
                    {
                        let (contract, function) = split_selector(stripped)?;
                        methods
                            .entry((contract.clone(), function.clone()))
                            .or_insert_with(|| ContractMethod {
                                contract: contract.clone(),
                                function: function.clone(),
                                export: name.to_string(),
                            });
                    }
                }
            }
            _ => {}
        }
    }

    Ok(methods.into_values().collect())
}

fn split_selector(selector: &str) -> Result<(String, String)> {
    let (contract, function) = selector.split_once("::").ok_or_else(|| {
        anyhow!(
            "malformed export name '{}': expected Contract::function",
            selector
        )
    })?;
    Ok((contract.to_string(), function.to_string()))
}

fn build_metadata(
    wasm_path: &Path,
    bytes: &[u8],
    methods: &[ContractMethod],
    signatures: Option<&BTreeMap<String, FunctionSignature>>,
) -> Result<CompilerMetadata> {
    let mut contract_map: BTreeMap<String, Vec<CompilerContractMethodMetadata>> = BTreeMap::new();
    for method in methods {
        let (params, return_type) = signatures
            .and_then(|sig_map| sig_map.get(&method.function))
            .map(|sig| (sig.params.clone(), sig.return_type.clone()))
            .unwrap_or_else(|| (Vec::new(), None));

        contract_map
            .entry(method.contract.clone())
            .or_default()
            .push(CompilerContractMethodMetadata {
                name: method.function.clone(),
                selector: format!("{}::{}", method.contract, method.function),
                export: method.export.clone(),
                params,
                return_type,
            });
    }

    let contracts = contract_map
        .into_iter()
        .map(|(name, methods)| CompilerContractMetadata {
            name,
            params: Vec::new(),
            storage: Vec::new(),
            methods,
        })
        .collect();

    let wasm_sha256 = format!("{:x}", sha2::Sha256::digest(bytes));

    Ok(CompilerMetadata {
        metadata_version: "1.0".to_string(),
        compiler_version: "metadata-tool".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        source_path: Some(wasm_path.display().to_string()),
        wasm_sha256,
        wasm_size_bytes: bytes.len(),
        contracts,
        free_functions: Vec::new(),
        host_functions: Vec::new(),
    })
}

fn parse_pysub_functions(dir: &Path) -> Result<BTreeMap<String, FunctionSignature>> {
    let mut functions: BTreeMap<String, FunctionSignature> = BTreeMap::new();

    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read pysub modules directory at {}", dir.display()))?;

    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("pysub") {
            continue;
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read pysub module {}", path.display()))?;
        let lines: Vec<&str> = content.lines().collect();
        let mut idx = 0;
        while idx < lines.len() {
            let raw_line = lines[idx];
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                idx += 1;
                continue;
            }

            // Only treat top-level function declarations; ignore indented statements.
            if raw_line.starts_with(char::is_whitespace) {
                idx += 1;
                continue;
            }

            if let Some(open) = line.find('(') {
                let name = line[..open].trim();
                if name.is_empty()
                    || !name
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_alphabetic())
                        .unwrap_or(false)
                {
                    idx += 1;
                    continue;
                }

                // Accumulate multiline signatures until we hit the terminating ':'.
                let mut signature = line.to_string();
                while !signature.trim_end().ends_with(':') && idx + 1 < lines.len() {
                    idx += 1;
                    signature.push(' ');
                    signature.push_str(lines[idx].trim());
                }

                let signature = signature.trim_end_matches(':').trim().to_string();
                let (head, return_type) = if let Some((lhs, rhs)) = signature.split_once("->") {
                    (lhs.trim(), Some(rhs.trim().to_string()))
                } else {
                    (signature.as_str().trim(), None)
                };

                if let Some((_, params_tail)) = head.split_once('(') {
                    if let Some((params_str, _)) = params_tail.rsplit_once(')') {
                        let params = params_str
                            .split(',')
                            .filter_map(|raw| {
                                let trimmed = raw.trim();
                                if trimmed.is_empty() {
                                    return None;
                                }
                                let (name, ty) = trimmed.split_once(':')?;
                                let name = name.trim();
                                if name == "ctx" {
                                    return None;
                                }
                                let ty = ty.trim();
                                Some(CompilerFunctionParamMetadata {
                                    name: name.to_string(),
                                    ty: ty.to_string(),
                                })
                            })
                            .collect();

                        functions.entry(name.to_string()).or_insert(FunctionSignature {
                            params,
                            return_type: return_type.clone(),
                        });
                    }
                }
            }

            idx += 1;
        }
    }

    Ok(functions)
}

fn sign_metadata(
    metadata: &CompilerMetadata,
    key_path: &Path,
) -> Result<CompilerMetadataSignature> {
    let raw_key = fs::read_to_string(key_path)
        .with_context(|| format!("failed to read signing key at {}", key_path.display()))?;
    let key_hex = raw_key.trim();
    let key_bytes = Vec::from_hex(key_hex).map_err(|err| anyhow!(err.to_string()))?;
    if key_bytes.len() != 32 {
        bail!("signing key must be 32 bytes, found {}", key_bytes.len());
    }
    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&key_bytes);

    let signing_key = SigningKey::from_bytes(&key_array);
    let digest = canonical_metadata_digest(metadata).map_err(|err| anyhow!(err.to_string()))?;
    let signature = signing_key.sign(digest.as_ref());
    let verifying_key = signing_key.verifying_key();

    Ok(CompilerMetadataSignature {
        algorithm: "ed25519".to_string(),
        public_key_hex: hex::encode(verifying_key.to_bytes()),
        signature_hex: hex::encode(signature.to_bytes()),
        digest_hex: hex::encode(digest),
        signed_at: Utc::now().to_rfc3339(),
    })
}

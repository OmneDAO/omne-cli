//! Helpers for inspecting compiled WASM contracts and extracting Omne ABI metadata.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::fs;
use wasmparser::{Parser, Payload};

/// Representation of a compiled contract module with extracted metadata.
#[derive(Debug, Clone)]
pub struct ContractModule {
    path: PathBuf,
    bytes: Vec<u8>,
    metadata: ContractMetadata,
}

impl ContractModule {
    /// Original filesystem path for this module.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Raw WASM bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Extracted contract metadata.
    pub fn metadata(&self) -> &ContractMetadata {
        &self.metadata
    }
}

/// Contract metadata extracted from a pysub-generated module.
#[derive(Debug, Clone, Serialize)]
pub struct ContractMetadata {
    has_runtime_entry: bool,
    has_legacy_entry: bool,
    contract_methods: Vec<ContractMethod>,
}

impl ContractMetadata {
    /// Return `true` if the module exports the ABI-stable entry point.
    pub fn has_runtime_entry(&self) -> bool {
        self.has_runtime_entry
    }

    /// Return `true` if the module exposes the legacy `main` export.
    pub fn has_legacy_entry(&self) -> bool {
        self.has_legacy_entry
    }

    /// List all discovered contract methods.
    pub fn contract_methods(&self) -> &[ContractMethod] {
        &self.contract_methods
    }

    /// Resolve a contract method by any recognised selector.
    pub fn resolve_method(&self, selector: &str) -> Option<&ContractMethod> {
        self.contract_methods
            .iter()
            .find(|method| method.matches_selector(selector))
    }

    /// Pick a default contract method when one exists.
    pub fn default_method(&self) -> Option<&ContractMethod> {
        if self.contract_methods.len() == 1 {
            self.contract_methods.first()
        } else {
            None
        }
    }
}

/// Description of a single contract export discovered within the module.
#[derive(Debug, Clone, Serialize)]
pub struct ContractMethod {
    pub contract: String,
    pub function: String,
    pub export: String,
    pub legacy_export: Option<String>,
    pub has_runtime_export: bool,
    pub has_legacy_export: bool,
}

impl ContractMethod {
    fn new(contract: &str, function: &str) -> Self {
        Self {
            contract: contract.to_string(),
            function: function.to_string(),
            export: axiom_runtime::abi::contract_export(contract, function),
            legacy_export: None,
            has_runtime_export: false,
            has_legacy_export: false,
        }
    }

    /// Canonical selector form (`Contract::function`).
    pub fn selector(&self) -> String {
        format!("{}::{}", self.contract, self.function)
    }

    fn matches_selector(&self, selector: &str) -> bool {
        let canonical = self.selector();
        let runtime_export = &self.export;
        let legacy_export = self.legacy_export.as_deref();

        selector == canonical
            || selector == runtime_export
            || legacy_export.map_or(false, |legacy| selector == legacy)
            || selector
                .strip_prefix(axiom_runtime::abi::CONTRACT_EXPORT_PREFIX)
                .map_or(false, |tail| tail == canonical)
    }
}

/// Load a WASM module from disk and extract the ABI metadata.
pub async fn load_contract_module(path: impl AsRef<Path>) -> Result<ContractModule> {
    let path = path.as_ref();
    let bytes = fs::read(path)
        .await
        .with_context(|| format!("failed to read contract module at {}", path.display()))?;

    let mut has_runtime_entry = false;
    let mut has_legacy_entry = false;
    let mut methods: BTreeMap<(String, String), ContractMethod> = BTreeMap::new();

    for payload in Parser::new(0).parse_all(&bytes) {
        match payload.with_context(|| format!("failed to parse {}", path.display()))? {
            Payload::ExportSection(section) => {
                for export in section {
                    let export = export
                        .with_context(|| format!("failed to parse export in {}", path.display()))?;
                    let name = export.name;

                    if name == axiom_runtime::abi::ENTRY_EXPORT {
                        has_runtime_entry = true;
                        continue;
                    }
                    if name == axiom_runtime::abi::LEGACY_ENTRY_EXPORT {
                        has_legacy_entry = true;
                        continue;
                    }

                    if let Some(stripped) =
                        name.strip_prefix(axiom_runtime::abi::CONTRACT_EXPORT_PREFIX)
                    {
                        let (contract, function) = split_selector(stripped)?;
                        let entry = methods
                            .entry((contract.clone(), function.clone()))
                            .or_insert_with(|| ContractMethod::new(&contract, &function));
                        entry.export = name.to_string();
                        entry.has_runtime_export = true;
                    } else if name.contains("::") {
                        let (contract, function) = split_selector(name)?;
                        let entry = methods
                            .entry((contract.clone(), function.clone()))
                            .or_insert_with(|| ContractMethod::new(&contract, &function));
                        entry.legacy_export = Some(name.to_string());
                        entry.has_legacy_export = true;
                    }
                }
            }
            _ => {}
        }
    }

    if methods.is_empty() {
        bail!(
            "no contract exports matched Omne ABI conventions in {}",
            path.display()
        );
    }

    let mut contract_methods: Vec<_> = methods.into_values().collect();
    contract_methods.sort_by(|a, b| {
        a.contract
            .cmp(&b.contract)
            .then_with(|| a.function.cmp(&b.function))
    });

    if contract_methods
        .iter()
        .any(|method| !method.has_runtime_export)
    {
        bail!(
            "contract module at {} is missing ABI-stable exports; recompile with an updated compiler",
            path.display()
        );
    }

    Ok(ContractModule {
        path: path.to_path_buf(),
        bytes,
        metadata: ContractMetadata {
            has_runtime_entry,
            has_legacy_entry,
            contract_methods,
        },
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_method_selector_matching() {
        let mut method = ContractMethod::new("Wallet", "balance");
        method.export = "axiom_contract::Wallet::balance".to_string();
        method.has_runtime_export = true;
        method.legacy_export = Some("Wallet::balance".to_string());
        method.has_legacy_export = true;

        assert!(method.matches_selector("Wallet::balance"));
        assert!(method.matches_selector("axiom_contract::Wallet::balance"));
        assert!(!method.matches_selector("Wallet::deposit"));
    }
}

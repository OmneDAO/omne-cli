#!/bin/bash

# Generate shell completions for OMNE CLI
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPLETIONS_DIR="$SCRIPT_DIR/completions"

# Create completions directory
mkdir -p "$COMPLETIONS_DIR"/{bash,zsh,fish,powershell}

# Build the CLI first
echo "Building OMNE CLI..."
cargo build --release

OMNE_BIN="$SCRIPT_DIR/target/release/omne"

# Generate completions for each shell
echo "Generating bash completions..."
$OMNE_BIN completion bash > "$COMPLETIONS_DIR/bash/omne"

echo "Generating zsh completions..."
$OMNE_BIN completion zsh > "$COMPLETIONS_DIR/zsh/_omne"

echo "Generating fish completions..."
$OMNE_BIN completion fish > "$COMPLETIONS_DIR/fish/omne.fish"

echo "Generating PowerShell completions..."
$OMNE_BIN completion powershell > "$COMPLETIONS_DIR/powershell/omne.ps1"

echo "Shell completions generated successfully in $COMPLETIONS_DIR"
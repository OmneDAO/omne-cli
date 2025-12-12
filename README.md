# Omne CLI 🚀

[![Build Status](https://img.shields.io/badge/build-passing-brightgreen)](https://github.com/OmneDAO/omne-cli)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://www.rust-lang.org)
[![Version](https://img.shields.io/badge/version-0.1.0-green)](https://github.com/OmneDAO/omne-cli)

**Unified command-line orchestration tool for the Omne blockchain ecosystem**

The Omne CLI provides comprehensive management for the entire Omne blockchain ecosystem, including network operations, validator coordination, infrastructure services, developer tools, and operational monitoring.

## ✨ Key Features

### 🌐 Network Management
- **Bootstrap networks** with configurable validators and services
- **Monitor network health** with real-time metrics and alerts
- **Coordinate upgrades** across the entire network safely
- **Health diagnostics** with comprehensive reporting

### 🏛️ Validator Operations  
- **Dynamic staking** with 15-28 OGT range based on network conditions
- **Infrastructure services** integration (OMP, ORC-20, EEC-4337)
- **Revenue tracking** with $2K-40K monthly earning potential
- **Auto-optimization** for maximum profitability

### 💻 Developer Tools
- **Project scaffolding** with templates for React, Python, Go, Rust
- **SDK management** across TypeScript, Python, and Go
- **Local development** networks with full service integration
- **Deployment automation** to testnet and mainnet

### ⚡ Infrastructure Services
- **OMP Storage**: Hybrid storage service ($0.01/MB pricing)
- **Enhanced ORC-20**: Meta-transaction relaying with gas sponsorship
- **EEC-4337**: Account abstraction with smart wallet support
- **Real-time monitoring** with revenue optimization

### 🔧 Operations & Monitoring
- **System monitoring** with interactive dashboard
- **Automated backups** with encryption and compression
- **Rolling upgrades** with safety checks and rollback
- **Performance optimization** with intelligent recommendations

## 📦 Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/OmneDAO/omne-cli
cd omne-cli

# Build and install
cargo build --release
sudo cp target/release/omne /usr/local/bin/

# Verify installation
omne --version
```

### Pre-built Binaries

Download the latest release for your platform:

```bash
# macOS (Intel)
wget https://github.com/OmneDAO/omne-cli/releases/latest/download/omne-x86_64-apple-darwin

# macOS (Apple Silicon) 
wget https://github.com/OmneDAO/omne-cli/releases/latest/download/omne-aarch64-apple-darwin

# Linux (x86_64)
wget https://github.com/OmneDAO/omne-cli/releases/latest/download/omne-x86_64-unknown-linux-gnu

# Windows
wget https://github.com/OmneDAO/omne-cli/releases/latest/download/omne-x86_64-pc-windows-msvc.exe
```

### Package Managers

```bash
# Homebrew (macOS/Linux)
brew install omne-cli

# Cargo
cargo install omne-cli

# Chocolatey (Windows)
choco install omne-cli

# Snap (Linux)
snap install omne-cli
```

## 🚀 Quick Start

### Network Operations

```bash
# Bootstrap a development network
omne network bootstrap --validators 3 --services

# Check network status
omne network status --detailed

# Monitor network health
omne network health --services --report
```

### Validator Management

```bash
# Initialize validator with infrastructure services
omne validator init --services omp,orc20,paymaster

# Start validator with optimization
omne validator start --auto-optimize --earnings-tracking

# Check validator earnings
omne validator earnings --breakdown --period month

# Monitor validator status
omne validator status --services
```

### Developer Workflow

```bash
# Create new React project with Omne integration
omne dev new my-dapp --template react-typescript --sdk latest

# Start local development network
omne dev local start --validators 3 --services

# Run comprehensive tests
omne dev test --integration --performance

# Deploy to testnet (plan mode, auto-signs with ephemeral key if none supplied)
omne dev deploy --contract ./contract.wasm --services omp --network testnet

# Verify a saved execution plan against the signer allow-list
omne dev deploy verify ./contract.execution.json

# Generate an unsigned plan (unsafe for hardened endpoints)
omne dev deploy --no-sign --contract ./contract.wasm --network devnet
```

#### Plan Signing & Verification

- `omne dev deploy` now signs every execution plan by default. Supply `--signing-key <path>` with a hex-encoded Ed25519 secret to use a managed key, or let the CLI mint an ephemeral key. Ephemeral secrets are stored beside the plan as `<plan>.signing-key` so operators can promote them into an allow-list.
- `omne dev deploy verify <plan.json>` replays the canonical digest computation and checks the signature against the configured signer allow-list. Add extra approved keys inline with `--allowed-signer <hex>` or bypass enforcement with `--allow-unknown-signer` (not recommended for production).
- `--no-sign` skips attaching a signature entirely—handy for local smoke tests, but hardened RPC endpoints will reject unsigned plans.
- After a successful submission the CLI now checks the deployment metadata service (plan listings and nonce provenance) to confirm durable persistence. The canonical service list stored in the metadata layer is echoed back to the operator so discrepancies between the submitted plan and persisted record are easy to spot.

##### SDK Alignment

- The TypeScript SDK exposes the same hardened submission flow via `omneClient.deployExecutionPlan(plan, options)` and low-level helpers such as `generateDeploymentNonce`, `buildDeploymentHeaders`, and `ensureSignedCompilerAttachment`. This allows backend services or CI pipelines to reuse the CLI’s guardrail logic when calling the `/v1/deployments` API directly.
- See `sdk/typescript/examples/basic-usage.ts` for an end-to-end sample that loads a signed plan, verifies compiler metadata, and submits using the new helper. Keeping the CLI and SDK aligned ensures signatures, nonce handling, and error messaging stay consistent across toolchains.

### Infrastructure Services

```bash
# Monitor all infrastructure services
omne infrastructure monitor --realtime --revenue-breakdown

# Configure OMP storage
omne infrastructure omp quota 200  # 200GB

# Check ORC-20 relayer performance
omne infrastructure orc20 metrics --realtime

# Configure paymaster policies
omne infrastructure paymaster configure --budget 2000 --min-reputation 0.8
```

### Operations & Maintenance

```bash
# System monitoring dashboard
omne ops monitor --dashboard --interval 10

# Backup system with encryption
omne ops backup --destination s3://my-backups --encrypt --compress

# Rolling system upgrade
omne ops upgrade --version 2.1.0 --safety-checks

# Performance optimization
omne ops optimize --recommendations
```

## 📖 Command Reference

### Global Options

| Option | Description | Default |
|--------|-------------|---------|
| `-v, --verbose` | Enable verbose logging | false |
| `-c, --config` | Configuration file path | auto-detect |
| `--network` | Network environment | testnet |

### Network Commands

| Command | Description |
|---------|-------------|
| `network bootstrap` | Bootstrap new network |
| `network status` | Show network status |
| `network upgrade` | Coordinate upgrades |
| `network health` | Health diagnostics |

### Validator Commands

| Command | Description |
|---------|-------------|
| `validator init` | Initialize validator |
| `validator start` | Start validator node |
| `validator stake` | Manage staking |
| `validator services` | Service management |
| `validator earnings` | Show earnings |
| `validator status` | Check status |

### Developer Commands

| Command | Description |
|---------|-------------|
| `dev new` | Create new project |
| `dev test` | Run test suite |
| `dev deploy` | Generate or verify signed execution plans |
| `dev sdk` | SDK management |
| `dev local` | Local network |

### Infrastructure Commands  

| Command | Description |
|---------|-------------|
| `infrastructure omp` | OMP storage |
| `infrastructure orc20` | ORC-20 relayer |
| `infrastructure paymaster` | EEC-4337 paymaster |
| `infrastructure monitor` | Service monitoring |

### Operations Commands

| Command | Description |
|---------|-------------|
| `ops monitor` | System monitoring |
| `ops backup` | Backup system |
| `ops upgrade` | System upgrades |
| `ops logs` | Log management |
| `ops optimize` | Performance tuning |

## ⚙️ Configuration

The CLI uses a configuration file located at `~/.config/omne-cli/config.toml`:

```toml
[network]
name = "testnet"
chain_id = 1338
rpc_endpoint = "https://testnet-rpc.omne.network"
ws_endpoint = "wss://testnet-ws.omne.network"
# Optional override for the deployment metadata service. By default the CLI derives
# https://.../v1/ from the RPC endpoint when hardened deployments are enabled.
metadata_base_url = "https://testnet-rpc.omne.network/v1/"

[validator]
enabled = true
auto_optimize = true
earnings_tracking = true

[infrastructure.omp]
enabled = true
storage_quota_gb = 100
price_per_mb_usd = 0.01

[infrastructure.orc20]
enabled = true
gas_price_multiplier = 1.2

[infrastructure.paymaster]
enabled = true
monthly_budget_usd = 1000
min_reputation_score = 0.7

[development]
local_network_validators = 3
auto_start_services = true
```

## 💰 Revenue Model

### Infrastructure Services Earnings

| Service | Revenue Range | Description |
|---------|--------------|-------------|
| **OMP Storage** | $500-2,000/month | Hybrid storage with 75% cost savings |
| **ORC-20 Relayer** | $300-1,500/month | Meta-transaction processing |
| **EEC-4337 Paymaster** | $1,000-5,000/month | Smart wallet abstraction |

**Total Potential**: $2,000-40,000/month depending on utilization and network size.

### Dynamic Staking System

- **Range**: 15-28 OGT minimum stake (vs static 20 OGT)
- **Factors**: Network utilization, validator density, economic conditions
- **Benefits**: Optimal security with accessibility

## 🔧 Development

### Building from Source

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/OmneDAO/omne-cli
cd omne-cli
cargo build --release

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run linting
cargo clippy
```

### Project Structure

```
omne-cli/
├── src/
│   ├── main.rs              # CLI entry point
│   ├── commands/            # Command implementations
│   │   ├── network.rs       # Network operations
│   │   ├── validator.rs     # Validator management
│   │   ├── dev.rs          # Developer tools
│   │   ├── infrastructure.rs # Service management
│   │   └── ops.rs          # Operations & monitoring
│   ├── config/             # Configuration management
│   └── utils/              # Utilities and helpers
├── Cargo.toml              # Dependencies and metadata
└── README.md              # This file
```

## 🔒 Security

### Key Management
- Hardware wallet support
- Encrypted key storage
- Secure key generation
- Multi-signature support

### Network Security
- TLS/SSL encryption
- Rate limiting
- DDoS protection
- Secure API endpoints

## 🐛 Troubleshooting

### Common Issues

**Installation Issues**
```bash
# Update Rust toolchain
rustup update

# Clear cargo cache
cargo clean
```

**Network Connection**
```bash
# Test network connectivity
omne network status

# Verify configuration
omne --config ~/.config/omne-cli/config.toml network status
```

**Validator Problems**
```bash
# Check validator status
omne validator status --services

# Restart with fresh configuration
omne validator init --services omp,orc20,paymaster
```

### Getting Help

- **Documentation**: [docs.omne.network](https://docs.omne.network)
- **Discord**: [Omne Community](https://discord.gg/omne)
- **GitHub Issues**: [Report bugs](https://github.com/OmneDAO/omne-cli/issues)
- **Email**: dev@omne.network

## 🤝 Contributing

We welcome contributions! Please see our [Contributing Guide](CONTRIBUTING.md) for details.

### Development Setup

```bash
# Fork and clone
git clone https://github.com/yourusername/omne-cli
cd omne-cli

# Create feature branch
git checkout -b feature/awesome-feature

# Make changes and test
cargo test
cargo fmt
cargo clippy

# Submit pull request
git push origin feature/awesome-feature
```

## 📄 License

This project is dual-licensed under:

- [MIT License](https://opensource.org/licenses/MIT)
- [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)

You may choose either license at your option.

## 🙏 Acknowledgments

- **Omne Network Team** for the blockchain infrastructure
- **Rust Community** for excellent tooling and libraries
- **Contributors** who help improve the CLI

---

**Ready to orchestrate the Omne ecosystem? Install the CLI and start earning today!** 🚀

For more information, visit [omne.network](https://omne.network) or join our [Discord community](https://discord.gg/omne).
# OMNE CLI Release Guide

This document provides a comprehensive guide for releasing the OMNE CLI across all distribution channels.

## Release Checklist

### Prerequisites
- [ ] GitHub repository access with release permissions
- [ ] Cargo.rs account with publish permissions  
- [ ] Docker Hub or GitHub Container Registry access
- [ ] Package manager accounts (Homebrew, Chocolatey, Snap, etc.)
- [ ] Vercel deployment access for documentation

### Required Secrets (GitHub Repository Settings)
Configure these secrets in your GitHub repository:

- `CARGO_REGISTRY_TOKEN` - Token for publishing to crates.io
- `CHOCOLATEY_API_KEY` - API key for Chocolatey package repository
- `SNAPCRAFT_STORE_CREDENTIALS` - Snap Store credentials (base64 encoded)
- `HOMEBREW_GITHUB_TOKEN` - GitHub token for Homebrew formula updates
- `VERCEL_TOKEN` - Vercel deployment token
- `VERCEL_ORG_ID` - Vercel organization ID
- `VERCEL_PROJECT_ID` - Vercel project ID
- `DOCS_DEPLOY_TOKEN` - Token for triggering documentation deployment

## Automated Release Process

### 1. Version Update
Update the version in `Cargo.toml`:
```toml
[package]
version = "0.1.0"  # Update this
```

### 2. Run Release Script
Execute the comprehensive release script:
```bash
# Dry run to verify everything looks correct
DRY_RUN=true ./scripts/release.sh 0.1.0

# Execute the actual release
./scripts/release.sh 0.1.0
```

### 3. Monitor GitHub Actions
The release script triggers several GitHub Actions workflows:
- **Release Workflow**: Builds binaries, creates GitHub release, publishes to crates.io
- **Package Manager Updates**: Updates Homebrew, Chocolatey, Snap packages
- **Documentation Deployment**: Updates docs.omne.foundation

## Distribution Channels

### 1. GitHub Release ✅ (Automated)
- **Trigger**: Git tag push
- **Artifacts**: Multi-platform binaries (Linux, macOS, Windows)
- **Status**: Fully automated via GitHub Actions

### 2. Cargo Crates.io ✅ (Automated)
- **Installation**: `cargo install omne-cli`
- **Trigger**: GitHub Actions after successful build
- **Status**: Fully automated

### 3. Docker Images ✅ (Automated)  
- **Registry**: GitHub Container Registry (ghcr.io)
- **Images**: Multi-arch (AMD64, ARM64)
- **Installation**: `docker run ghcr.io/omnedao/omne-cli`
- **Status**: Fully automated

### 4. Homebrew 🔶 (Semi-automated)
- **Installation**: `brew install omne-cli`
- **Process**: GitHub Actions attempts auto-update of formula
- **Manual Steps**: May require PR to homebrew-core if auto-update fails
- **Status**: Automated with manual fallback

### 5. Chocolatey 🔶 (Semi-automated)
- **Installation**: `choco install omne-cli`
- **Process**: GitHub Actions builds package
- **Manual Steps**: Submit to Chocolatey community repository
- **Status**: Build automated, publishing requires manual approval

### 6. Snap Store 🔶 (Semi-automated)
- **Installation**: `sudo snap install omne-cli`
- **Process**: GitHub Actions builds snap package
- **Manual Steps**: May require manual store submission
- **Status**: Build automated, store submission varies

### 7. Documentation Site ✅ (Automated)
- **URL**: https://docs.omne.foundation/cli
- **Process**: Vercel deployment via GitHub Actions
- **Status**: Fully automated

## Manual Release Steps

### Homebrew Formula Update (if automation fails)
1. Fork `homebrew/homebrew-core`
2. Update `Formula/omne-cli.rb`:
   ```ruby
   url "https://github.com/OmneDAO/omne-cli/archive/v0.1.0.tar.gz"
   sha256 "CALCULATED_SHA256_HASH"
   ```
3. Submit PR to homebrew-core

### Chocolatey Package Submission
1. Build completes automatically via GitHub Actions
2. Download built package from workflow artifacts
3. Submit to Chocolatey community repository:
   ```bash
   choco push omne-cli.0.1.0.nupkg --api-key YOUR_API_KEY
   ```

### Snap Store Submission
1. Build completes automatically via GitHub Actions
2. If store submission fails, manually submit:
   ```bash
   snapcraft login
   snapcraft upload --release=stable omne-cli_0.1.0_amd64.snap
   ```

## Verification Steps

After release, verify installation from each channel:

### 1. GitHub Release
```bash
# Download and test binary
curl -L https://github.com/OmneDAO/omne-cli/releases/download/v0.1.0/omne-linux-x86_64.tar.gz | tar -xz
./omne --version
```

### 2. Package Managers
```bash
# Homebrew
brew install omne-cli && omne --version

# Cargo
cargo install omne-cli && omne --version

# Chocolatey (Windows)
choco install omne-cli && omne --version

# Snap
sudo snap install omne-cli && omne --version
```

### 3. Docker
```bash
docker run ghcr.io/omnedao/omne-cli:v0.1.0 --version
```

### 4. Documentation
Visit https://docs.omne.foundation/cli and verify content is updated.

## Troubleshooting

### Common Issues

**GitHub Actions Workflow Fails**
- Check required secrets are configured
- Verify token permissions
- Review workflow logs for specific errors

**Package Manager Updates Fail**
- Homebrew: May need manual PR submission
- Chocolatey: Requires manual approval process
- Snap: Check store credentials and permissions

**Docker Build Fails**
- Verify Dockerfile syntax
- Check multi-arch build configuration
- Ensure container registry permissions

**Documentation Deployment Fails**
- Verify Vercel configuration
- Check build process in omne-foundation repository
- Ensure deployment tokens are valid

### Getting Help

1. **GitHub Issues**: Report problems at https://github.com/OmneDAO/omne-cli/issues
2. **Discord**: Join the OMNE community Discord for real-time help
3. **Documentation**: Comprehensive guides at https://docs.omne.foundation

## Release History

| Version | Date | Notes |
|---------|------|-------|
| v0.1.0  | 2024-XX-XX | Initial release |

---

**Maintainers**: OMNE Core Team  
**Last Updated**: September 24, 2025
#!/bin/bash

# OMNE CLI Release Script
# Systematically handles all release and distribution steps

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
VERSION=${1:-"0.1.0"}
GITHUB_REPO="OmneDAO/omne-cli"
DRY_RUN=${DRY_RUN:-false}

echo -e "${BLUE}🚀 OMNE CLI Release Pipeline v${VERSION}${NC}"
echo "=============================================="

# Helper functions
log_step() {
    echo -e "\n${BLUE}📋 Step: $1${NC}"
}

log_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

log_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

log_error() {
    echo -e "${RED}❌ $1${NC}"
    exit 1
}

run_cmd() {
    if [ "$DRY_RUN" = "true" ]; then
        echo -e "${YELLOW}[DRY RUN] $1${NC}"
    else
        echo -e "${BLUE}Running: $1${NC}"
        eval "$1"
    fi
}

# Pre-flight checks
log_step "Pre-flight Checks"

# Check if we're in the right directory
if [ ! -f "Cargo.toml" ] || [ ! -f "src/main.rs" ]; then
    log_error "Must be run from the omne-cli root directory"
fi

# Check if version matches Cargo.toml
CARGO_VERSION=$(grep "^version" Cargo.toml | cut -d'"' -f2)
if [ "$VERSION" != "$CARGO_VERSION" ]; then
    log_error "Version mismatch: CLI arg ($VERSION) vs Cargo.toml ($CARGO_VERSION)"
fi

# Check required tools
for cmd in cargo git gh docker; do
    if ! command -v $cmd &> /dev/null; then
        log_error "$cmd is required but not installed"
    fi
done

log_success "Pre-flight checks passed"

# Step 1: Build and Test
log_step "Build and Test"
run_cmd "cargo test --all-features"
run_cmd "cargo build --release"
run_cmd "cargo clippy -- -D warnings"
run_cmd "cargo fmt --check"
log_success "Build and tests completed"

# Step 2: Generate Documentation and Completions  
log_step "Generate Documentation and Completions"
run_cmd "mkdir -p completions/{bash,zsh,fish,powershell}"
run_cmd "./target/release/omne completion bash > completions/bash/omne"
run_cmd "./target/release/omne completion zsh > completions/zsh/_omne" 
run_cmd "./target/release/omne completion fish > completions/fish/omne.fish"
run_cmd "./target/release/omne completion powershell > completions/powershell/omne.ps1"
log_success "Generated shell completions"

# Step 3: Create Git Tag and GitHub Release
log_step "Create Git Tag and GitHub Release"
if ! git rev-parse "v${VERSION}" >/dev/null 2>&1; then
    run_cmd "git tag -a v${VERSION} -m 'Release v${VERSION}'"
    run_cmd "git push origin v${VERSION}"
    log_success "Created and pushed git tag v${VERSION}"
else
    log_warning "Tag v${VERSION} already exists"
fi

# Trigger GitHub Actions workflow for binary builds
if [ "$DRY_RUN" != "true" ]; then
    # The workflow will be triggered automatically by the tag push
    log_success "GitHub Actions release workflow triggered"
else
    echo -e "${YELLOW}[DRY RUN] Would trigger GitHub Actions workflow${NC}"
fi

# Step 4: Publish to Crates.io (handled by GitHub Actions)
log_step "Publish to Crates.io"
if [ "$DRY_RUN" != "true" ]; then
    log_warning "Crates.io publishing will be handled by GitHub Actions"
    echo "Monitor the workflow at: https://github.com/${GITHUB_REPO}/actions"
else
    echo -e "${YELLOW}[DRY RUN] Would publish to crates.io via GitHub Actions${NC}"
fi

# Step 5: Build and Push Docker Images (handled by GitHub Actions)
log_step "Build and Push Docker Images"
if [ "$DRY_RUN" != "true" ]; then
    log_warning "Docker image building will be handled by GitHub Actions"
    echo "Images will be available at: ghcr.io/${GITHUB_REPO,,}"
else
    echo -e "${YELLOW}[DRY RUN] Would build and push Docker images via GitHub Actions${NC}"
fi

# Step 6: Update Package Managers
log_step "Update Package Managers"

# Homebrew Formula Update (manual for now)
log_warning "Homebrew formula update:"
echo "1. Fork homebrew-core if not already done"
echo "2. Update formula with new version and SHA256"
echo "3. Submit PR to homebrew-core"
echo "Formula template is in: ./homebrew/omne-cli.rb"

# Chocolatey Package (manual process)
log_warning "Chocolatey package update:"
echo "1. Update version in chocolatey/omne-cli.nuspec"
echo "2. Update checksum in chocolatey/tools/chocolateyinstall.ps1"
echo "3. Submit to Chocolatey community repository"

# Snap Package (manual process)
log_warning "Snap package update:" 
echo "1. Update version in snap/snapcraft.yaml"
echo "2. Build and publish to Snap Store"
echo "3. snapcraft login && snapcraft push omne-cli_${VERSION}_amd64.snap"

# Step 7: Deploy Documentation
log_step "Deploy Documentation to docs.omne.foundation"
if [ "$DRY_RUN" != "true" ]; then
    if [ -d "../omne-foundation" ]; then
        cd ../omne-foundation
        log_success "Found omne-foundation directory"
        
        # Trigger docs deployment
        if [ -f ".github/workflows/deploy-docs.yml" ]; then
            log_warning "Documentation deployment will be triggered by GitHub workflow"
            echo "Monitor at: https://github.com/OmneDAO/omne-foundation/actions"
        else
            log_warning "Manual documentation deployment required"
            echo "1. Update CLI documentation in src/app/docs/cli/"
            echo "2. Deploy to Vercel: npm run build && vercel --prod"
        fi
        cd ../omne-cli
    else
        log_warning "omne-foundation not found in ../omne-foundation"
        echo "Clone omne-foundation repository to deploy documentation"
    fi
else
    echo -e "${YELLOW}[DRY RUN] Would deploy documentation to docs.omne.foundation${NC}"
fi

# Step 8: Post-release Verification
log_step "Post-release Verification"

if [ "$DRY_RUN" != "true" ]; then
    echo "Verify the following after GitHub Actions complete:"
    echo "1. ✅ GitHub Release: https://github.com/${GITHUB_REPO}/releases/tag/v${VERSION}"
    echo "2. ✅ Crates.io: https://crates.io/crates/omne-cli"
    echo "3. ✅ Docker Images: https://github.com/${GITHUB_REPO}/pkgs/container/omne-cli"
    echo "4. ⏳ Homebrew: Update homebrew-core manually"
    echo "5. ⏳ Chocolatey: Submit to community repository"
    echo "6. ⏳ Snap: Build and publish to Snap Store"
    echo "7. ✅ Documentation: https://docs.omne.foundation/cli"
else
    log_success "Dry run completed successfully"
fi

# Step 9: Generate Release Notes
log_step "Generate Release Notes"
cat > "RELEASE_NOTES_v${VERSION}.md" << EOF
# OMNE CLI v${VERSION} Release Notes

## Installation

### Package Managers
\`\`\`bash
# Homebrew (macOS/Linux)
brew install omne-cli

# Cargo (Rust)
cargo install omne-cli

# Chocolatey (Windows)
choco install omne-cli

# Snap (Linux)
sudo snap install omne-cli
\`\`\`

### Docker
\`\`\`bash
docker run ghcr.io/omnedao/omne-cli:v${VERSION}
\`\`\`

### Binary Downloads
- [Linux x86_64](https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}/omne-linux-x86_64.tar.gz)
- [Linux ARM64](https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}/omne-linux-aarch64.tar.gz)
- [macOS x86_64](https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}/omne-macos-x86_64.tar.gz)
- [macOS ARM64](https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}/omne-macos-aarch64.tar.gz)
- [Windows x86_64](https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}/omne-windows-x86_64.zip)

## What's New in v${VERSION}

### Features
- Unified blockchain ecosystem orchestration
- Network operations and management
- Validator lifecycle management
- Developer tools and project scaffolding
- Infrastructure service deployment
- Operations monitoring and maintenance

### Commands
- \`omne network\` - Network-level operations
- \`omne validator\` - Validator management
- \`omne dev\` - Developer tools  
- \`omne infrastructure\` - Infrastructure services
- \`omne ops\` - Operations and monitoring

## Documentation
- [CLI Documentation](https://docs.omne.foundation/cli)
- [GitHub Repository](https://github.com/${GITHUB_REPO})
- [Issue Tracker](https://github.com/${GITHUB_REPO}/issues)

## Checksums
SHA256 checksums for all release artifacts are available in the GitHub release.
EOF

log_success "Generated release notes: RELEASE_NOTES_v${VERSION}.md"

# Summary
echo -e "\n${GREEN}🎉 OMNE CLI v${VERSION} Release Pipeline Complete!${NC}"
echo "=============================================="
echo -e "${BLUE}Next Steps:${NC}"
echo "1. Monitor GitHub Actions workflows"
echo "2. Update package managers manually where needed"
echo "3. Announce release on social media and Discord"
echo "4. Update any dependent projects"

if [ "$DRY_RUN" = "true" ]; then
    echo -e "\n${YELLOW}This was a DRY RUN. To execute for real, run:${NC}"
    echo -e "${YELLOW}./scripts/release.sh ${VERSION}${NC}"
fi
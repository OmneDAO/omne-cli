#!/bin/bash

# OMNE CLI Release Validation Script
# Tests all distribution channels after release

set -e

VERSION=${1:-"0.1.0"}
GITHUB_REPO="OmneDAO/omne-cli"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_test() {
    echo -e "\n${BLUE}🧪 Testing: $1${NC}"
}

log_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

log_failure() {
    echo -e "${RED}❌ $1${NC}"
}

log_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

echo -e "${BLUE}🔍 OMNE CLI v${VERSION} Release Validation${NC}"
echo "=============================================="

# Test 1: GitHub Release
log_test "GitHub Release"
RELEASE_URL="https://api.github.com/repos/${GITHUB_REPO}/releases/tags/v${VERSION}"
if curl -s "$RELEASE_URL" | grep -q "\"tag_name\": \"v${VERSION}\""; then
    log_success "GitHub release v${VERSION} exists"
    
    # Check binary assets
    ASSETS=$(curl -s "$RELEASE_URL" | grep -o '"browser_download_url": "[^"]*"' | wc -l)
    if [ "$ASSETS" -gt 0 ]; then
        log_success "Found $ASSETS binary assets"
    else
        log_failure "No binary assets found"
    fi
else
    log_failure "GitHub release v${VERSION} not found"
fi

# Test 2: Crates.io
log_test "Crates.io Package"
if curl -s "https://crates.io/api/v1/crates/omne-cli" | grep -q "\"max_version\": \"${VERSION}\""; then
    log_success "omne-cli v${VERSION} available on crates.io"
else
    log_warning "omne-cli v${VERSION} not yet available on crates.io (may take time to propagate)"
fi

# Test 3: Docker Images
log_test "Docker Images"
if docker manifest inspect "ghcr.io/${GITHUB_REPO,,}:v${VERSION}" >/dev/null 2>&1; then
    log_success "Docker image v${VERSION} available"
    
    # Test multi-arch
    ARCH_COUNT=$(docker manifest inspect "ghcr.io/${GITHUB_REPO,,}:v${VERSION}" | grep -c "architecture")
    log_success "Found $ARCH_COUNT architecture variants"
else
    log_warning "Docker image v${VERSION} not yet available"
fi

# Test 4: Package Managers (these may take time to propagate)
log_test "Package Managers"

# Homebrew
if command -v brew >/dev/null 2>&1; then
    if brew search omne-cli | grep -q "omne-cli"; then
        log_success "Found in Homebrew"
    else
        log_warning "Not yet available in Homebrew"
    fi
else
    log_warning "Homebrew not installed, skipping test"
fi

# Snap (requires snapd)
if command -v snap >/dev/null 2>&1; then
    if snap find omne-cli | grep -q "omne-cli"; then
        log_success "Found in Snap Store"
    else
        log_warning "Not yet available in Snap Store"
    fi
else
    log_warning "Snap not installed, skipping test"
fi

# Test 5: Documentation
log_test "Documentation Site"
if curl -s "https://docs.omne.foundation/cli" | grep -q "OMNE CLI"; then
    log_success "Documentation site accessible"
else
    log_warning "Documentation site not accessible or not updated"
fi

# Test 6: Binary Functionality
log_test "Binary Functionality"
TEMP_DIR=$(mktemp -d)
cd "$TEMP_DIR"

# Download and test a binary
BINARY_URL="https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}/omne-linux-x86_64.tar.gz"
if curl -L "$BINARY_URL" | tar -xz 2>/dev/null; then
    if ./omne --version | grep -q "$VERSION"; then
        log_success "Binary downloads and runs correctly"
    else
        log_failure "Binary version mismatch or execution failed"
    fi
else
    log_warning "Could not download or extract binary (may not be available yet)"
fi

cd - >/dev/null
rm -rf "$TEMP_DIR"

# Test 7: Shell Completions
log_test "Shell Completions"
if [ -d "completions" ] && [ -f "completions/bash/omne" ]; then
    log_success "Shell completions generated"
else
    log_warning "Shell completions not found"
fi

# Summary
echo -e "\n${BLUE}📊 Validation Summary${NC}"
echo "=============================================="
echo "Release validation completed for v${VERSION}"
echo ""
echo "✅ Items that should be immediately available:"
echo "   - GitHub Release"
echo "   - GitHub Container Registry (Docker)"
echo "   - Documentation Site"
echo ""
echo "⚠️  Items that may take time to propagate:"
echo "   - Crates.io (up to 24 hours)"
echo "   - Package Managers (varies by platform)"
echo ""
echo "📋 Next Steps:"
echo "1. Monitor package manager availability"
echo "2. Test installations from each channel"
echo "3. Announce release when all channels are live"
echo ""
echo "🔗 Useful Links:"
echo "   - Release: https://github.com/${GITHUB_REPO}/releases/tag/v${VERSION}"
echo "   - Crates.io: https://crates.io/crates/omne-cli"
echo "   - Docker: https://github.com/${GITHUB_REPO}/pkgs/container/omne-cli"
echo "   - Docs: https://docs.omne.foundation/cli"
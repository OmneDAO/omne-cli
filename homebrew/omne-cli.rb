class OmneCli < Formula
  desc "Unified command-line orchestration tool for the Omne blockchain ecosystem"
  homepage "https://omne.network"
  url "https://github.com/OmneDAO/omne-cli/archive/v0.1.0.tar.gz"
  sha256 "PLACEHOLDER_SHA256" # This will be automatically updated by the release workflow
  license "MIT OR Apache-2.0"
  head "https://github.com/OmneDAO/omne-cli.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    
    # Generate shell completions
    generate_completions_from_executable(bin/"omne", "completion")
    
    # Install man pages if they exist
    man1.install Dir["man/omne*.1"] if File.directory?("man")
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/omne --version")
    
    # Test basic functionality
    output = shell_output("#{bin}/omne --help")
    assert_match "Unified command-line orchestration tool", output
    assert_match "network", output
    assert_match "validator", output
    assert_match "dev", output
  end
end
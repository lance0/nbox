# Homebrew formula template for nbox.
#
# This is a starting point — it belongs in a tap repo (e.g. lance0/homebrew-tap as
# `Formula/nbox.rb`), not in homebrew-core. Fill in VERSION and the per-arch
# sha256 sums at release time from the release's combined `SHA256SUMS` asset.
# Linux uses the static **musl** archives (`*-unknown-linux-musl`); macOS uses
# the darwin archives. This file is not auto-updated.
class Nbox < Formula
  desc "Terminal UI and CLI for NetBox"
  homepage "https://github.com/lance0/nbox"
  version "0.1.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/lance0/nbox/releases/download/v#{version}/nbox-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_DARWIN_SHA256"
    end
    on_intel do
      url "https://github.com/lance0/nbox/releases/download/v#{version}/nbox-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_DARWIN_SHA256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/lance0/nbox/releases/download/v#{version}/nbox-aarch64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_LINUX_MUSL_SHA256"
    end
    on_intel do
      url "https://github.com/lance0/nbox/releases/download/v#{version}/nbox-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_X86_64_LINUX_MUSL_SHA256"
    end
  end

  def install
    bin.install "nbox"
    # Generate and install shell completions from the binary.
    generate_completions_from_executable(bin/"nbox", "completions")
  end

  test do
    assert_match "nbox #{version}", shell_output("#{bin}/nbox --version")
  end
end

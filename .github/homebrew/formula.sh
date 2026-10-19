#!/usr/bin/env bash
# Prints the Homebrew formula of a release, e.g. `formula.sh v0.3.0`, from the binaries attached to
# it. Needs `gh` to be logged in, or GH_TOKEN to be set.
set -euo pipefail

tag=$1
repo=abusch/sinuous
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT

gh release download "$tag" --repo "$repo" --dir "$dir" \
  --pattern 'sinuous-aarch64-apple-darwin.tar.gz' \
  --pattern 'sinuous-x86_64-apple-darwin.tar.gz' \
  --pattern 'sinuous-aarch64-unknown-linux-musl.tar.gz' \
  --pattern 'sinuous-x86_64-unknown-linux-musl.tar.gz'

url() {
  echo "https://github.com/$repo/releases/download/$tag/sinuous-$1.tar.gz"
}

sha256() {
  shasum -a 256 "$dir/sinuous-$1.tar.gz" | cut -d ' ' -f 1
}

cat <<EOF
class Sinuous < Formula
  desc "Simple TUI for controlling local Sonos speakers"
  homepage "https://github.com/$repo"
  license "MIT"

  on_macos do
    on_arm do
      url "$(url aarch64-apple-darwin)"
      sha256 "$(sha256 aarch64-apple-darwin)"
    end
    on_intel do
      url "$(url x86_64-apple-darwin)"
      sha256 "$(sha256 x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$(url aarch64-unknown-linux-musl)"
      sha256 "$(sha256 aarch64-unknown-linux-musl)"
    end
    on_intel do
      url "$(url x86_64-unknown-linux-musl)"
      sha256 "$(sha256 x86_64-unknown-linux-musl)"
    end
  end

  def install
    bin.install "sinuous"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/sinuous --version")
  end
end
EOF

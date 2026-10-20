#!/usr/bin/env bash
# Prints the PKGBUILD of the AUR package for a release, e.g. `pkgbuild.sh v0.3.0`, building it from
# the source archive of the release's tag.
set -euo pipefail

tag=$1
version=${tag#v}
repo=abusch/sinuous

sha256=$(curl -fsSL "https://github.com/$repo/archive/refs/tags/$tag.tar.gz" | sha256sum | cut -d ' ' -f 1)

# Follows https://wiki.archlinux.org/title/Rust_package_guidelines.
cat <<EOF
# Maintainer: Antoine Büsch <antoine dot busch at gmail dot com>

pkgname=sinuous
pkgver=$version
pkgrel=1
pkgdesc="A simple TUI for controlling local Sonos speakers"
url="https://github.com/$repo"
license=('MIT')
arch=('x86_64' 'aarch64')
depends=('gcc-libs' 'glibc')
makedepends=('cargo')
# The release profile already strips the binary, so the debug package would be empty.
options=('!lto' '!debug')
source=("\$pkgname-\$pkgver.tar.gz::\$url/archive/refs/tags/v\$pkgver.tar.gz")
sha256sums=('$sha256')

prepare() {
	cd "\$pkgname-\$pkgver"
	export RUSTUP_TOOLCHAIN=stable
	cargo fetch --locked --target "\$(rustc -vV | sed -n 's/host: //p')"
}

build() {
	cd "\$pkgname-\$pkgver"
	export RUSTUP_TOOLCHAIN=stable
	export CARGO_TARGET_DIR=target
	cargo build --frozen --release
}

check() {
	cd "\$pkgname-\$pkgver"
	export RUSTUP_TOOLCHAIN=stable
	cargo test --frozen
}

package() {
	cd "\$pkgname-\$pkgver"
	install -Dm755 target/release/\$pkgname -t "\$pkgdir/usr/bin/"
	install -Dm644 LICENSE -t "\$pkgdir/usr/share/licenses/\$pkgname/"
	install -Dm644 README.md -t "\$pkgdir/usr/share/doc/\$pkgname/"
}
EOF

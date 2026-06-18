#!/usr/bin/env sh
# Install nbox: download the latest release binary for this OS/arch, or fall
# back to `cargo install`. Override the install dir with NBOX_INSTALL_DIR.
set -eu

REPO="lance0/nbox"
BIN="nbox"
INSTALL_DIR="${NBOX_INSTALL_DIR:-$HOME/.local/bin}"

err() { echo "error: $*" >&2; exit 1; }

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)
            # Release ships static musl archives for Linux (portable across
            # glibc versions); see .github/workflows/release.yml.
            case "$arch" in
                x86_64|amd64)  echo "x86_64-unknown-linux-musl" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-musl" ;;
                *) return 1 ;;
            esac ;;
        Darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64)  echo "aarch64-apple-darwin" ;;
                *) return 1 ;;
            esac ;;
        *) return 1 ;;
    esac
}

cargo_fallback() {
    if command -v cargo >/dev/null 2>&1; then
        echo "Falling back to: cargo install ${BIN}"
        exec cargo install "${BIN}"
    fi
    err "no prebuilt binary for this platform and cargo is not installed"
}

target="$(detect_target)" || cargo_fallback

tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
[ -n "${tag:-}" ] || err "could not determine the latest release tag"

asset="${BIN}-${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/${tag}/${asset}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading ${asset} (${tag})..."
curl -fsSL "$url" -o "$tmp/$asset" || cargo_fallback
tar xzf "$tmp/$asset" -C "$tmp"

# The release tarball holds a bare `nbox` binary at its root, but locate it by
# search so we survive either a flat or a subdir layout.
bin_path="$(find "$tmp" -type f -name "$BIN" -print 2>/dev/null | head -n1)"
[ -n "${bin_path:-}" ] || err "could not find ${BIN} in ${asset}"

mkdir -p "$INSTALL_DIR"
install -m 0755 "$bin_path" "$INSTALL_DIR/${BIN}"

echo "Installed ${BIN} to ${INSTALL_DIR}/${BIN}"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "note: add ${INSTALL_DIR} to your PATH" ;;
esac

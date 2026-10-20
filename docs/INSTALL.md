# Installing nbox

[Documentation](README.md) · [Configuration](CONFIG.md) · [Troubleshooting](TROUBLESHOOTING.md)

nbox is one static binary with no runtime. Install it, then point it at a NetBox
4.2+ instance (see [First run](#first-run)).

## Channels

### crates.io

Requires [Rust 1.88+](https://www.rust-lang.org/tools/install):

```bash
# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

cargo install nbox
```

### Homebrew (macOS and Linux)

```bash
brew install lance0/tap/nbox
```

### Docker (GHCR)

Multi-arch image (amd64/arm64):

```bash
docker run --rm ghcr.io/lance0/nbox:latest --help
```

Pass `-e NBOX_TOKEN=...` and mount a config (or pass `--config`) to run real
lookups. Worked examples are in [Scripting: Docker](SCRIPTING.md#docker).

### Pre-built binaries

Download from [GitHub Releases](https://github.com/lance0/nbox/releases):

| Platform | Archive |
|----------|---------|
| Linux x86_64 (static musl) | `nbox-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 (static musl) | `nbox-aarch64-unknown-linux-musl.tar.gz` |
| Linux ARM64 (glibc) | `nbox-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `nbox-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `nbox-x86_64-apple-darwin.tar.gz` |
| Windows x86_64 | `nbox-x86_64-pc-windows-msvc.zip` |

`nbox-completions.tar.gz` holds shell completions and the full man-page set.

```bash
# Download, verify, and install (Linux x86_64 example)
curl -LO https://github.com/lance0/nbox/releases/latest/download/nbox-x86_64-unknown-linux-musl.tar.gz
curl -LO https://github.com/lance0/nbox/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing   # macOS: shasum -a 256 -c
tar xzf nbox-*.tar.gz && sudo mv nbox /usr/local/bin/
```

### Install script

Piping a script to `sh` skips reading it first;
[review the script](../scripts/install.sh) before running. It downloads the
latest release binary for your OS/arch into `~/.local/bin` (override with
`NBOX_INSTALL_DIR`) and falls back to `cargo install` when there is no asset for
your platform.

```bash
curl -fsSL https://raw.githubusercontent.com/lance0/nbox/master/scripts/install.sh | sh
```

### From source

```bash
git clone https://github.com/lance0/nbox
cd nbox && cargo install --path .
```

## Build features

`cargo install nbox` builds the one canonical binary: clipboard, the MCP server
(stdio and HTTP), and update checks are on by default. There are no variant
builds to choose between.

| Feature | Default | Description |
|---------|---------|-------------|
| `clipboard` | Yes | Copy values with `y` in the TUI (desktop clipboard via `arboard`; OSC 52 fallback over SSH/headless) |
| `http` | Yes | `nbox serve --http` MCP transport (+ OIDC) |
| `updates` | Yes | GitHub update notifications |

```bash
# Lean stdio-only build (drops all of the above)
cargo install nbox --no-default-features
```

The API token lives in `config.toml` or an env var; there is no OS-keyring
storage. See [Configuration: Tokens](CONFIG.md#tokens).

## Shell completions

```bash
# Bash
nbox completions bash > ~/.local/share/bash-completion/completions/nbox

# Zsh (add ~/.zfunc to fpath in .zshrc first)
nbox completions zsh > ~/.zfunc/_nbox

# Fish
nbox completions fish > ~/.config/fish/completions/nbox.fish

# PowerShell (append to $PROFILE)
nbox completions powershell >> $PROFILE

# Elvish
nbox completions elvish > ~/.elvish/lib/nbox.elv
```

## Man pages

`nbox man > nbox.1` writes the top-level page. Pass a directory to write the
full set instead, `nbox.1` plus one `nbox-<subcommand>.1` per subcommand, so
`man nbox-device` works once installed:

```bash
nbox man man/
```

## First run

Launch `nbox` with no config and the TUI runs a first-run wizard: it captures a
profile, test-connects it, writes `config.toml`, and continues into the app. A
pasted token is saved user-only (`0600` on Unix) and redacted by `config show`.

From the shell instead:

```bash
nbox profile add work https://netbox.example.com --token-env NETBOX_TOKEN
nbox profile use work
export NETBOX_TOKEN=...
nbox status                # reachability, NetBox version, token check
```

Profiles, token precedence, and every config key are in
[Configuration](CONFIG.md).

## Platform support

| Platform | Status |
|----------|--------|
| Linux x86_64, ARM64 | Release binaries (static musl; glibc for ARM64), Homebrew, GHCR image |
| macOS (Apple Silicon, Intel) | Release binaries, Homebrew |
| Windows x86_64 | Release binary; config at `%APPDATA%\nbox\config.toml` |

nbox needs NetBox **4.2+**. Version-by-version API differences are in
[Compatibility](COMPATIBILITY.md).

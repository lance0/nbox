#!/usr/bin/env bash
# bench-coldstart.sh — cold-start and footprint benchmark for the nbox MCP server.
#
# nbox ships as one statically-linked binary with zero runtime: it drops into any
# agent sandbox and is ready to answer MCP calls the instant it spawns. The Python
# MCP field pays an interpreter start plus a stack of imports before its server can
# respond. This script measures that gap with real numbers.
#
# What it measures (lower is better), each as median + min over RUNS samples:
#   1. nbox process start          — `nbox --version`: native binary spawn + exit.
#   2. nbox serve → MCP-ready       — spawn `nbox serve` (stdio) and time the round
#                                     trip to the `initialize` response. This is the
#                                     real "ready to take MCP calls" signal, offline
#                                     (initialize touches no network).
#   3. python interpreter floor     — `python -c pass`: the bare interpreter.
#   4. python NetBox-MCP import floor — `python -c "import mcp.server.fastmcp,
#                                     pynetbox"`: the imports any Python NetBox MCP
#                                     server pays *before* it can initialize. This is
#                                     a conservative lower bound on the Python cold
#                                     start — the real server adds its own init on
#                                     top — so comparing nbox's full MCP-ready time
#                                     (2) against this floor understates nbox's lead.
#
# Plus footprint: the static nbox binary (one file) vs the Python venv site-packages
# (interpreter not even counted).
#
# Reproduce:
#   cargo build --release --target x86_64-unknown-linux-musl
#   python3 -m venv /tmp/pybench-venv && /tmp/pybench-venv/bin/pip install mcp pynetbox
#   scripts/bench-coldstart.sh
#
# Knobs (env): RUNS (default 30), WARMUP (3), NBOX (binary path), PYBENCH_PY (python).
# Requires GNU `date` (nanoseconds) — i.e. Linux/coreutils. On macOS run in a Linux
# container or install coreutils and point this at `gdate`.
set -euo pipefail

RUNS="${RUNS:-30}"
WARMUP="${WARMUP:-3}"

# --- resolve the nbox binary (prefer the static musl release) -----------------
NBOX="${NBOX:-}"
if [ -z "$NBOX" ]; then
  for c in \
    target/x86_64-unknown-linux-musl/release/nbox \
    target/aarch64-unknown-linux-musl/release/nbox \
    target/release/nbox \
    "$(command -v nbox 2>/dev/null || true)"; do
    [ -n "$c" ] && [ -x "$c" ] && { NBOX="$c"; break; }
  done
fi
if ! { [ -n "$NBOX" ] && [ -x "$NBOX" ]; }; then
  echo "bench: no nbox binary (set NBOX=path)" >&2; exit 1
fi

# --- resolve python + check the NetBox-MCP stack ------------------------------
PYBIN="${PYBENCH_PY:-}"
if [ -z "$PYBIN" ]; then
  for c in /tmp/pybench-venv/bin/python "$(command -v python3 2>/dev/null || true)"; do
    [ -n "$c" ] && [ -x "$c" ] && { PYBIN="$c"; break; }
  done
fi
PY_MCP_OK=0
if [ -n "${PYBIN:-}" ] && [ -x "$PYBIN" ] && "$PYBIN" -c "import mcp.server.fastmcp, pynetbox" 2>/dev/null; then
  PY_MCP_OK=1
fi

# --- a throwaway config so `nbox serve` can build its (lazy) client -----------
CFG="$(mktemp -d)/bench.toml"
printf '%s\n' \
  'config_version = 1' 'active_profile = "b"' '' \
  '[profiles.b]' 'url = "http://127.0.0.1:1"' 'token = "x"' 'auth_scheme = "auto"' > "$CFG"
trap 'rm -rf "$(dirname "$CFG")"' EXIT

INIT='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"bench","version":"0"}}}'

now_ns() { date +%s%N; }

# bench <label> <cmd...> — warm up, then time RUNS samples; print median + min (ms).
bench() {
  local label="$1"; shift
  local i t0 t1 samples; samples="$(mktemp)"
  for ((i = 0; i < WARMUP; i++)); do "$@" >/dev/null 2>&1 || true; done
  for ((i = 0; i < RUNS; i++)); do
    t0=$(now_ns); "$@" >/dev/null 2>&1 || true; t1=$(now_ns)
    echo $(( t1 - t0 )) >> "$samples"   # nanoseconds
  done
  sort -n "$samples" | awk -v L="$label" '
    {a[NR] = $1}
    END {
      n = NR
      med = (n % 2) ? a[(n + 1) / 2] : (a[n / 2] + a[n / 2 + 1]) / 2
      printf "  %-38s  median %8.2f ms    min %8.2f ms\n", L, med / 1e6, a[1] / 1e6
    }'
  rm -f "$samples"
}

# commands under test (functions so pipelines work as a single bench target)
nbox_version()     { "$NBOX" --version; }
nbox_serve_ready() { printf '%s\n' "$INIT" | timeout 10 "$NBOX" serve --config "$CFG" 2>/dev/null | head -n1 >/dev/null; }
py_floor()         { "$PYBIN" -c "pass"; }
py_mcp_floor()     { "$PYBIN" -c "import mcp.server.fastmcp, pynetbox"; }

dir_size() { du -sh "$1" 2>/dev/null | cut -f1; }

echo "=============================================================================="
echo " nbox cold-start & footprint benchmark"
echo "=============================================================================="
echo " host    : $(uname -srm)"
echo " cpu     : $(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ //' || echo '?')"
echo " nbox    : $NBOX ($("$NBOX" --version 2>/dev/null))"
if [ "$PY_MCP_OK" = 1 ]; then
  echo " python  : $PYBIN ($("$PYBIN" --version 2>&1)) — mcp + pynetbox present"
else
  echo " python  : ${PYBIN:-none} — mcp/pynetbox NOT importable (NetBox-MCP floor skipped)"
fi
echo " samples : $RUNS (after $WARMUP warmup)"
echo

echo "Cold start (lower is better):"
bench "nbox process start (--version)"        nbox_version
bench "nbox serve → MCP initialize-ready"     nbox_serve_ready
if [ -n "${PYBIN:-}" ] && [ -x "$PYBIN" ]; then
  bench "python interpreter floor (-c pass)"  py_floor
  [ "$PY_MCP_OK" = 1 ] && bench "python NetBox-MCP import floor"  py_mcp_floor
fi
echo

echo "Footprint (one file, zero runtime vs. interpreter + packages):"
nbox_bytes=$(stat -c%s "$NBOX" 2>/dev/null || wc -c < "$NBOX")
printf "  %-38s  %s\n" "nbox static binary" "$(numfmt --to=iec "$nbox_bytes" 2>/dev/null || echo "${nbox_bytes}B") (one file, no runtime deps)"
if [ "$PY_MCP_OK" = 1 ]; then
  sp="$("$PYBIN" -c 'import site,sys; print(sys.prefix)')"
  printf "  %-38s  %s\n" "python venv site-packages" "$(dir_size "$sp/lib") (excludes the interpreter)"
fi
echo "=============================================================================="

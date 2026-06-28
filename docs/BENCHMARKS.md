# Benchmarks

nbox ships as one statically-linked binary with zero runtime. The point of this
page is to put a number on what that buys an agent: how fast the MCP server is
ready to answer, and how little it weighs. An agent host spawns the server fresh
for each session — often in a throwaway sandbox — so cold start is paid every
time, not amortized.

This measures **cold start and footprint**, nbox's structural edge. It does *not*
measure per-call latency, which is dominated by NetBox itself (the API round
trip), not the client — so it would say little about nbox specifically.

## Method

Four launch-to-ready times, each the median (and min) of 30 samples after 3
warmup runs, plus on-disk footprint. Reproduce with
[`scripts/bench-coldstart.sh`](../scripts/bench-coldstart.sh):

```bash
cargo build --release --target x86_64-unknown-linux-musl
python3 -m venv /tmp/pybench-venv && /tmp/pybench-venv/bin/pip install mcp pynetbox
scripts/bench-coldstart.sh
```

What each row is:

- **nbox process start** — `nbox --version`: the native binary spawning and
  exiting. The floor for "the binary ran at all."
- **nbox serve → MCP-ready** — spawn `nbox serve` (stdio) and time the round trip
  to the `initialize` response. This is the real "ready to take MCP calls"
  signal. It touches no network (`initialize` is a pure handshake).
- **python interpreter floor** — `python -c pass`: the bare interpreter, no
  imports. A Python MCP server can never start faster than this.
- **python NetBox-MCP import floor** — `python -c "import mcp.server.fastmcp,
  pynetbox"`: the imports any Python NetBox MCP server pays *before* it can
  initialize its server. This is a **conservative lower bound** on the Python
  cold start — the real server adds argument parsing, config, and server setup on
  top — so comparing nbox's *full* MCP-ready time against this *imports-only*
  floor understates nbox's lead, not the reverse.

## Results

Host: AMD Ryzen Threadripper 7970X, Linux 6.17 x86_64. nbox 0.13.0 (static musl),
Python 3.12.3, `mcp` + `pynetbox` from PyPI.

| Cold start (lower is better)        | median  | min     |
|-------------------------------------|--------:|--------:|
| nbox process start (`--version`)    | 5.6 ms  | 5.5 ms  |
| **nbox serve → MCP-ready**          | **9.4 ms** | 8.9 ms |
| python interpreter floor (`-c pass`)| 8.1 ms  | 7.7 ms  |
| **python NetBox-MCP import floor**  | **292 ms** | 285 ms |

| Footprint                  | size  | notes                          |
|----------------------------|------:|--------------------------------|
| nbox static binary         | 17 MB | one file, no runtime deps      |
| python venv site-packages  | 60 MB | excludes the interpreter itself|

## Reading it

nbox is fully ready to answer MCP calls in about **9 ms** — before a Python
NetBox MCP server has finished *importing its dependencies* (~292 ms), let alone
started its server. That is roughly a **30×** gap on the conservative side, and
it is paid on every cold start: every new agent session, every fresh sandbox, in
CI.

The footprint compounds it. nbox is a single 17 MB file you drop onto a jump host
or into a scratch container with nothing else — no interpreter, no `pip install`,
no virtualenv. The Python path needs an interpreter plus ~60 MB of packages
before it runs.

None of this makes nbox "faster than Python" at the work that matters once
warm — both then wait on NetBox. It makes nbox cheap to *start*, which is exactly
the cost an agent pays most often.

# nbox — README demo cassette (TUI walkthrough)
#
# Rendered with VHS (https://github.com/charmbracelet/vhs):
#   vhs docs/demo.tape
#
# A ~25s walk through the k9s-style TUI: launch → search → device detail →
# overview dashboard → hierarchical prefix tree.
#
# Reproduce: boot the integration fixture
#   docker compose -f tests/integration/docker-compose.yml up -d
#   ./tests/integration/wait-for-ready.sh && ./tests/integration/seed.py
# then export that instance's token and render:
#   export NBOX_TOKEN=<token>; vhs docs/demo.tape
# The token is read from the environment, so it never lands in this file, and the
# recording only ever touches the throwaway test instance.

Output docs/demo.gif

# A clean, fixed-size terminal sized for the three-pane TUI at README width.
Set Width 1280
Set Height 760
Set FontSize 15
Set Padding 16
Set Theme "Catppuccin Mocha"
Set CursorBlink false

Require nbox

# ---------------------------------------------------------------------------
# Hidden setup: a temp config pointing the TUI at the test NetBox. NBOX_TOKEN is
# inherited from the environment vhs runs in (so no token lands in the GIF or
# this file).
# ---------------------------------------------------------------------------
Hide
Type `export NBOX_CONFIG=$(mktemp -d)/demo.toml && printf '%s\n' 'config_version = 1' 'active_profile = "demo"' '' '[profiles.demo]' 'url = "http://localhost:8000"' 'token_env = "NBOX_TOKEN"' 'auth_scheme = "auto"' 'verify_tls = true' 'timeout_secs = 15' > $NBOX_CONFIG`
Enter
Sleep 400ms
Type "clear"
Enter
Sleep 400ms
Show

# Launch the interactive TUI (bare `nbox`; --config points it at the test data).
Type@50ms "nbox --config $NBOX_CONFIG"
Enter
Sleep 3500ms

# Search across object kinds.
Type "/"
Sleep 700ms
Type@90ms "ci-dev1"
Sleep 1200ms
Enter
Sleep 3000ms

# Open the device → one lookup, all the context (interfaces / IPs / cables /
# VLANs / services on tabs).
Enter
Sleep 3500ms
Tab
Sleep 2200ms
Tab
Sleep 2200ms

# The hierarchical prefix tree (T): the /16 with its 25 children and live
# utilization bars. Give the fetch time to land, then scroll a little.
Type "T"
Sleep 5500ms
Down
Sleep 600ms
Down
Sleep 600ms
Down
Sleep 1800ms

# Quit.
Type "q"
Sleep 1200ms

# ---------------------------------------------------------------------------
# Hidden teardown: drop the temp config.
# ---------------------------------------------------------------------------
Hide
Type "rm -rf $(dirname $NBOX_CONFIG); unset NBOX_CONFIG"
Enter
Show
Sleep 300ms

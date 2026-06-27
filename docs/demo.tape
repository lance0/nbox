# nbox — README demo cassette
#
# Rendered with VHS (https://github.com/charmbracelet/vhs):
#   vhs docs/demo.tape
#
# This tape drives a ~45s walkthrough of nbox's key flows:
#   status → search → device detail → IP lookup → dry-run write plan.
#
# The commands run against a throwaway test NetBox instance. A hidden setup
# block (below) writes a temporary config pointing nbox at it and exports a
# read-only token, so the recording is reproducible and never touches a real
# production NetBox. Re-record after wiring a test instance: set the URL/token
# in the Hide block, then `vhs docs/demo.tape`.

Output docs/demo.gif
Output docs/demo.webm

# A clean, fixed-size terminal. 1200x600 fits the widest device/IP detail
# without wrapping, and renders legibly at README width.
Set Width 1200
Set Height 600
Set FontSize 16
Set FontFamily "JetBrains Mono"
Set Padding 20
Set Theme "Catppuccin Mocha"
Set CursorBlink false

# Required programs — fail fast in CI if nbox or jq are missing.
Require nbox
Require jq

# ---------------------------------------------------------------------------
# Hidden setup: point nbox at a test NetBox instance for the duration of the
# recording. Nothing here is captured in the GIF. Swap the URL/token for your
# test instance before re-recording.
# ---------------------------------------------------------------------------
Hide
Type `export NBOX_CONFIG=$(mktemp -d)/demo.toml NBOX_TOKEN=nbt_demo_readonly NBOX_TEST_URL=http://localhost:8000 && printf '%s\n' 'config_version = 1' 'active_profile = "demo"' '' '[profiles.demo]' 'url = "http://localhost:8000"' 'token_env = "NBOX_TOKEN"' 'auth_scheme = "auto"' 'verify_tls = true' 'timeout_secs = 15' > $NBOX_CONFIG`
Enter
Sleep 500ms
Type "echo 'nbox demo — profile ready against test NetBox at '$NBOX_TEST_URL"
Enter
Sleep 500ms
Show

# Clear the prompt so the recording opens clean.
Type "clear"
Enter
Sleep 500ms

# ---------------------------------------------------------------------------
# 1. Connection + versions
# ---------------------------------------------------------------------------
Type@40ms "nbox --config $NBOX_CONFIG status"
Sleep 500ms
Enter
Sleep 3s

Type "clear"
Enter
Sleep 500ms

# ---------------------------------------------------------------------------
# 2. Ranked, deduped search across object kinds
# ---------------------------------------------------------------------------
Type@40ms "nbox --config $NBOX_CONFIG search edge --json | jq '.[0:3] | map({kind, display})'"
Sleep 500ms
Enter
Sleep 3s

Type "clear"
Enter
Sleep 500ms

# ---------------------------------------------------------------------------
# 3. Device detail (plain text — one lookup, all the context)
# ---------------------------------------------------------------------------
Type@40ms "nbox --config $NBOX_CONFIG device edge01"
Sleep 500ms
Enter
Sleep 3s

Type "clear"
Enter
Sleep 500ms

# ---------------------------------------------------------------------------
# 4. IP lookup with --fields projection (only the columns you want)
# ---------------------------------------------------------------------------
Type@40ms "nbox --config $NBOX_CONFIG ip 10.44.208.55 --fields address,parent_prefix,scope --json"
Sleep 500ms
Enter
Sleep 3s

Type "clear"
Enter
Sleep 500ms

# ---------------------------------------------------------------------------
# 5. Safe write: dry-run plan + diff (no mutation)
#    --dry-run previews the PATCH without --allow-writes or confirmation.
# ---------------------------------------------------------------------------
Type@40ms "nbox --config $NBOX_CONFIG device edge01 set status active --dry-run --json"
Sleep 500ms
Enter
Sleep 3s

# ---------------------------------------------------------------------------
# Hidden teardown: drop the temp config so nothing leaks out of the recording.
# ---------------------------------------------------------------------------
Hide
Type "rm -f $NBOX_CONFIG && unset NBOX_TOKEN NBOX_TEST_URL NBOX_CONFIG"
Enter
Sleep 500ms
Show

# Final beat.
Sleep 2s

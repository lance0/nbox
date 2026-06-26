# Known Issues

Known limitations and edge cases — documented, not yet addressed. See ROADMAP.md
for where they're headed.

### Writes are narrow and opt-in (ADR-0001)

**Status:** Seven safe-write commands have landed (`interface set description`,
`device set status`, `ip reserve`, `prefix reserve`, `ip-range reserve`,
`tag add`, `tag remove`), behind `--allow-writes` + `--confirm` (or `--dry-run`
to preview). Reads remain the default everywhere.

**Remaining limitations:** Multi-IP allocation (`--count N`), choosing a
specific address/block, interface/VM assignment, status/tags on reserve, and
generic create/delete are still deferred (ADR-0001 Decision 6). The MCP server
stays read-only.

---

### Parent-prefix enrichment is a best-effort longest match

**Issue:** `nbox ip` computes the parent prefix locally (longest match) from the
prefixes containing the address, scoped to the IP's VRF (or the global table).

**Impact:** If NetBox data has unusual/overlapping containment, the chosen parent
(and the VLAN/site derived from it) is the most-specific match, which may not be
the one you expect.

**Mitigation:** The full prefix is shown; cross-check with `nbox prefix <cidr>`.

---

### Fuzzy name lookups pick by exact-then-contains

**Issue:** Name lookups try exact (case-insensitive) first, then a "contains"
fallback. A contains-fallback that matches more than one object is reported as
ambiguous (exit 5) rather than guessed.

**Impact:** A short query may error as ambiguous instead of returning a result.

**Mitigation:** Be more specific, or use an ID / exact name.

---

### Device-by-name misses when the display carries an asset tag

**Issue:** Device references resolve against NetBox's `name` (plus slug / numeric
id), not the API `display`. Some instances decorate `display` with a suffix the
`name` doesn't carry — e.g. an asset tag, so a device named `edge01` shows as
`edge01 (m0001)`. A reference copied from that decorated display string won't
match. This affects `nbox device <ref>` and anywhere a device ref is a component
(the interface `<device>/<name>` ref, MAC/interface resolution).

**Impact:** Pasting a device's *displayed* string (with the suffix) returns
not-found (exit 4) even though the device exists; the bare name resolves fine.

**Mitigation:** Use the device's bare `name` (the part before the decoration) or
its numeric id — `nbox search` and `nbox device` show the canonical name. A
`device_by_ref` fallback that strips a trailing ` (…)` suffix is a candidate fix.

---

### Sub-resource lists are capped

**Issue:** Device interfaces/IPs/services and prefix children/IPs are capped
(200 and 50 respectively) per request.

**Impact:** Very large devices/prefixes show a truncated section.

**Mitigation:** Use `nbox raw GET` with paging for the full set.

---

### CSV is tabular-only

**Issue:** `-o csv` renders arrays (lists) as a table. A single object is
rejected with a usage error (exit 2) rather than a `field,value` fallback —
there's no good flat CSV shape for one nested record.

**Impact:** CSV is for list results like `search`; single detail objects can't
be CSV.

**Mitigation:** Use `--json` (or plain) for single objects and nested data.

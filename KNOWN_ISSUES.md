# Known Issues

Known limitations and edge cases — documented, not yet addressed. See ROADMAP.md
for where they're headed.

### Read-only (no writes yet)

**Issue:** v0.1 is read-only. There is no way to create, edit, or delete objects.

**Impact:** Allocation (`next-ip`/`next-prefix`) shows candidates but doesn't
claim them; `tags`/`journal` are list-only.

**Mitigation:** Safe, diff-confirmed `PATCH` writes are planned for v0.2.

---

### Structured search filters aren't VRF/scope-aware

**Issue:** `nbox search --status/--site/--tenant/--role/--tag` map to NetBox query
params, but `search` does not take `--vrf`. Only the exact lookups (`nbox ip`,
`nbox prefix`, `nbox vlan`) accept scope flags (`--vrf`, `--site`, `--group`).

**Impact:** In a VRF-heavy instance, `search` can return overlapping addresses
across VRFs with no way to narrow by VRF from `search`.

**Mitigation:** Use the exact lookup with `--vrf`, or `nbox raw GET` with explicit
params. A server-side `--vrf` filter on `search`/list is planned for v0.3.

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

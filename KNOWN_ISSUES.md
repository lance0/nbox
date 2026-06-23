# Known Issues

Known limitations and edge cases — documented, not yet addressed. See ROADMAP.md
for where they're headed.

### Read-only (no writes yet)

**Issue:** nbox is read-only. There is no way to create, edit, or delete objects.

**Impact:** Allocation (`next-ip`/`next-prefix`) shows candidates but doesn't
claim them; `tags`/`journal` are list-only.

**Mitigation:** Safe, diff-confirmed `PATCH` writes are planned for a later release.

---

### Search scope/VRF filters are exact, not hierarchical

**Issue:** `nbox search` takes `--vrf` (resolves id/RD/name, filters IP/prefix by
`vrf_id=`) and the scope flags `--site/--region/--site-group/--location` (resolve
the ref once, filter prefixes by `scope_type`+`scope_id`). The scope match is
**exact**: `--region` filters by that region's own scope only — it does not pull
in prefixes scoped to sites *inside* the region. At most one scope flag may be set
at a time.

**Impact:** A hierarchical question ("everything under region X") needs more than
one query, or an id-based filter on an endpoint that supports it.

**Mitigation:** Filter at the level the object is scoped, combine with `--vrf`, or
use `nbox raw GET` with explicit params. Descendant/hierarchy expansion is not
implemented.

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

### Browse lists can skip rows if the server's page-size ceiling is below the cap

**Issue:** A Nav-rail browse pulls up to `BROWSE_CAP` (1000) rows, and `list_all`
advances its offset by the requested page size, not by the rows actually returned.
If a NetBox server lowers `MAX_PAGE_SIZE` below the requested limit, it returns a
short page; the next offset overshoots and the rows in the gap are skipped. Default
NetBox caps responses at 1000 and honors limits up to it, so a cap-sized browse
fits one page and the gap can't open there.

**Impact:** On a NetBox configured with `MAX_PAGE_SIZE` < 1000, browsing a kind with
more rows than that ceiling may silently omit some (no error). Single-object detail
lookups are unaffected — this is specific to the capped browse list. The cap raise
(500 → 1000) widened the window slightly; the behavior itself is pre-existing.

**Mitigation:** Keep NetBox's `MAX_PAGE_SIZE` ≥ 1000 (the default), or narrow the
browse with the name filter (`/`) so the result fits one page. The robust fix —
following the API's `next` link instead of computing offsets — is planned with
load-more browsing.

---

### CSV is tabular-only

**Issue:** `-o csv` renders arrays (lists) as a table. A single object is
rejected with a usage error (exit 2) rather than a `field,value` fallback —
there's no good flat CSV shape for one nested record.

**Impact:** CSV is for list results like `search`; single detail objects can't
be CSV.

**Mitigation:** Use `--json` (or plain) for single objects and nested data.

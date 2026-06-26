#!/usr/bin/env python3
"""Seed a small, deterministic fixture into a live NetBox 4.2.x for nbox's
integration tests.

Uses only the Python standard library (urllib) so CI needs no extra packages.
Idempotent: each object is looked up by a natural key first and only created if
absent, so re-running the seed against an already-seeded instance is a no-op.

Objects created, in dependency order:
  - a tag                         (slug: ci-tag)
  - a region                      (slug: ci-region)
  - a site-group                  (slug: ci-sitegroup)
  - a site                        (slug: ci-site, in ci-region AND ci-sitegroup)
  - a location + child location   (slugs: ci-loc, ci-loc-child; inside ci-site)
  - a VRF                         (name: ci-vrf)
  - a site-scoped prefix          (10.10.0.0/16, scope_type=dcim.site)
  - region/site-group/location    (10.20/10.30/10.40.0.0/16 scoped to the
    scoped prefixes                selected scopes, plus 10.41.0.0/16 scoped to
                                   ci-loc-child)
  - a duplicate prefix            (10.0.0.0/24 in ci-vrf AND in the global table)
  - a VLAN                        (vid 1234 "ci-vlan" at ci-site)
  - a duplicate VLAN              (vid 1234 "ci-vlan2" at ci-site2 — exit-5 case)
  - a scoped VLAN group + VLAN    (ci-vgroup scoped to ci-region; vid 1300 in it,
                                   surfaces group_scope / group_scope_type)
  - manufacturer/type/role/device (ci-mfg / ci-model / ci-role / ci-dev1)
  - an interface                  (xe-0/0/1 on ci-dev1 — name has a slash)
  - an IP on that interface       (10.10.0.5/24, set as the device's primary)
  - a journal entry on ci-dev1    (for `nbox journal` / `--journal`)
  - 25 child prefixes             (10.10.<n>.0/24, nested under the scoped /16)
                                   to force >1 page of pagination on the prefix
                                   detail's child-prefix list (uses list_all)
  - an exhausted /32 prefix       (10.50.0.1/32, sole address assigned — next-ip
                                   graceful-empty edge)
  - a free /28 prefix             (10.60.0.0/28 — next-ip --count / next-prefix
                                   --length edges)

Env:
  NBOX_URL       base URL (default http://localhost:8000)
  NETBOX_TOKEN   API token (default matches docker-compose.yml's SUPERUSER_API_TOKEN)
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

NBOX_URL = os.environ.get("NBOX_URL", "http://localhost:8000").rstrip("/")
TOKEN = os.environ.get(
    "NETBOX_TOKEN", "0123456789abcdef0123456789abcdef0fedcba9"
)

# How many child prefixes to create under the scoped /16 so a small-page_size
# list of them spans several pages. The gated pagination test drives the prefix
# detail with page_size=5, so 25 children walk five offset windows.
FILLER_PREFIX_COUNT = 25


def _request(method, path, body=None):
    url = f"{NBOX_URL}/api{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Token {TOKEN}")
    req.add_header("Accept", "application/json")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")
        raise SystemExit(
            f"{method} {url} failed: HTTP {e.code}\n{detail}"
        ) from e


def get(path):
    return _request("GET", path)


def post(path, body):
    return _request("POST", path, body)


def patch(path, body):
    return _request("PATCH", path, body)


def find_one(endpoint, **params):
    """Return the first object on `endpoint` matching `params`, or None."""
    query = "&".join(f"{k}={urllib.parse.quote(str(v))}" for k, v in params.items())
    page = get(f"{endpoint}?{query}")
    results = page.get("results", [])
    return results[0] if results else None


def ensure(endpoint, lookup, body):
    """Idempotent create: find by `lookup`, else POST `body`. Returns the object."""
    existing = find_one(endpoint, **lookup)
    if existing:
        return existing
    return post(endpoint, body)


def brief_id(value):
    """Return an id from either a nested NetBox brief object or a raw id."""
    if isinstance(value, dict):
        return value.get("id")
    return value


def main():
    print(f"Seeding {NBOX_URL} ...")

    # --- tag -----------------------------------------------------------------
    tag = ensure(
        "/extras/tags/",
        {"slug": "ci-tag"},
        {"name": "ci-tag", "slug": "ci-tag", "color": "00ff00"},
    )
    print(f"tag: {tag['slug']} (id {tag['id']})")

    # --- region / site-group (the site's polymorphic-scope parents) ----------
    # The region and site-group both contain ci-site; the location lives inside
    # ci-site. Search scope filters use NetBox's tree-aware region_id /
    # site_group_id / location_id filters for non-site scopes, so the live tests
    # assert both selected-scope hits and descendant hits.
    region = ensure(
        "/dcim/regions/",
        {"slug": "ci-region"},
        {"name": "ci-region", "slug": "ci-region"},
    )
    print(f"region: {region['slug']} (id {region['id']})")

    site_group = ensure(
        "/dcim/site-groups/",
        {"slug": "ci-sitegroup"},
        {"name": "ci-sitegroup", "slug": "ci-sitegroup"},
    )
    print(f"site-group: {site_group['slug']} (id {site_group['id']})")

    # --- site (in the region AND the site-group) -----------------------------
    site = ensure(
        "/dcim/sites/",
        {"slug": "ci-site"},
        {
            "name": "ci-site",
            "slug": "ci-site",
            "status": "active",
            "region": region["id"],
            "group": site_group["id"],
        },
    )
    # Idempotently attach the region/group to a pre-existing site (a fresh DB
    # gets them from the body above; a re-seed of an older fixture gets patched).
    if site.get("region") is None or site.get("group") is None:
        site = patch(
            f"/dcim/sites/{site['id']}/",
            {"region": region["id"], "group": site_group["id"]},
        )
    print(f"site: {site['slug']} (id {site['id']}) in ci-region / ci-sitegroup")

    # --- location inside the site --------------------------------------------
    # A location is polymorphically usable as a prefix scope and must belong to
    # a site.
    location = ensure(
        "/dcim/locations/",
        {"slug": "ci-loc"},
        {"name": "ci-loc", "slug": "ci-loc", "site": site["id"], "status": "active"},
    )
    print(f"location: {location['slug']} (id {location['id']}) at ci-site")

    child_location = ensure(
        "/dcim/locations/",
        {"slug": "ci-loc-child"},
        {
            "name": "ci-loc-child",
            "slug": "ci-loc-child",
            "site": site["id"],
            "parent": location["id"],
            "status": "active",
        },
    )
    if brief_id(child_location.get("parent")) != location["id"] or brief_id(
        child_location.get("site")
    ) != site["id"]:
        child_location = patch(
            f"/dcim/locations/{child_location['id']}/",
            {"site": site["id"], "parent": location["id"]},
        )
    print(
        f"child location: {child_location['slug']} "
        f"(id {child_location['id']}) under ci-loc"
    )

    # --- VRF -----------------------------------------------------------------
    vrf = ensure("/ipam/vrfs/", {"name": "ci-vrf"}, {"name": "ci-vrf"})
    print(f"vrf: {vrf['name']} (id {vrf['id']})")

    # --- site-scoped prefix (polymorphic scope: scope_type + scope_id) -------
    scoped_prefix = ensure(
        "/ipam/prefixes/",
        {"prefix": "10.10.0.0/16"},
        {
            "prefix": "10.10.0.0/16",
            "status": "active",
            "scope_type": "dcim.site",
            "scope_id": site["id"],
            "tags": [{"slug": "ci-tag"}],
        },
    )
    print(f"scoped prefix: {scoped_prefix['prefix']} (scope=dcim.site:{site['id']})")

    # --- region / site-group / location scoped prefixes ----------------------
    # Prefixes scoped to the selected non-site scopes plus a child location. The
    # live tests assert that NetBox's native tree filters include descendants:
    # region/site-group find the site-scoped 10.10/16, and location finds the
    # child-location-scoped 10.41/16.
    for cidr, ct, obj, label in [
        ("10.20.0.0/16", "dcim.region", region, "region"),
        ("10.30.0.0/16", "dcim.sitegroup", site_group, "site-group"),
        ("10.40.0.0/16", "dcim.location", location, "location"),
        ("10.41.0.0/16", "dcim.location", child_location, "child-location"),
    ]:
        p = ensure(
            "/ipam/prefixes/",
            {"prefix": cidr},
            {
                "prefix": cidr,
                "status": "active",
                "scope_type": ct,
                "scope_id": obj["id"],
            },
        )
        print(f"{label}-scoped prefix: {p['prefix']} (scope={ct}:{obj['id']})")

    # --- duplicate prefix: same CIDR in ci-vrf AND the global table -----------
    # NetBox allows duplicate prefixes when they live in different VRFs (or one
    # in a VRF and one global). This exercises --vrf disambiguation / ambiguity.
    dup_in_vrf = find_one("/ipam/prefixes/", prefix="10.0.0.0/24", vrf_id=vrf["id"])
    if not dup_in_vrf:
        dup_in_vrf = post(
            "/ipam/prefixes/",
            {"prefix": "10.0.0.0/24", "status": "active", "vrf": vrf["id"]},
        )
    dup_global = find_one("/ipam/prefixes/", prefix="10.0.0.0/24", vrf_id="null")
    if not dup_global:
        dup_global = post(
            "/ipam/prefixes/",
            {"prefix": "10.0.0.0/24", "status": "active", "vrf": None},
        )
    print(
        f"duplicate prefix 10.0.0.0/24 in vrf ci-vrf (id {dup_in_vrf['id']}) "
        f"and global (id {dup_global['id']})"
    )

    # --- VLAN at the site ----------------------------------------------------
    vlan = find_one("/ipam/vlans/", vid=1234, site_id=site["id"])
    if not vlan:
        vlan = post(
            "/ipam/vlans/",
            {"vid": 1234, "name": "ci-vlan", "status": "active", "site": site["id"]},
        )
    print(f"vlan: vid {vlan['vid']} ({vlan['name']}) at ci-site")

    # --- duplicate VLAN VID across two sites (for exit-5 ambiguity) -----------
    # A second site holding the SAME vid 1234. A bare `nbox vlan 1234` is then
    # ambiguous (exit 5) and `--site ci-site` / `--site ci-site2` disambiguates.
    # NetBox allows a vid to repeat across sites (uniqueness is per scope).
    site2 = ensure(
        "/dcim/sites/",
        {"slug": "ci-site2"},
        {"name": "ci-site2", "slug": "ci-site2", "status": "active"},
    )
    vlan_dup = find_one("/ipam/vlans/", vid=1234, site_id=site2["id"])
    if not vlan_dup:
        vlan_dup = post(
            "/ipam/vlans/",
            {"vid": 1234, "name": "ci-vlan2", "status": "active", "site": site2["id"]},
        )
    print(
        f"duplicate vlan vid 1234 at ci-site (id {vlan['id']}) "
        f"and ci-site2 (id {vlan_dup['id']})"
    )

    # --- scoped VLAN group + a VLAN in it ------------------------------------
    # A VLAN group is itself polymorphically scoped (unlike a VLAN). Scope this
    # group to ci-region; the VLAN below belongs to it (and to no site of its
    # own), so `nbox vlan 1300` resolves uniquely and surfaces the GROUP's scope
    # on the additive group_scope / group_scope_type fields.
    vlan_group = find_one("/ipam/vlan-groups/", slug="ci-vgroup")
    if not vlan_group:
        vlan_group = post(
            "/ipam/vlan-groups/",
            {
                "name": "ci-vgroup",
                "slug": "ci-vgroup",
                "scope_type": "dcim.region",
                "scope_id": region["id"],
            },
        )
    print(
        f"vlan-group: {vlan_group['slug']} (id {vlan_group['id']}) "
        f"scoped to ci-region"
    )

    grouped_vlan = find_one("/ipam/vlans/", vid=1300, group_id=vlan_group["id"])
    if not grouped_vlan:
        grouped_vlan = post(
            "/ipam/vlans/",
            {
                "vid": 1300,
                "name": "ci-grouped-vlan",
                "status": "active",
                "group": vlan_group["id"],
            },
        )
    print(
        f"grouped vlan: vid {grouped_vlan['vid']} ({grouped_vlan['name']}) "
        f"in ci-vgroup"
    )

    # --- manufacturer -> device-type -> device-role -> device ----------------
    mfg = ensure(
        "/dcim/manufacturers/",
        {"slug": "ci-mfg"},
        {"name": "ci-mfg", "slug": "ci-mfg"},
    )
    device_type = ensure(
        "/dcim/device-types/",
        {"slug": "ci-model"},
        {"manufacturer": mfg["id"], "model": "ci-model", "slug": "ci-model"},
    )
    role = ensure(
        "/dcim/device-roles/",
        {"slug": "ci-role"},
        {"name": "ci-role", "slug": "ci-role", "color": "0000ff"},
    )
    device = ensure(
        "/dcim/devices/",
        {"name": "ci-dev1"},
        {
            "name": "ci-dev1",
            "device_type": device_type["id"],
            "role": role["id"],
            "site": site["id"],
            "status": "active",
        },
    )
    print(f"device: {device['name']} (id {device['id']}) at ci-site")

    # --- interface on the device (a name WITH a slash) -----------------------
    iface_name = "xe-0/0/1"
    iface = find_one("/dcim/interfaces/", device_id=device["id"], name=iface_name)
    if not iface:
        iface = post(
            "/dcim/interfaces/",
            {"device": device["id"], "name": iface_name, "type": "10gbase-x-sfpp"},
        )
    print(f"interface: {iface['name']} on ci-dev1 (id {iface['id']})")

    # --- IP on the interface, set as the device's primary --------------------
    ip = find_one("/ipam/ip-addresses/", address="10.10.0.5/24")
    if not ip:
        ip = post(
            "/ipam/ip-addresses/",
            {
                "address": "10.10.0.5/24",
                "status": "active",
                "assigned_object_type": "dcim.interface",
                "assigned_object_id": iface["id"],
            },
        )
    print(f"ip: {ip['address']} -> ci-dev1 {iface_name} (id {ip['id']})")

    # Promote to the device's primary IPv4 (idempotent).
    if device.get("primary_ip4") is None or device["primary_ip4"] is None:
        patch(f"/dcim/devices/{device['id']}/", {"primary_ip4": ip["id"]})
        print("set 10.10.0.5/24 as ci-dev1's primary IPv4")

    # --- journal entry on the device -----------------------------------------
    # One entry so `nbox journal device ci-dev1` and `nbox device ci-dev1
    # --journal` both surface it. Keyed on (content type, id) + comments for
    # idempotency (re-seeding won't pile up duplicate entries).
    JOURNAL_COMMENT = "ci seed journal entry"
    journal = find_one(
        "/extras/journal-entries/",
        assigned_object_type="dcim.device",
        assigned_object_id=device["id"],
    )
    if not journal or journal.get("comments") != JOURNAL_COMMENT:
        journal = post(
            "/extras/journal-entries/",
            {
                "assigned_object_type": "dcim.device",
                "assigned_object_id": device["id"],
                "kind": "info",
                "comments": JOURNAL_COMMENT,
            },
        )
    print(f"journal entry on ci-dev1 (id {journal['id']})")

    # --- child prefixes to force multi-page pagination -----------------------
    # 25 prefixes 10.10.<n>.0/24 nested under the scoped /16. The prefix detail
    # lists child prefixes via `list_all` (offset pagination); with page_size=5
    # this walks five windows, exercising the offset-windows fix end to end.
    created = 0
    for n in range(1, FILLER_PREFIX_COUNT + 1):
        cidr = f"10.10.{n}.0/24"
        if not find_one("/ipam/prefixes/", prefix=cidr, vrf_id="null"):
            post(
                "/ipam/prefixes/",
                {"prefix": cidr, "status": "active", "tags": [{"slug": "ci-tag"}]},
            )
            created += 1
    print(
        f"child prefixes 10.10.1.0/24..10.10.{FILLER_PREFIX_COUNT}.0/24 "
        f"({created} created this run, {FILLER_PREFIX_COUNT} total under the /16)"
    )

    # --- exhausted /32 prefix (next-ip graceful-empty edge) ------------------
    # A /32 host prefix whose single address is already assigned: its
    # `…/available-ips/` returns []. `nbox next-ip 10.50.0.1/32` must then exit 0
    # with an empty `available` list (graceful), not error.
    full_prefix = ensure(
        "/ipam/prefixes/",
        {"prefix": "10.50.0.1/32"},
        {"prefix": "10.50.0.1/32", "status": "active"},
    )
    if not find_one("/ipam/ip-addresses/", address="10.50.0.1/32"):
        post(
            "/ipam/ip-addresses/",
            {"address": "10.50.0.1/32", "status": "active"},
        )
    print(f"exhausted prefix: {full_prefix['prefix']} (sole address assigned)")

    # --- empty /28 prefix (next-ip --count / next-prefix --length edges) -----
    # A fully free /28 with NO children: `next-ip --count 3` yields three
    # addresses inside it, and `next-prefix --length 30` carves /30 blocks from
    # it. Distinct from the busy /16 so the counts/lengths are deterministic.
    free_prefix = ensure(
        "/ipam/prefixes/",
        {"prefix": "10.60.0.0/28"},
        {"prefix": "10.60.0.0/28", "status": "active"},
    )
    print(f"free prefix: {free_prefix['prefix']} (no children, all free)")

    print("Seed complete.")


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:  # pragma: no cover - surfaced to CI logs
        print(f"Seed failed: {e}", file=sys.stderr)
        sys.exit(1)

#!/usr/bin/env python3
"""Seed a small, deterministic fixture into a live NetBox 4.2.x for nbox's
integration tests.

Uses only the Python standard library (urllib) so CI needs no extra packages.
Idempotent: each object is looked up by a natural key first and only created if
absent, so re-running the seed against an already-seeded instance is a no-op.

Objects created, in dependency order:
  - a tag                         (slug: ci-tag)
  - a site                        (slug: ci-site)
  - a VRF                         (name: ci-vrf)
  - a site-scoped prefix          (10.10.0.0/16, scope_type=dcim.site)
  - a duplicate prefix            (10.0.0.0/24 in ci-vrf AND in the global table)
  - a VLAN                        (vid 1234 "ci-vlan" at ci-site)
  - manufacturer/type/role/device (ci-mfg / ci-model / ci-role / ci-dev1)
  - an interface                  (xe-0/0/1 on ci-dev1 — name has a slash)
  - an IP on that interface       (10.10.0.5/24, set as the device's primary)
  - 25 child prefixes             (10.10.<n>.0/24, nested under the scoped /16)
                                   to force >1 page of pagination on the prefix
                                   detail's child-prefix list (uses list_all)

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


def main():
    print(f"Seeding {NBOX_URL} ...")

    # --- tag -----------------------------------------------------------------
    tag = ensure(
        "/extras/tags/",
        {"slug": "ci-tag"},
        {"name": "ci-tag", "slug": "ci-tag", "color": "00ff00"},
    )
    print(f"tag: {tag['slug']} (id {tag['id']})")

    # --- site ----------------------------------------------------------------
    site = ensure(
        "/dcim/sites/",
        {"slug": "ci-site"},
        {"name": "ci-site", "slug": "ci-site", "status": "active"},
    )
    print(f"site: {site['slug']} (id {site['id']})")

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

    print("Seed complete.")


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:  # pragma: no cover - surfaced to CI logs
        print(f"Seed failed: {e}", file=sys.stderr)
        sys.exit(1)

#!/usr/bin/env python3
"""Enrich the local demo NetBox with richer, show-off data.

This is *demo* scaffolding, separate from `seed.py` (the CI integration fixture):
it's additive and idempotent, so it can run repeatedly against the running
`tests/integration` NetBox without disturbing the CI objects. It populates
ci-rack-1 with varied-height devices (so the rack-elevation tab looks real) and
adds a spread of objects across kinds (tenant, circuit, cluster/VM, aggregate,
ASN, IPs, journal entries) so search, the dashboard, and detail views all have
something to show.

Usage:
    NETBOX_TOKEN="$(tests/integration/resolve-token.sh)" \
        python3 tests/integration/seed-demo.py
"""

import json
import os
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("NETBOX_URL", "http://localhost:8000/api").rstrip("/")
TOKEN = os.environ.get("NETBOX_TOKEN", "0123456789abcdef0123456789abcdef0fedcba9")


def _request(method, path, body=None):
    url = f"{BASE}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Token {TOKEN}")
    req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req) as resp:
            return json.loads(resp.read() or "null")
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")
        raise SystemExit(f"{method} {path} -> {e.code}: {detail}") from e


def get(path):
    return _request("GET", path)


def post(path, body):
    return _request("POST", path, body)


def find_one(endpoint, **params):
    q = "&".join(f"{k}={urllib.parse.quote(str(v))}" for k, v in params.items())
    res = get(f"/{endpoint}/?{q}")
    results = res.get("results", [])
    return results[0] if results else None


def ensure(endpoint, lookup, body):
    """Find an object by `lookup`, else create it from `body`. Returns it."""
    existing = find_one(endpoint, **lookup)
    if existing:
        return existing
    created = post(f"/{endpoint}/", body)
    print(f"  + {endpoint}: {body.get('name') or body.get('model') or body.get('cid') or body.get('prefix') or body.get('address') or body.get('asn')}")
    return created


def main():
    print(f"Seeding demo data into {BASE} …")

    site = find_one("dcim/sites", slug="ci-site")
    site2 = find_one("dcim/sites", slug="ci-site2")
    rack = find_one("dcim/racks", name="ci-rack-1")
    if not (site and rack):
        raise SystemExit("expected ci-site + ci-rack-1 from seed.py; run seed.py first")
    site_id, rack_id = site["id"], rack["id"]

    # --- Catalog: one manufacturer, roles (colored), varied-height types -------
    mfg = ensure("dcim/manufacturers", {"slug": "demo-systems"},
                 {"name": "Demo Systems", "slug": "demo-systems"})["id"]

    roles = {}
    for name, slug, color in [
        ("Router", "router", "f44336"),
        ("Switch", "switch", "2196f3"),
        ("Server", "server", "4caf50"),
        ("Storage", "storage", "009688"),
        ("Firewall", "firewall", "ff9800"),
    ]:
        roles[slug] = ensure("dcim/device-roles", {"slug": slug},
                             {"name": name, "slug": slug, "color": color})["id"]

    types = {}
    for model, slug, u in [
        ("DX-4000 Router", "dx-4000", 4),
        ("DX-2000 Switch", "dx-2000", 2),
        ("DX-1000 Server", "dx-1000", 1),
        ("DX-Storage", "dx-storage", 2),
    ]:
        types[slug] = ensure("dcim/device-types", {"slug": slug},
                            {"manufacturer": mfg, "model": model, "slug": slug,
                             "u_height": u})["id"]

    # --- Rack devices: a realistic, varied elevation for ci-rack-1 -------------
    # (name, type, role, position, status)  — ci-dev1 already sits at U10.
    racked = [
        ("core-rtr-01", "dx-4000", "router", 39, "active"),
        ("agg-sw-01", "dx-2000", "switch", 36, "active"),
        ("agg-sw-02", "dx-2000", "switch", 34, "active"),
        ("app-01", "dx-1000", "server", 30, "active"),
        ("app-02", "dx-1000", "server", 29, "active"),
        ("app-03", "dx-1000", "server", 28, "staged"),
        ("app-04", "dx-1000", "server", 27, "active"),
        ("db-01", "dx-1000", "server", 25, "active"),
        ("storage-01", "dx-storage", "storage", 4, "active"),
    ]
    for name, dt, role, pos, status in racked:
        ensure("dcim/devices", {"name": name}, {
            "name": name, "device_type": types[dt], "role": roles[role],
            "site": site_id, "rack": rack_id, "position": pos, "face": "front",
            "status": status,
        })

    # --- A few devices in the second site (search variety, not racked) ---------
    if site2:
        for name, dt, role, status in [
            ("edge-fw-01", "dx-2000", "firewall", "active"),
            ("edge-sw-01", "dx-2000", "switch", "active"),
            ("edge-sw-02", "dx-2000", "switch", "offline"),
        ]:
            ensure("dcim/devices", {"name": name}, {
                "name": name, "device_type": types[dt], "role": roles[role],
                "site": site2["id"], "status": status,
            })

    # --- Tenancy -------------------------------------------------------------
    tenant = ensure("tenancy/tenants", {"slug": "acme-corp"},
                    {"name": "Acme Corp", "slug": "acme-corp"})["id"]
    ensure("tenancy/contacts", {"name": "Dana Ops"},
           {"name": "Dana Ops", "email": "dana@acme.example"})

    # --- Circuits ------------------------------------------------------------
    ctype = ensure("circuits/circuit-types", {"slug": "internet"},
                   {"name": "Internet", "slug": "internet"})["id"]
    provider = ensure("circuits/providers", {"slug": "lumen"},
                      {"name": "Lumen", "slug": "lumen"})["id"]
    for cid in ("wan-iad-lax-01", "wan-iad-ord-01"):
        ensure("circuits/circuits", {"cid": cid},
               {"cid": cid, "provider": provider, "type": ctype, "status": "active"})

    # --- Virtualization ------------------------------------------------------
    cltype = ensure("virtualization/cluster-types", {"slug": "vmware"},
                    {"name": "VMware", "slug": "vmware"})["id"]
    cluster = ensure("virtualization/clusters", {"name": "prod-vmware"},
                     {"name": "prod-vmware", "type": cltype})["id"]
    for vm, status in [("web-vm-01", "active"), ("web-vm-02", "active"),
                       ("ci-runner-vm", "offline")]:
        ensure("virtualization/virtual-machines", {"name": vm},
               {"name": vm, "cluster": cluster, "status": status})

    # --- IPAM extras: RIR + aggregate + ASN + a few addresses ----------------
    rir = ensure("ipam/rirs", {"slug": "arin"}, {"name": "ARIN", "slug": "arin"})["id"]
    ensure("ipam/aggregates", {"prefix": "10.0.0.0/8"},
           {"prefix": "10.0.0.0/8", "rir": rir})
    ensure("ipam/asns", {"asn": 65001}, {"asn": 65001, "rir": rir, "tenant": tenant})
    for addr, dns in [
        ("10.10.1.10/24", "app-01.demo"),
        ("10.10.1.11/24", "app-02.demo"),
        ("10.10.2.10/24", "db-01.demo"),
    ]:
        ensure("ipam/ip-addresses", {"address": addr},
               {"address": addr, "status": "active", "dns_name": dns})

    # --- A VRF as a routing context (shows off the VRF view) ------------------
    # Two route targets, a customer VRF that imports both / exports one, and a
    # small prefix tree + addresses scoped to it so the detail has a real tree.
    rts = {}
    for rt in ("65000:100", "65000:200"):
        rts[rt] = ensure("ipam/route-targets", {"name": rt}, {"name": rt})["id"]
    vrf = ensure("ipam/vrfs", {"name": "customer-prod"}, {
        "name": "customer-prod",
        "rd": "65000:100",
        "tenant": tenant,
        "enforce_unique": True,
        "description": "Production customer routing instance",
        "import_targets": [rts["65000:100"], rts["65000:200"]],
        "export_targets": [rts["65000:100"]],
    })["id"]
    for prefix, status, descr in [
        ("10.20.0.0/16", "container", "customer supernet"),
        ("10.20.1.0/24", "active", "web tier"),
        ("10.20.2.0/24", "active", "app tier"),
        ("10.20.10.0/24", "reserved", "db tier"),
        ("10.20.20.0/24", "active", "mgmt"),
    ]:
        ensure("ipam/prefixes", {"prefix": prefix, "vrf_id": vrf},
               {"prefix": prefix, "vrf": vrf, "status": status, "description": descr})
    for addr, dns in [
        ("10.20.1.10/24", "web-01.customer"),
        ("10.20.1.11/24", "web-02.customer"),
        ("10.20.2.10/24", "app-01.customer"),
        ("10.20.10.10/24", "db-01.customer"),
    ]:
        ensure("ipam/ip-addresses", {"address": addr, "vrf_id": vrf},
               {"address": addr, "vrf": vrf, "status": "active", "dns_name": dns})

    # --- Journal entries (dashboard "recent" + device journal tab) ------------
    core = find_one("dcim/devices", name="core-rtr-01")
    if core and not find_one("extras/journal-entries", assigned_object_id=core["id"]):
        post("/extras/journal-entries/", {
            "assigned_object_type": "dcim.device",
            "assigned_object_id": core["id"],
            "kind": "info",
            "comments": "Demo: installed and cabled into agg-sw-01/02.",
        })
        print("  + journal entry on core-rtr-01")

    print("Done.")


if __name__ == "__main__":
    main()

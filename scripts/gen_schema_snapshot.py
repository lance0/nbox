#!/usr/bin/env python3
"""Regenerate the pinned NetBox OpenAPI snapshot the schema canary uses.

The canary (``src/netbox/search.rs``, ``mod schema_canary``) validates that the
filter params nbox's search fan-out sends are accepted by each endpoint, against
a pinned compact snapshot committed at ``tests/schema/netbox-<version>.json``.
This script rebuilds that snapshot from a full OpenAPI schema so it can be
refreshed against a new NetBox release — at which point the canary immediately
flags any endpoint/filter nbox uses that the new release dropped.

The snapshot is deliberately compact: per search endpoint, only the *bare* GET
filter params (``status``, ``tenant``, ``owner``, …) — not the ``__ic``/
``__n``/``__regex`` lookup variants nbox never sends. That keeps the file
reviewable (~15 KB) while covering everything ``search_supported()`` could
declare.

Usage::

    # From a saved schema file (e.g. curled from /api/schema/):
    scripts/gen_schema_snapshot.py /tmp/nb_schema.json -o tests/schema/netbox-4.6.2.json
    # From a live NetBox:
    scripts/gen_schema_snapshot.py https://netbox.example.com/api/schema/ \\
        --token NBT_xxx -o tests/schema/netbox-4.7.0.json

Then update the ``SNAPSHOT`` path in ``src/netbox/search.rs`` (``schema_canary``)
to point at the new file, and run the canary — drift shows up as a test failure
with the exact endpoint + filter that's no longer accepted.

Exit 1 if a search endpoint is missing from the schema (the canary would also
catch this, but fail loudly here so the snapshot isn't silently partial).
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.request
from pathlib import Path

# The 20 endpoints nbox's search fan-out hits (matches Endpoint::path() in
# src/netbox/endpoints.rs). Keep in sync with the `search_supported` table.
SEARCHED_ENDPOINTS = [
    "/api/dcim/devices/",
    "/api/dcim/sites/",
    "/api/ipam/ip-addresses/",
    "/api/ipam/prefixes/",
    "/api/ipam/vlans/",
    "/api/circuits/circuits/",
    "/api/circuits/virtual-circuits/",
    "/api/ipam/aggregates/",
    "/api/ipam/asns/",
    "/api/ipam/ip-ranges/",
    "/api/tenancy/tenants/",
    "/api/tenancy/contacts/",
    "/api/circuits/providers/",
    "/api/virtualization/virtual-machines/",
    "/api/virtualization/virtual-machine-types/",
    "/api/virtualization/clusters/",
    "/api/dcim/racks/",
    "/api/dcim/rack-groups/",
    "/api/ipam/vrfs/",
    "/api/ipam/route-targets/",
]


def load_schema(source: str, token: str | None) -> dict:
    if source.startswith(("http://", "https://")):
        req = urllib.request.Request(source)
        if token:
            req.add_header("Authorization", f"Token {token}")
        with urllib.request.urlopen(req, timeout=60) as r:  # noqa: S310
            return json.load(r)
    return json.loads(Path(source).read_text())


def bare_filter_params(get_op: dict) -> list[str]:
    """Bare GET filter param names (no ``__lookup`` variants)."""
    return sorted(
        {
            p["name"]
            for p in get_op.get("parameters", [])
            if "__" not in p["name"]
        }
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "source",
        help="Path to a saved OpenAPI schema JSON, or a live /api/schema/ URL.",
    )
    ap.add_argument("--token", help="API token (only for a live URL).")
    ap.add_argument(
        "-o",
        "--out",
        default="tests/schema/netbox-snapshot.json",
        help="Output snapshot path.",
    )
    args = ap.parse_args()

    schema = load_schema(args.source, args.token)
    paths = schema.get("paths", {})
    version = schema.get("info", {}).get("version", "unknown")

    snap = {
        "_meta": {
            "netbox_version": version,
            "schema_source": args.source,
            "note": (
                "Bare GET filter params (no __lookup variants) per search "
                "endpoint. The schema canary (src/netbox/search.rs) validates "
                "nbox's declared search_supported() filters are a subset of "
                "each endpoint's list here."
            ),
        }
    }
    missing = []
    for ep in SEARCHED_ENDPOINTS:
        get_op = paths.get(ep, {}).get("get")
        if get_op is None:
            missing.append(ep)
            snap[ep] = []
            continue
        snap[ep] = bare_filter_params(get_op)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(snap, indent=2, sort_keys=True) + "\n")
    print(f"wrote {out_path} ({out_path.stat().st_size} bytes; NetBox {version})")

    if missing:
        print(
            f"\nERROR: {len(missing)} search endpoint(s) missing from the "
            f"schema — snapshot is partial:\n  "
            + "\n  ".join(missing),
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

---
name: nbox-device-context
description: Pull the full physical context of a device or interface from NetBox with nbox — a device's interfaces/IPs/cables/VLANs/services, an interface's cable-path A↔Z trace, a MAC reverse-resolved to its carrying interface/device, plus rack and site context. Use when the user asks how a device is connected, what's on an interface, where a MAC lives, or what's in a rack/site.
---

# nbox device context

These read commands answer the physical/connectivity questions: what a device
is, how it's wired, what an interface terminates into, and where it sits. All
are read-only. For the flags of any one, run `nbox <cmd> --help` — this skill is
flag-free by design, describing what each command answers.

## What each command answers

- **`nbox device <name|slug|id>`** — a device plus its interfaces, IPs, cables,
  VLANs, and services. The starting point for "what is `edge01` and what's on
  it?"
- **`nbox interface <device> <interface>`** — one interface: type, MTU, MAC,
  mode, VLANs, addresses, the attached cable, and the **cable-path A↔Z trace**
  (a diagram naming the device at each hop). The reference is the `<device>`
  `<interface>` pair; interface names may contain slashes (e.g. `xe-0/0/1`).
- **`nbox mac <addr>`** — reverse-resolve a MAC to the interface(s) and
  device(s) that carry it. Any common form is normalized
  (`aa:bb:cc:dd:ee:ff`, `AABB.CCDD.EEFF`, `aa-bb-…`, `aabbccddeeff`).
- **`nbox rack <name|id>`** / **`nbox site <name|slug>`** — the rack or site a
  device sits in, for surrounding context.

```bash
nbox --no-tui device edge01 --json --envelope
nbox --no-tui interface edge01 xe-0/0/1 --json    # config + cable-path trace
nbox --no-tui mac aa:bb:cc:dd:ee:ff --json        # → carrying interface/device
nbox --no-tui rack R12 --json
nbox --no-tui site DC1 --json
```

## The cable-path trace (A↔Z)

`nbox interface` is the connectivity tool: its cable-path section traces the
physical path end to end, naming the device at each hop. Reach for it to answer
"what is `edge01:xe-0/0/1` actually wired to?" — the trace follows through
patch panels and intermediate cables to the far-end device/interface.

## The `<device>/<name>` ref form

Some commands take an interface as a single `<device>/<name>` reference rather
than two arguments — e.g. `nbox journal interface edge01/xe-0/0/1`,
`nbox history interface edge01/xe-0/0/1`, and `nbox open
interface/edge01/xe-0/0/1`. The interface name keeps its slashes; the resolver
splits on the device boundary, so `xe-0/0/1` stays intact.

## Resolving a MAC, then drilling in

`nbox mac` is a reverse lookup: a non-MAC input is a usage error, and a MAC
carried by several interfaces is ambiguous (exit 5) and lists the candidates. A
resolved interface chains straight into `nbox interface <device> <name>` for the
full config and cable path.

## Reference

- [AGENTS.md](https://github.com/lance0/nbox/blob/master/AGENTS.md) — the
  complete command + flag reference, ref forms, and exit codes
- [Search](../search/SKILL.md) — to find a device/interface reference when the
  exact name isn't known
- [IPAM read](../ipam-read/SKILL.md) — to go from an interface's IP to its
  parent prefix / VLAN / scope

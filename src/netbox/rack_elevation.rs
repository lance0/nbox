//! Rack elevation — a framed, U-by-U view of a rack's front face for the TUI
//! detail's `e` (elevation) tab.
//!
//! NetBox computes the layout for us: `/api/dcim/racks/{id}/elevation/?face=front`
//! returns one entry per half-U slot (so `2 × u_height` rows) with the device
//! occupying it. We keep only the whole-U slots (occupancy is identical on the
//! integer unit), preserve the endpoint's order (already top-of-rack first, so a
//! descending-unit rack renders right-side-up for free), and render a bordered
//! rack. Devices assigned to the rack without a position don't appear in the
//! elevation, so they're listed underneath as "not racked".

use std::fmt::Write as _;

use anyhow::Result;
use serde::Deserialize;

use crate::netbox::client::NetBoxClient;
use crate::netbox::pagination::Page;

/// One half-U slot as returned by the elevation endpoint.
#[derive(Debug, Deserialize)]
struct ElevationUnit {
    /// Unit position; whole numbers are real U positions, `.5` are half-U slots.
    id: f64,
    #[serde(default)]
    device: Option<ElevationDevice>,
}

#[derive(Debug, Deserialize)]
struct ElevationDevice {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    display: Option<String>,
}

/// The device assignment for a rack (to surface position-less devices).
#[derive(Debug, Deserialize)]
struct RackDevice {
    name: String,
    #[serde(default)]
    position: Option<f64>,
}

/// One whole-U slot in display order (top of rack first).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Slot {
    u: u32,
    /// `(device id, name)` occupying this U, or `None` when empty.
    device: Option<(u64, String)>,
}

/// A rack's front elevation: whole-U slots in display order plus any rack-assigned
/// devices that have no mounted position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RackElevation {
    slots: Vec<Slot>,
    unpositioned: Vec<String>,
}

/// Fetch and assemble a rack's front elevation. `u_height` is the rack height
/// (used only to size the single-page request); pass the value from the rack.
pub async fn load_rack_elevation(
    client: &NetBoxClient,
    rack_id: u64,
    u_height: u32,
) -> Result<RackElevation> {
    // The endpoint paginates at half-U granularity (`2 × u_height` rows); ask for
    // them all in one page.
    let limit = (usize::try_from(u_height).unwrap_or(0) * 2 + 4).max(50);
    let page: Page<ElevationUnit> = client
        .get(
            &format!("/api/dcim/racks/{rack_id}/elevation/"),
            &[("face", "front".to_string()), ("limit", limit.to_string())],
        )
        .await?;

    // Whole-U slots only, in the endpoint's order (top of rack first).
    let slots: Vec<Slot> = page
        .results
        .into_iter()
        .filter(|u| u.id.fract() == 0.0)
        .map(|u| Slot {
            u: u.id as u32,
            device: u.device.map(|d| {
                let name = d
                    .name
                    .or(d.display)
                    .unwrap_or_else(|| format!("device {}", d.id));
                (d.id, name)
            }),
        })
        .collect();

    // Devices assigned to this rack but not mounted at a position never show up in
    // the elevation above; surface them so they aren't silently hidden.
    let devices: Page<RackDevice> = client
        .get(
            "/api/dcim/devices/",
            &[
                ("rack_id", rack_id.to_string()),
                ("limit", "1000".to_string()),
            ],
        )
        .await?;
    let unpositioned: Vec<String> = devices
        .results
        .into_iter()
        .filter(|d| d.position.is_none())
        .map(|d| d.name)
        .collect();

    Ok(RackElevation {
        slots,
        unpositioned,
    })
}

/// The widest device label sets the inner column; clamped so a long name can't
/// blow out the frame and a near-empty rack still draws a sensible width.
const MIN_CONTENT_W: usize = 16;
const MAX_CONTENT_W: usize = 40;
/// The filled block + a space that prefixes a device cell (`"██ "`).
const BLOCK: &str = "██";

impl RackElevation {
    /// Render the framed front elevation as plain text for the detail tab body.
    #[must_use]
    pub fn render(&self) -> String {
        if self.slots.is_empty() {
            // No height / empty elevation: still surface any unracked devices.
            return self.unpositioned_footer().trim_start().to_string();
        }

        // U-label column width: "U" + the widest unit number, zero-padded so the
        // pipes line up (U09 / U42).
        let digits = self
            .slots
            .iter()
            .map(|s| s.u)
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(2);
        let name_w = self
            .slots
            .iter()
            .filter_map(|s| s.device.as_ref())
            .map(|(_, n)| n.chars().count())
            .max()
            .unwrap_or(0);
        let content_w = (name_w + BLOCK.chars().count() + 1).clamp(MIN_CONTENT_W, MAX_CONTENT_W);

        let lead = "    ";
        let label_pad = " ".repeat(1 + digits);
        let bar = "─".repeat(content_w + 2);
        let mut out = String::new();
        let _ = writeln!(out, "{lead}{label_pad} ┌{bar}┐");

        let mut prev_id: Option<u64> = None;
        for slot in &self.slots {
            let cell = match &slot.device {
                None => String::new(),
                Some((id, name)) => {
                    let top = prev_id != Some(*id);
                    if top {
                        let budget = content_w.saturating_sub(BLOCK.chars().count() + 1);
                        format!("{BLOCK} {}", truncate(name, budget))
                    } else {
                        BLOCK.to_string()
                    }
                }
            };
            prev_id = slot.device.as_ref().map(|(id, _)| *id);
            let _ = writeln!(
                out,
                "{lead}U{u:0digits$} │ {cell:<content_w$} │",
                u = slot.u
            );
        }
        let _ = write!(out, "{lead}{label_pad} └{bar}┘");
        out.push_str(&self.unpositioned_footer());
        out
    }

    /// A trailing "not racked · …" line listing position-less devices (empty when
    /// there are none). Leads with a blank line so it sits clear of the frame.
    fn unpositioned_footer(&self) -> String {
        if self.unpositioned.is_empty() {
            String::new()
        } else {
            format!("\n\n  not racked · {}", self.unpositioned.join(", "))
        }
    }
}

/// Truncate `s` to at most `max` characters, marking elision with `…`.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let kept: String = s.chars().take(max - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(u: u32, dev: Option<(u64, &str)>) -> Slot {
        Slot {
            u,
            device: dev.map(|(id, n)| (id, n.to_string())),
        }
    }

    #[test]
    fn renders_a_framed_elevation_with_a_spanning_device() {
        // A 4U rack, top→bottom, with a 2U device at U3-U2 (name on the top row).
        let e = RackElevation {
            slots: vec![
                slot(4, None),
                slot(3, Some((1, "core-rtr-01"))),
                slot(2, Some((1, "core-rtr-01"))),
                slot(1, None),
            ],
            unpositioned: vec!["ci-dev1".to_string()],
        };
        let out = e.render();
        let lines: Vec<&str> = out.lines().collect();
        // Border + 4 U rows + border + blank + footer = 8 lines.
        assert_eq!(lines.len(), 8, "got:\n{out}");
        assert!(lines[0].contains('┌') && lines[0].contains('┐'));
        // U4 empty; U3 carries the name; U2 is the continuation block (no name).
        assert!(lines[1].contains("U04 │") && !lines[1].contains('█'));
        assert!(lines[2].contains("U03 │") && lines[2].contains("██ core-rtr-01"));
        assert!(lines[3].contains("U02 │") && lines[3].contains('█'));
        assert!(
            !lines[3].contains("core-rtr-01"),
            "continuation has no name"
        );
        assert!(lines[5].contains('└') && lines[5].contains('┘'));
        assert_eq!(lines.last().unwrap().trim(), "not racked · ci-dev1");
        // Every framed row is the same visual width (pipes line up).
        let widths: Vec<usize> = lines[0..=5].iter().map(|l| l.chars().count()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "frame rows differ in width: {widths:?}"
        );
    }

    #[test]
    fn empty_rack_renders_an_empty_frame() {
        let e = RackElevation {
            slots: vec![slot(2, None), slot(1, None)],
            unpositioned: vec![],
        };
        let out = e.render();
        assert!(out.contains("U02 │") && out.contains("U01 │"));
        assert!(!out.contains('█'), "no devices, no blocks");
        assert!(!out.contains("not racked"));
    }

    #[test]
    fn long_device_name_is_truncated_within_the_frame() {
        let e = RackElevation {
            slots: vec![slot(1, Some((9, &"x".repeat(80))))],
            unpositioned: vec![],
        };
        let out = e.render();
        assert!(out.contains('…'), "an over-long name is elided");
        let row = out.lines().find(|l| l.contains("U01")).unwrap();
        // The framed row stays within the clamped content width (+ chrome).
        assert!(
            row.chars().count() <= 4 + 3 + 1 + MAX_CONTENT_W + 4,
            "row: {row:?}"
        );
    }

    #[test]
    fn no_slots_just_lists_unracked_devices() {
        let e = RackElevation {
            slots: vec![],
            unpositioned: vec!["a".to_string(), "b".to_string()],
        };
        assert_eq!(e.render(), "not racked · a, b");
    }
}

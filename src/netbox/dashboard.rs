//! Dashboard data: a small read-only fan-out for the TUI overview screen —
//! device status counts, the most-utilized prefixes, and recent journal activity.
//! Reuses the existing `list`/`get` primitives; strictly read-only.

use anyhow::Result;

use crate::netbox::client::NetBoxClient;
use crate::netbox::endpoints::Endpoint;
use crate::netbox::models::extras::JournalEntry;
use crate::netbox::models::ipam::Prefix;
use crate::netbox::pagination::Page;

/// Device statuses the dashboard facets, in display order.
pub const DASHBOARD_STATUSES: &[&str] = &[
    "active",
    "planned",
    "staged",
    "offline",
    "failed",
    "decommissioning",
];

/// How many prefixes to scan when ranking by utilization. NetBox has no
/// `ordering=utilization` (it's a computed property, not a column), so we rank a
/// capped page client-side — best-effort, not an exhaustive ranking.
const PREFIX_SCAN: usize = 500;
/// How many top prefixes the prefix card shows.
const TOP_N: usize = 6;
/// How many recent entries the activity card shows.
const JOURNAL_N: usize = 8;

/// The data behind the overview dashboard.
#[derive(Debug, Clone, Default)]
pub struct DashboardData {
    /// Total device count.
    pub device_total: usize,
    /// `(status, count)` for each faceted status with a non-zero count.
    pub device_status_counts: Vec<(String, usize)>,
    /// `(cidr, utilization percent)` for the most-utilized prefixes, highest first.
    pub top_prefixes: Vec<(String, u8)>,
    /// Recent journal activity, newest first.
    pub recent: Vec<JournalLine>,
}

/// One recent-activity row.
#[derive(Debug, Clone)]
pub struct JournalLine {
    pub created: String,
    pub kind: String,
    pub summary: String,
}

/// Coerce NetBox's permissive `utilization` value (a number, a `"42%"`/`"42"`
/// string, or null) to a clamped 0–100 percent. Pure + testable.
pub fn utilization_pct(v: &serde_json::Value) -> Option<u8> {
    let pct = match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().trim_end_matches('%').trim().parse::<f64>().ok(),
        _ => None,
    }?;
    pct.is_finite().then(|| pct.round().clamp(0.0, 100.0) as u8)
}

async fn device_count(client: &NetBoxClient, status: Option<&str>) -> Result<usize> {
    let mut params = vec![("limit", "1".to_string())];
    if let Some(s) = status {
        params.push(("status", s.to_string()));
    }
    let page: Page<serde_json::Value> = client.get(Endpoint::Devices.path(), &params).await?;
    Ok(page.count)
}

async fn status_counts(client: &NetBoxClient) -> Result<Vec<(String, usize)>> {
    let futs = DASHBOARD_STATUSES.iter().map(|s| async move {
        device_count(client, Some(s))
            .await
            .map(|c| ((*s).to_string(), c))
    });
    let pairs = futures::future::try_join_all(futs).await?;
    Ok(pairs.into_iter().filter(|(_, c)| *c > 0).collect())
}

async fn top_prefixes(client: &NetBoxClient) -> Result<Vec<(String, u8)>> {
    let params = vec![("limit", PREFIX_SCAN.to_string())];
    let page: Page<Prefix> = client.get(Endpoint::Prefixes.path(), &params).await?;
    // Reuse the prefix-tree builder so utilization is populated even on NetBox 4.5,
    // which dropped the API `utilization` field: container prefixes get their
    // child-coverage computed from this same page (no extra calls). On older
    // NetBox the API-provided value still wins. See `prefix_tree::fill_child_coverage`.
    let mut utils: Vec<(String, u8)> = crate::netbox::prefix_tree::build_nodes(page.results)
        .into_iter()
        .filter_map(|n| n.utilization.map(|pct| (n.prefix, pct)))
        .collect();
    // Highest utilization first; ties broken by CIDR for a stable order.
    utils.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    utils.truncate(TOP_N);
    Ok(utils)
}

async fn recent_journal(client: &NetBoxClient) -> Result<Vec<JournalLine>> {
    let params = vec![
        ("ordering", "-created".to_string()),
        ("limit", JOURNAL_N.to_string()),
    ];
    let page: Page<JournalEntry> = client.get(Endpoint::JournalEntries.path(), &params).await?;
    Ok(page
        .results
        .into_iter()
        .map(|e| JournalLine {
            created: e.created.unwrap_or_default(),
            kind: e.kind.map(|k| k.label).unwrap_or_default(),
            summary: e
                .comments
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
        })
        .collect())
}

/// Load the dashboard: device status counts, top-utilized prefixes, and recent
/// journal activity, fanned out concurrently. Read-only.
pub async fn load_dashboard(client: &NetBoxClient) -> Result<DashboardData> {
    let (device_total, device_status_counts, top_prefixes, recent) = tokio::try_join!(
        device_count(client, None),
        status_counts(client),
        top_prefixes(client),
        recent_journal(client),
    )?;
    Ok(DashboardData {
        device_total,
        device_status_counts,
        top_prefixes,
        recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn utilization_pct_coerces_numbers_strings_and_clamps() {
        assert_eq!(utilization_pct(&json!(42)), Some(42));
        assert_eq!(utilization_pct(&json!(42.6)), Some(43));
        assert_eq!(utilization_pct(&json!("78")), Some(78));
        assert_eq!(utilization_pct(&json!("92%")), Some(92));
        assert_eq!(utilization_pct(&json!(150)), Some(100), "clamped high");
        assert_eq!(utilization_pct(&json!(-5)), Some(0), "clamped low");
        assert_eq!(utilization_pct(&json!(null)), None);
        assert_eq!(utilization_pct(&json!("n/a")), None);
    }
}

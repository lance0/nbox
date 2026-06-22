//! TUI application state and (pure) input handling.
//!
//! `handle_event`/`handle_key` mutate state and return the commands to run —
//! they perform no I/O, so they're unit-testable without a terminal. Network
//! work happens in spawned tasks (see `tui::app`), never in the render loop.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use crate::cache::{Cache, Freshness};
use crate::config::{ApiConfig, BackendPreference, ConfigToken, ProfileConfig};
use crate::domain::detail::{DetailRow, DetailView, ObjectLink};
use crate::netbox::auth::AuthScheme;
use crate::netbox::client::NetBoxClient;
use crate::netbox::dashboard::DashboardData;
use crate::netbox::prefix_tree::{self, PrefixNode, PrefixTreeData};
use crate::netbox::search::{ObjectKind, SearchFilters, SearchOutcome, SearchResult};
use crate::tui::cheese::{Spinner, TextInput};
use crate::tui::config_modal::{
    ConfigModal, ConnectionSeed, ModalOutcome, ProfileFormData, ProfilesMode, TestState,
};
use crate::tui::filter_modal::{FilterModal, FilterOutcome};
use crate::tui::palette::{self, PaletteCommand};
use crate::tui::theme::{Severity, Theme};

/// A floating overlay drawn on top of the live screen. Both are modal: while one
/// is open keys route to it and are consumed (the underlying screen is untouched).
/// Mirrors the old `help_open` flag, generalized so the Config editor can share
/// the overlay machinery (rendered last, keys consumed while open).
pub enum Modal {
    /// The `?`/`F1` keybindings overlay — any key (or `Esc`) closes it.
    Help,
    /// The `S` (or palette `config`) Config modal: an in-app profile editor and
    /// settings form. Has its own key handling. Boxed so the `Modal` enum stays
    /// small (the Config state is much larger than `Help`).
    Config(Box<ConfigModal>),
    /// The `f` filter modal: a discoverable editor for the active search filters.
    /// Boxed for the same size reason as `Config`.
    Filter(Box<FilterModal>),
    /// The `R` related-objects modal on a detail screen: a pick-list of the
    /// object's navigable relations (site/tenant/rack/parent prefix/…); Enter
    /// jumps to the selected one (drilling the NetBox graph without re-searching).
    Related(Box<RelatedModal>),
}

/// State for the `R` related-objects jump list: the current detail's navigable
/// links and the cursor over them. Pure; the selection is clamped on every move.
pub struct RelatedModal {
    pub links: Vec<ObjectLink>,
    pub selected: usize,
}

impl RelatedModal {
    fn new(links: Vec<ObjectLink>) -> Self {
        Self { links, selected: 0 }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.links.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.links.len() - 1;
        let cur = self.selected.min(max);
        self.selected = if delta >= 0 {
            cur.saturating_add(usize::try_from(delta).unwrap_or(0))
                .min(max)
        } else {
            cur.saturating_sub(delta.unsigned_abs())
        };
    }

    fn selected_link(&self) -> Option<&ObjectLink> {
        self.links.get(self.selected)
    }
}

/// Which screen is in the body area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Detail,
    /// The overview dashboard (`D`): status counts, top prefixes, recent activity.
    Dashboard,
    /// The hierarchical prefix tree (`T`): the IPAM prefix hierarchy, VRF-grouped,
    /// depth-indented, with collapse/expand.
    PrefixTree,
}

/// Input mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Command,
}

/// Which pane of the three-pane home screen has focus. Movement keys route to it:
/// Nav moves the section cursor, the list moves the selection, the preview scrolls
/// its body. `Tab`/`Shift+Tab` cycle left→right (Nav → List → Preview).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Nav,
    List,
    Preview,
}

/// A section in the home Navigation pane: a browse-by-kind entry, or `Recent`.
/// Selecting a kind lists all of it into the Results pane; `Recent` shows the
/// recently-opened items. Search stays on `/` (not a nav entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavSection {
    Devices,
    Prefixes,
    Ips,
    Vlans,
    Vrfs,
    RouteTargets,
    Sites,
    Racks,
    Recent,
}

/// The Nav sections in display order: the browse kinds, then `Recent` (rendered
/// under a divider). The selection cursor indexes this array.
pub const NAV_SECTIONS: [NavSection; 9] = [
    NavSection::Devices,
    NavSection::Prefixes,
    NavSection::Ips,
    NavSection::Vlans,
    NavSection::Vrfs,
    NavSection::RouteTargets,
    NavSection::Sites,
    NavSection::Racks,
    NavSection::Recent,
];

impl NavSection {
    /// The pane label.
    pub fn label(self) -> &'static str {
        match self {
            NavSection::Devices => "Devices",
            NavSection::Prefixes => "Prefixes",
            NavSection::Ips => "IPs",
            NavSection::Vlans => "VLANs",
            NavSection::Vrfs => "VRFs",
            // Abbreviated: the Nav rail is narrow, and "Route Targets" crowds the
            // count column. The list pane's title still spells it out in full.
            NavSection::RouteTargets => "RTs",
            NavSection::Sites => "Sites",
            NavSection::Racks => "Racks",
            NavSection::Recent => "Recent",
        }
    }

    /// The object kind this section browses, or `None` for `Recent`.
    pub fn object_kind(self) -> Option<ObjectKind> {
        Some(match self {
            NavSection::Devices => ObjectKind::Device,
            NavSection::Prefixes => ObjectKind::Prefix,
            NavSection::Ips => ObjectKind::IpAddress,
            NavSection::Vlans => ObjectKind::Vlan,
            NavSection::Vrfs => ObjectKind::Vrf,
            NavSection::RouteTargets => ObjectKind::RouteTarget,
            NavSection::Sites => ObjectKind::Site,
            NavSection::Racks => ObjectKind::Rack,
            NavSection::Recent => return None,
        })
    }
}

/// The [`NAV_SECTIONS`] index whose kind matches a slug (e.g. `device`, `vrf`,
/// `route-target`), or `None` for an unknown slug (or `Recent`). Used to restore
/// the Nav cursor from `[ui].last_browsed`.
fn nav_section_index_for_slug(slug: &str) -> Option<usize> {
    NAV_SECTIONS
        .iter()
        .position(|s| s.object_kind().is_some_and(|k| k.as_str() == slug))
}

/// Events delivered to the event loop.
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
    /// A fast, always-on debounce tick: flushes a settled selection into a
    /// preview load (see [`App::on_preview_tick`]). Distinct from `Tick`, which
    /// is the optional auto-refresh and may not be running.
    PreviewTick,
    /// A full-search result, tagged with the search channel's request id it was
    /// issued for so a stale (older-id) response can be dropped on arrival.
    SearchComplete {
        req: RequestId,
        result: anyhow::Result<SearchOutcome>,
    },
    /// A browse-by-kind result for the Nav pane, tagged with the browse channel's
    /// request id (stale ones dropped) and the kind it listed (for the Results
    /// title + count).
    BrowseComplete {
        req: RequestId,
        kind: ObjectKind,
        result: anyhow::Result<Vec<SearchResult>>,
    },
    /// Per-kind object counts for the Nav pane labels (background; last write wins).
    NavCounts(Vec<(ObjectKind, u32)>),
    /// A full-detail load (from Enter / a palette lookup), tagged with the detail
    /// channel's request id so a stale (older-id) response can be dropped.
    DetailLoaded {
        req: RequestId,
        result: anyhow::Result<DetailView>,
    },
    /// An overview-dashboard load result, tagged with its request id so a stale
    /// (superseded) load is dropped on arrival.
    DashboardLoaded {
        req: RequestId,
        result: anyhow::Result<DashboardData>,
    },
    /// A prefix-tree load result, tagged with its request id so a stale
    /// (superseded) load is dropped on arrival.
    PrefixTreeLoaded {
        req: RequestId,
        result: anyhow::Result<PrefixTreeData>,
    },
    /// A preview-pane detail load, tagged with the (kind, id) it was issued for
    /// so a stale response (the selection moved on) can be dropped.
    PreviewLoaded {
        kind: ObjectKind,
        id: u64,
        result: anyhow::Result<DetailView>,
    },
    /// Freshness for the detail that just loaded (additive to `DetailLoaded`, so
    /// the cache stays invisible to the load path). Tagged with the detail request
    /// id so a stale one is ignored; drives the footer's "cached Ns ago".
    DetailFreshness {
        req: RequestId,
        freshness: Freshness,
    },
    /// A profile switch finished re-probing the new instance: on success carries
    /// the rebuilt client and the new instance's `/api/status/` version; on
    /// failure carries the error to surface. Tagged with the monotonic switch
    /// `id` it was issued for so a switch superseded by a newer one — even one to
    /// the *same* profile name — is dropped on arrival (the latest-switch-wins
    /// guard, mirroring the search/detail request-id guard — see
    /// [`App::is_current_switch`]). `name` is carried for display only;
    /// correctness rides the `id`, never the name.
    ProfileSwitched {
        id: RequestId,
        name: String,
        result: anyhow::Result<(NetBoxClient, String)>,
    },
    /// A test-connect probe (from the Config modal's add/edit form) finished:
    /// success carries the probed NetBox version, failure the error. Tagged with
    /// the monotonic test `id` so a superseded test — the form changed and the
    /// user re-tested — is dropped on arrival (the request-id guard).
    ConnectTested {
        id: RequestId,
        result: anyhow::Result<String>,
    },
    /// The background update check finished: `Some(version)` when a newer release
    /// is available (drives the TUI banner), `None` when up to date or the check
    /// was skipped. Fired at most once per session.
    UpdateAvailable(Option<String>),
    Status(String),
}

/// The fields a test-connect / save needs to build a temporary client for a
/// candidate profile, independent of whether it's saved yet. The token is the
/// raw secret typed into the (masked) form; it is passed straight to the temp
/// client and never logged or persisted to TOML. Built from the form on demand.
///
/// `Debug` is hand-written to **redact the token** — it must never leak into a
/// log or a `{:?}` of the carrying [`AppCommand`].
#[derive(Clone)]
pub struct ConnectRequest {
    pub url: String,
    pub auth_scheme: AuthScheme,
    pub verify_tls: bool,
    /// The token to authenticate the probe with: the form's typed token, else the
    /// resolved env/config token (token_env / `NBOX_TOKEN` / profile `token`). If
    /// absent the probe runs unauthenticated and likely 401s (surfaced as failure).
    pub token: Option<String>,
}

impl std::fmt::Debug for ConnectRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is a secret: never print its value, only whether one is set.
        f.debug_struct("ConnectRequest")
            .field("url", &self.url)
            .field("auth_scheme", &self.auth_scheme)
            .field("verify_tls", &self.verify_tls)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ConnectRequest {
    /// The token to probe with. A thin accessor kept so the spawned probe has a
    /// single call site (the token is already fully resolved when the request is
    /// built — there is no deferred keyring tier any more).
    #[must_use]
    pub fn resolved_token(&self) -> Option<String> {
        self.token.clone()
    }
}

impl ConnectRequest {
    /// Build a [`ProfileConfig`] for this candidate (for `NetBoxClient::new`). The
    /// token isn't part of the profile (it's resolved separately / passed
    /// alongside), so it never lands in a serialized profile.
    pub fn to_profile(&self) -> ProfileConfig {
        ProfileConfig {
            url: self.url.clone(),
            auth_scheme: Some(self.auth_scheme),
            verify_tls: Some(self.verify_tls),
            ..Default::default()
        }
    }
}

/// A recently-opened object, for quick reopening from the home screen.
#[derive(Debug, Clone)]
pub struct RecentItem {
    pub kind: ObjectKind,
    pub id: u64,
    pub title: String,
}

/// A configured NetBox profile the session can switch to without restarting: its
/// config-file name and the [`ProfileConfig`] used to rebuild the client. Held in
/// [`App::profiles`] in config order; cycling/selecting picks one and triggers an
/// async reconnect + re-probe (see [`AppCommand::SwitchProfile`]).
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    pub name: String,
    pub config: ProfileConfig,
}

/// Build the live [`ApiConfig`] for a profile from the editor's per-surface
/// backend choices. REST is the implicit default, so a REST-everywhere profile
/// gets `None` (no `[api]` table) — mirroring what `set_profile_api_backend`
/// writes to disk. Only a GraphQL surface is recorded; `search` is never set here
/// (search always falls back to REST, so the editor doesn't surface it).
fn build_api_config(vrf: BackendPreference, route_target: BackendPreference) -> Option<ApiConfig> {
    let some_if_graphql = |p: BackendPreference| (p == BackendPreference::Graphql).then_some(p);
    let api = ApiConfig {
        search: None,
        vrf: some_if_graphql(vrf),
        route_target: some_if_graphql(route_target),
    };
    (api != ApiConfig::default()).then_some(api)
}

/// Most-recent-first cap for the recents list.
const RECENT_CAP: usize = 20;

/// Pre-render fallback for the results-list PgUp/PgDn step, used before the first
/// render has stashed a live viewport height (see [`App::list_page`]). Once a
/// frame has been drawn the list pages by ~one screenful instead, matching the
/// detail/preview panes.
const PAGE_JUMP_FALLBACK: usize = 10;

/// A monotonic per-channel request id, stamped on a spawned full-search or
/// full-detail command and echoed back on its result event. The pure handler
/// drops a result whose id is older than the latest spawned for that channel, so
/// a slow earlier request can't clobber the UI after a newer one (mirrors the
/// preview path's `(kind, id)` stale-response suppression, scoped per channel).
pub type RequestId = u64;

/// A short footer-notice expiry, driven by the 180ms `PreviewTick`.
const TRANSIENT_STATUS_TICKS: u8 = 10;

/// Side-effecting work the loop should spawn off the render thread.
#[derive(Debug, Clone)]
pub enum AppCommand {
    /// A full search. Carries the search channel's request id (stamped at
    /// dispatch) so a stale `SearchComplete` can be dropped on arrival.
    Search {
        query: String,
        req: RequestId,
        /// The active search filters, snapshotted at dispatch (the dispatcher has
        /// no `App` handle). Resolved by `NetBoxClient::search`, exactly as the CLI.
        filters: SearchFilters,
    },
    /// Browse all objects of one kind into the Results pane (the Nav pane picked a
    /// kind). Carries the browse channel's request id so a stale `BrowseComplete`
    /// is dropped on arrival, like `Search`.
    Browse {
        kind: ObjectKind,
        req: RequestId,
    },
    LoadDetail {
        kind: ObjectKind,
        id: u64,
        /// The detail channel's request id, for stale-`DetailLoaded` suppression.
        req: RequestId,
        /// Bust the cache for this object before loading — set by an explicit
        /// refresh (`r` / auto-tick), so a refresh never re-serves a cached copy.
        force: bool,
    },
    /// Load the overview dashboard (status counts / top prefixes / recent journal).
    /// Tagged with a request id so a stale `DashboardLoaded` is dropped.
    LoadDashboard {
        req: RequestId,
    },
    /// Load the IPAM prefix tree (one capped page, grouped by VRF). Tagged with a
    /// request id so a stale `PrefixTreeLoaded` is dropped.
    LoadPrefixTree {
        req: RequestId,
    },
    /// Fetch per-kind object counts for the Nav pane labels (background, no
    /// spinner). Idempotent — a late result just overwrites, so it carries no
    /// request-id guard.
    LoadNavCounts,
    /// Load the highlighted result's detail into the live preview pane. Carries
    /// its (kind, id) so the response can be matched against the selection it
    /// was issued for (stale ones are dropped on arrival).
    LoadPreview {
        kind: ObjectKind,
        id: u64,
    },
    LoadByRef {
        kind: ObjectKind,
        value: String,
        /// The detail channel's request id (shared with `LoadDetail`, since both
        /// resolve back through `DetailLoaded`), for stale-response suppression.
        req: RequestId,
        /// Bust the cache for the resolved object before loading (see
        /// [`AppCommand::LoadDetail::force`]).
        force: bool,
    },
    /// Open `url` in the browser, honoring the live `open_browser_command`
    /// (carried so the next `o` uses a just-changed setting). When `command` is
    /// empty the OS default opener is used. The URL is appended as a literal final
    /// argument by [`crate::config::open_url`] — never shell-interpolated.
    OpenBrowser {
        url: String,
        command: String,
    },
    Copy(String),
    /// Re-arm the auto-refresh ticker with a new interval (the Settings section
    /// changed `refresh_secs`). Handled by the event loop, which aborts the old
    /// ticker and spawns one at the new interval (or none when `None`/`0`). Not a
    /// fetch — it spawns no network and drives no spinner.
    ArmRefresh(Option<u64>),
    /// Switch the live session to the named profile: rebuild the NetBox client
    /// from its [`ProfileConfig`] and re-probe `/api/status/`, off the render
    /// thread. The result returns as [`AppEvent::ProfileSwitched`]; `id` (the
    /// monotonic switch id, echoed back) is what a superseded switch is dropped
    /// by on arrival. `name` is carried for display only.
    SwitchProfile {
        id: RequestId,
        name: String,
        config: ProfileConfig,
        /// The active config file path, kept for call-site signature stability in
        /// [`crate::config::resolve_token`]. `None` when the session has no backing
        /// file (the token then comes from env/config only).
        config_path: Option<PathBuf>,
    },
    /// Test-connect a candidate profile from the Config modal: build a temporary
    /// client from `req` and probe `/api/status/` (the same `verify_compatible`
    /// path a real connect/switch uses), off the render thread. The result returns
    /// as [`AppEvent::ConnectTested`]; `id` (the monotonic test id, echoed back)
    /// is what a superseded test is dropped by on arrival. The carried token is a
    /// secret — never logged (see [`ConnectRequest`]'s redacting `Debug`).
    TestConnect {
        id: RequestId,
        req: ConnectRequest,
    },
}

impl AppCommand {
    /// True when this command kicks off a *user-visible* network wait whose result
    /// returns as an `AppEvent`: a search, a detail open, a dashboard/tree load, a
    /// profile switch, a test-connect. These bump the in-flight counter so the
    /// footer spinner runs until the matching result lands.
    ///
    /// `LoadPreview` is deliberately excluded: the preview pane reloads on every
    /// selection change as you scroll the results, and pulsing the global spinner
    /// for that background work makes simple navigation feel noisy. A preview
    /// failure surfaces in the preview pane itself, not via the spinner.
    /// `OpenBrowser`/`Copy` are fire-and-forget (their async `Status` push isn't a
    /// tracked fetch).
    fn is_fetch(&self) -> bool {
        matches!(
            self,
            AppCommand::Search { .. }
                | AppCommand::Browse { .. }
                | AppCommand::LoadDetail { .. }
                | AppCommand::LoadByRef { .. }
                | AppCommand::LoadDashboard { .. }
                | AppCommand::LoadPrefixTree { .. }
                | AppCommand::SwitchProfile { .. }
                | AppCommand::TestConnect { .. }
        )
    }
}

/// The whole TUI application state.
pub struct App {
    pub client: NetBoxClient,
    /// The read cache shared with the dispatch tasks. Defaults to a disabled
    /// no-op (so tests see no caching); the real one is installed by `run_tui`.
    pub cache: Cache,
    /// Freshness of the detail currently shown (set from `DetailFreshness`,
    /// rendered in the footer). `None` until a detail is loaded / when it errors.
    pub detail_freshness: Option<Freshness>,
    pub theme: Theme,
    pub theme_index: usize,
    pub initial_theme: String,
    pub config_path: Option<PathBuf>,
    pub profile_name: String,
    pub base_url: String,
    pub netbox_version: String,
    /// Live `[ui].refresh_secs`: the TUI auto-refresh interval (0/None = off).
    /// Mirrors the value the ticker was armed with at launch; the Settings section
    /// re-arms the ticker through [`AppCommand::ArmRefresh`] when it changes.
    pub refresh_secs: Option<u64>,
    /// Live `[ui].open_browser_command`: a custom browser-open command (empty =
    /// the OS default). Carried on each [`AppCommand::OpenBrowser`] so `o` uses the
    /// just-changed value without a restart. Never holds a token or a URL.
    pub open_browser_command: String,
    /// Live top-level `log_level` / `log_file`, seeded from config so the Settings
    /// section edits start from the configured values. These persist on save but
    /// apply on the next launch (tracing inits at startup), so they're seed/persist
    /// only — not consulted at runtime here.
    pub log_level: Option<String>,
    pub log_file: Option<String>,
    /// All configured profiles, in config order, that the session can switch
    /// between without restarting. Empty/one-element ⇒ cycling is a graceful
    /// no-op. Populated at launch via [`App::set_profiles`].
    pub profiles: Vec<ProfileEntry>,
    /// Index into [`Self::profiles`] of the active profile. Cycling advances /
    /// wraps it; the palette `profile <name>` verb jumps to a named one.
    pub profile_index: usize,
    /// The target profile name of the in-flight switch, for *display* ("switching
    /// to '…'") and for stepping rapid cycles from the pending target. Set when a
    /// switch is initiated, cleared when the matching (by-id) completion settles.
    /// Correctness — whether a [`AppEvent::ProfileSwitched`] is current — rides
    /// [`Self::pending_switch`]/[`Self::switch_gen`], NOT this name: two switches
    /// to the same name are distinguished only by their id (see
    /// [`Self::is_current_switch`]).
    pub pending_profile: Option<String>,
    /// Monotonic profile-switch id source. Each initiated [`AppCommand::SwitchProfile`]
    /// bumps this and records the new value as both [`Self::pending_switch`] (the
    /// awaited completion) and [`Self::switch_gen`] (the high-water mark). The
    /// matching [`AppEvent::ProfileSwitched`] echoes the id back; one whose id is
    /// older than `switch_gen` is from a superseded switch and is dropped — even a
    /// switch to the same profile name can never settle a newer attempt. Mirrors
    /// the search/detail `request_seq`/`*_gen` guard, scoped to switches.
    pub switch_seq: RequestId,
    /// The id of the switch awaited by [`Self::pending_profile`], or `None` when no
    /// switch is in flight. A `ProfileSwitched` settles state only if its id equals
    /// this (and so also `>= switch_gen`); see [`Self::is_current_switch`].
    pub pending_switch: Option<RequestId>,
    /// High-water mark: the id of the latest initiated switch. A `ProfileSwitched`
    /// with an older id is dropped on arrival (latest-switch-wins).
    pub switch_gen: RequestId,

    pub mode: Mode,
    pub screen: Screen,
    /// The open floating overlay, if any (Help or the Config editor). Orthogonal
    /// to `screen`: a modal floats over the live Home/Detail view (see
    /// `tui::ui::render` drawing it last), so opening or closing it never disturbs
    /// the underlying screen or history. While a modal is open, keys route to it
    /// and are consumed. Help is any-key-close; Config has its own key handling.
    pub modal: Option<Modal>,
    /// Monotonic id source for Config-modal test-connect probes. Each
    /// [`AppCommand::TestConnect`] bumps this and records it as the high-water
    /// mark; an older [`AppEvent::ConnectTested`] is dropped (latest-test-wins),
    /// so editing the form and re-testing can't be settled by a stale probe.
    pub test_seq: RequestId,
    /// High-water mark: the id of the latest test-connect initiated.
    pub test_gen: RequestId,
    /// Which home pane has focus: Nav, the results list, or the preview. Movement
    /// keys route to it. `Tab`/`Shift+Tab` cycle it; only meaningful on Home.
    pub focus: Focus,
    /// Cursor into [`NAV_SECTIONS`] — the highlighted Navigation-pane row.
    pub nav_selected: usize,
    /// The [`Self::nav_selected`] value observed at the previous `PreviewTick`,
    /// used to gate live-browse on a settled cursor: a fast scroll through the
    /// rail keeps moving between ticks, so it only fetches once movement stops.
    pub nav_tick_anchor: usize,
    /// The kind currently browsed into the Results pane (the Nav pane picked it),
    /// or `None` when Results holds a search / the recents fallback. Drives the
    /// Results pane title.
    pub browse_kind: Option<ObjectKind>,
    /// The most recent kind browsed this session (set whenever the Nav rail
    /// browses a kind), persisted to `[ui].last_browsed` on exit and restored on
    /// the next launch so the Nav rail lands where you left off. `None` until a
    /// kind is browsed (or restored from config).
    pub last_browsed: Option<ObjectKind>,
    /// The `[ui].last_browsed` slug loaded at launch, to detect a change on exit
    /// (persist only when it actually moved), mirroring `initial_theme`.
    pub initial_last_browsed: Option<String>,
    /// Per-kind object totals shown in the Nav pane labels. Populated by a
    /// background count probe at launch (and after a profile switch); a kind absent
    /// from the map just shows no number.
    pub nav_counts: std::collections::HashMap<ObjectKind, u32>,
    pub history: Vec<Screen>,
    pub status: String,
    /// Severity of the current `status` message, so the footer can color it via
    /// [`Theme::message_style`]. Set in lockstep with `status` (see
    /// [`App::set_status`]); resets to `Info` whenever the status is cleared.
    pub status_severity: Severity,
    /// Remaining fast UI ticks before the current status clears itself. `None`
    /// means the message is sticky until another action replaces or clears it.
    status_ttl: Option<u8>,

    /// The `/` search line editor. Text entry, backspace/delete, cursor
    /// movement and the visible cursor are delegated to the cheese-backed
    /// [`TextInput`] wrapper (see `tui::cheese`); the submit/cancel flow stays
    /// here. Read its text with [`TextInput::value`].
    pub search_input: TextInput,
    /// The `:` command-palette line editor — same cheese-backed wrapper.
    pub command_input: TextInput,
    pub last_query: Option<String>,
    /// Active search filters (status / scope / tenant / role / tag / vrf), applied
    /// to every search. Set via the palette `filter k=v` verb (and, later, the
    /// chips bar / `f` modal). Fed to `NetBoxClient::search` through
    /// [`AppCommand::Search`] — the same resolver the CLI uses.
    pub filters: SearchFilters,

    pub results: Vec<SearchResult>,
    /// Indices into `results` in display order (fuzzy-filtered while searching).
    pub view: Vec<usize>,
    /// Index of the cursor into the active home list (results or recents). The
    /// pure movement keys (j/k/g/G/PgUp/PgDn) own this; `table_state` is synced
    /// from it at render time so ratatui scrolls the offset to keep it visible.
    pub selected: usize,
    /// Stateful-table selection/offset, driven by `selected` each frame so the
    /// selected row is always on screen no matter how tall the table gets.
    /// `TableState` keeps the selected row visible exactly like `ListState` did,
    /// so the selection-stays-visible / page-jump viewport behaviour is unchanged.
    pub table_state: TableState,
    /// Last-known results-list inner height (visible rows), stashed during render
    /// so the pure PgUp/PgDn handler can page by one screenful. Mirrors the
    /// detail/preview live-viewport pattern. 0 until the first render, where the
    /// pre-render fallback ([`PAGE_JUMP_FALLBACK`]) applies instead.
    pub list_viewport: u16,
    /// On the next `SearchComplete`, try to re-select this (kind, id) — used to
    /// keep the cursor stable across an auto-refresh.
    pub pending_reselect: Option<(ObjectKind, u64)>,
    pub recent: Vec<RecentItem>,
    pub detail: Option<DetailView>,
    /// Per-object detail view state — `(kind, id) → (tab, scroll)` — so re-opening
    /// (or refreshing) an object you've already looked at restores its tab and
    /// scroll instead of snapping back to the summary at the top. Captured when a
    /// detail is left or replaced; cleared on a profile switch.
    detail_view_state: std::collections::HashMap<(ObjectKind, u64), (usize, u16)>,
    /// Active detail tab: 0 = summary, n>0 = `detail.tabs[n-1]`.
    pub detail_tab: usize,
    /// Selected row in an interactive detail section (a section with navigable
    /// rows — e.g. a VRF's prefix tree). Indexes the active section's rows; `Enter`
    /// opens the selected row's target. Ignored for plain text sections (which
    /// scroll instead). Reset to the first selectable row on load and tab switch.
    pub detail_row: usize,
    /// Vertical scroll offset (in lines) of the detail body. The pure scroll
    /// keys (j/k/g/G/PgUp/PgDn) own this; the render path feeds it straight to
    /// `Paragraph::scroll`. Reset to 0 on navigation and on a tab switch.
    pub detail_scroll: u16,
    /// Last-known detail pane inner height (content rows), stashed during render
    /// so the pure handler can clamp scrolling at the bottom without doing I/O.
    /// Mirrors the home list's live-viewport pattern. 0 until the first render.
    pub detail_viewport: u16,

    /// The (kind, id) whose full detail is currently loaded in the preview pane,
    /// if any. Compared against the live selection to detect staleness.
    pub preview_for: Option<(ObjectKind, u64)>,
    /// The detail loaded into the preview pane (None until a load returns, or
    /// while the selection is empty). Separate from `detail` so opening the full
    /// detail screen (Enter) never collides with the live peek.
    pub preview: Option<DetailView>,
    /// Set whenever the selection changes; cleared once a `LoadPreview` for the
    /// settled selection has been dispatched. The debounce: a preview load fires
    /// on the next `PreviewTick`, not on every keystroke — so a burst of j/k
    /// scrolling issues at most one fetch when the cursor finally settles.
    pub preview_dirty: bool,
    /// Set when the Nav-rail cursor moves; cleared once a debounced live-browse
    /// for the highlighted kind has been dispatched. The Nav-side twin of
    /// [`Self::preview_dirty`]: a burst of `j`/`k` on the rail coalesces into a
    /// single `Browse` when the cursor settles (see [`App::on_nav_browse_tick`]),
    /// so scrolling the rail previews each kind's list without leaving Nav focus.
    pub browse_dirty: bool,
    /// Vertical scroll offset of the preview body, owned by the pure scroll keys
    /// while the Preview pane is focused. Reset when the previewed object changes.
    pub preview_scroll: u16,
    /// Last-known preview pane inner height (content rows), stashed at render so
    /// the pure scroll handler can clamp at the bottom. 0 until the first render.
    pub preview_viewport: u16,

    /// Monotonic request-id source for the full-search and full-detail channels.
    /// Each spawned `Search`/`LoadDetail`/`LoadByRef` is stamped with the next id
    /// here (see [`App::stamp_request_ids`]); the latest stamped id per channel is
    /// kept in [`Self::search_gen`]/[`Self::detail_gen`] so a stale (older-id)
    /// result event can be dropped, the same way preview loads drop a stale
    /// `(kind, id)`.
    pub request_seq: RequestId,
    /// The latest request id stamped on a spawned full search. A `SearchComplete`
    /// older than this is from a superseded request and is dropped on arrival.
    pub search_gen: RequestId,
    /// High-water mark for the browse-by-kind channel; a `BrowseComplete` older
    /// than this is dropped (latest-wins, like search).
    pub browse_gen: RequestId,
    /// The latest request id stamped on a spawned full-detail load (`LoadDetail`
    /// or `LoadByRef`). A `DetailLoaded` older than this is dropped on arrival.
    pub detail_gen: RequestId,
    /// High-water mark for the overview dashboard load; a `DashboardLoaded` with an
    /// older id is dropped (latest-wins, like search/detail).
    pub dashboard_gen: RequestId,
    /// The loaded overview data (`None` until the first load settles).
    pub dashboard: Option<DashboardData>,
    /// The last dashboard load error, shown on the dashboard when a load fails.
    pub dashboard_error: Option<String>,

    /// The object-level back-stack for cross-object navigation: each related-link
    /// jump (the `R` modal) pushes the object you jumped *from*, so `b`/`Esc` walks
    /// back through the drill path one object at a time. Cleared when a fresh detail
    /// is opened from a non-detail screen (a new drill path starts).
    pub detail_nav: Vec<(ObjectKind, u64)>,

    /// High-water mark for the prefix-tree load; a `PrefixTreeLoaded` with an
    /// older id is dropped (latest-wins, like search/detail/dashboard).
    pub prefix_tree_gen: RequestId,
    /// The loaded prefix tree (`None` until the first load settles).
    pub prefix_tree: Option<PrefixTreeData>,
    /// The last prefix-tree load error, shown on the screen when a load fails.
    pub prefix_tree_error: Option<String>,
    /// Cursor into the *visible* prefix-tree rows (those not under a collapsed
    /// ancestor). Clamped on every move and after a collapse/expand reshapes the
    /// visible set.
    pub prefix_tree_selected: usize,
    /// Prefix ids whose subtree is collapsed (hidden). Toggled by Space/←/→ on the
    /// tree screen; ids that vanish on reload are harmless (just unused).
    pub prefix_tree_collapsed: std::collections::HashSet<u64>,
    /// Last-known prefix-tree viewport height (visible rows), stashed at render so
    /// the pure paging handler can step by a screenful. 0 until the first render.
    pub prefix_tree_viewport: u16,

    /// Count of fetching async commands currently in flight (Search / LoadDetail
    /// / LoadPreview / LoadByRef / refresh). Bumped when such a command is
    /// dispatched, decremented when its matching result event arrives; clamped at
    /// 0 so a stray/duplicate result can't drive it negative. The footer shows
    /// the spinner iff this is non-zero (see [`App::loading`]), so concurrent
    /// fetches all have to resolve before the UI reads as idle.
    pub pending: u32,
    /// The footer loading spinner. Advances one frame per tick *only while*
    /// [`App::loading`] (so it's still at rest), and resets to frame 0 when the
    /// last request settles. Confined behind the cheese adapter.
    pub spinner: Spinner,

    /// A newer release version (raw, may carry a leading `v`) when the background
    /// update check found one and the user hasn't dismissed the banner with `u`.
    /// `None` ⇒ no banner. Only ever populated when the `updates` feature is built.
    pub update_available: Option<String>,
    /// The install-appropriate upgrade command shown in the banner (e.g.
    /// `brew upgrade nbox`). Set once at launch by `build_tui_app`; empty in lean
    /// builds. A plain string so `ui.rs` needs no dependency on the `updates` module.
    pub update_command: &'static str,

    pub should_quit: bool,
}

impl App {
    /// Construct a fresh app on the home screen.
    pub fn new(
        client: NetBoxClient,
        theme_name: &str,
        profile_name: String,
        base_url: String,
        netbox_version: String,
        config_path: Option<PathBuf>,
    ) -> Self {
        Self {
            client,
            cache: Cache::disabled(),
            detail_freshness: None,
            theme: Theme::by_name(theme_name),
            theme_index: Theme::index_of(theme_name),
            initial_theme: Theme::by_name(theme_name).name().to_string(),
            config_path,
            profile_name,
            base_url,
            netbox_version,
            refresh_secs: None,
            open_browser_command: String::new(),
            log_level: None,
            log_file: None,
            profiles: Vec::new(),
            profile_index: 0,
            pending_profile: None,
            switch_seq: 0,
            pending_switch: None,
            switch_gen: 0,
            mode: Mode::Normal,
            screen: Screen::Home,
            modal: None,
            test_seq: 0,
            test_gen: 0,
            // Open on the Browse rail so the first thing a user sees is the
            // browse-by-kind entry point (with live counts); the cursor starts on
            // Recent, which the Results pane mirrors until a kind is chosen.
            focus: Focus::Nav,
            nav_selected: NAV_SECTIONS.len() - 1, // Recent, matching the default view
            nav_tick_anchor: NAV_SECTIONS.len() - 1,
            browse_kind: None,
            last_browsed: None,
            initial_last_browsed: None,
            nav_counts: std::collections::HashMap::new(),
            history: Vec::new(),
            status: String::new(),
            status_severity: Severity::Info,
            status_ttl: None,
            search_input: TextInput::new("search NetBox…"),
            command_input: TextInput::new("command (e.g. device edge01)"),
            last_query: None,
            filters: SearchFilters::default(),
            results: Vec::new(),
            view: Vec::new(),
            selected: 0,
            table_state: TableState::default(),
            list_viewport: 0,
            pending_reselect: None,
            recent: Vec::new(),
            detail: None,
            detail_tab: 0,
            detail_row: 0,
            detail_scroll: 0,
            detail_viewport: 0,
            preview_for: None,
            preview: None,
            preview_dirty: false,
            browse_dirty: false,
            preview_scroll: 0,
            preview_viewport: 0,
            request_seq: 0,
            search_gen: 0,
            browse_gen: 0,
            detail_gen: 0,
            detail_view_state: std::collections::HashMap::new(),
            dashboard_gen: 0,
            dashboard: None,
            dashboard_error: None,
            detail_nav: Vec::new(),
            prefix_tree_gen: 0,
            prefix_tree: None,
            prefix_tree_error: None,
            prefix_tree_selected: 0,
            prefix_tree_collapsed: std::collections::HashSet::new(),
            prefix_tree_viewport: 0,
            pending: 0,
            spinner: Spinner::new(),
            update_available: None,
            update_command: "",
            should_quit: false,
        }
    }

    /// Restore the Nav rail's last-browsed kind from `[ui].last_browsed` (a kind
    /// slug). When it resolves to a browsable section, the cursor lands on it and
    /// `browse_kind` is primed so the event loop can preload that kind's list at
    /// startup (focus stays on the Nav rail). An unknown/absent slug leaves the
    /// default (cursor on Recent). `initial_last_browsed` is pinned for the
    /// exit-time persist guard, mirroring `initial_theme`.
    #[must_use]
    pub fn with_last_browsed(mut self, slug: Option<String>) -> Self {
        if let Some(s) = &slug
            && let Some(idx) = nav_section_index_for_slug(s)
        {
            self.nav_selected = idx;
            let kind = NAV_SECTIONS[idx].object_kind();
            self.browse_kind = kind;
            self.last_browsed = kind;
        }
        // Pin the loaded slug (moved, not cloned) for the exit-time persist guard.
        self.initial_last_browsed = slug;
        self
    }

    /// The kind to preload at startup, if a last-browsed kind was restored — the
    /// event loop dispatches a `Browse` for it so the list pane lands populated.
    #[must_use]
    pub fn startup_browse(&self) -> Option<ObjectKind> {
        self.browse_kind
    }

    /// Switch the app into the monochrome `NO_COLOR` mode: the theme renders with
    /// no color at all (see [`Theme::no_color`]). Wired at TUI startup when
    /// `NO_COLOR` is set in the environment. `initial_theme` is pinned to the
    /// no-color sentinel too, so the exit-time theme-persistence guard
    /// (`theme.name() != initial_theme`) stays a no-op and we never write
    /// `"no_color"` back into the user's config.
    pub fn set_no_color(&mut self) {
        self.theme = Theme::no_color();
        self.theme_index = 0;
        self.initial_theme = self.theme.name().to_string();
    }

    /// Seed the live UI settings from the loaded config at launch, so the Settings
    /// section edits — and the `o` open path — start from the configured values.
    /// Called once in `tui::run_tui`.
    pub fn set_ui_settings(
        &mut self,
        refresh_secs: Option<u64>,
        open_browser_command: String,
        log_level: Option<String>,
        log_file: Option<String>,
    ) {
        self.refresh_secs = refresh_secs;
        self.open_browser_command = open_browser_command;
        self.log_level = log_level;
        self.log_file = log_file;
    }

    /// Seed the footer status line + its severity at launch. Used by first-run
    /// onboarding to carry guidance into the freshly-launched app (e.g. "set
    /// NBOX_TOKEN or a token_env") so the message isn't lost on the hand-off from
    /// the wizard to the app.
    pub fn set_initial_status(&mut self, message: impl Into<String>, severity: Severity) {
        self.set_status(message, severity);
    }

    /// Attach the configured profiles the session can switch between, and point
    /// [`Self::profile_index`] at whichever matches the active [`Self::profile_name`]
    /// (falling back to 0). Called once at launch (see `tui::run_tui`). With zero
    /// or one profile, the switch hotkey is a graceful no-op (see
    /// [`Self::cycle_profile`]).
    pub fn set_profiles(&mut self, profiles: Vec<ProfileEntry>) {
        self.profile_index = profiles
            .iter()
            .position(|p| p.name == self.profile_name)
            .unwrap_or(0);
        self.profiles = profiles;
    }

    /// Install the live read cache (called once by `run_tui` after connecting).
    pub fn set_cache(&mut self, cache: Cache) {
        self.cache = cache;
    }

    /// Apply an event, returning any commands to dispatch. The commands handed
    /// back are accounted into the in-flight counter (each fetch bumps it) so the
    /// footer spinner runs until the matching result event lands.
    pub fn handle_event(&mut self, event: AppEvent) -> Vec<AppCommand> {
        let mut commands = self.dispatch_event(event);
        self.stamp_request_ids(&mut commands);
        self.track_dispatched(&commands);
        commands
    }

    /// Stamp the about-to-be-spawned full-search / full-detail commands with a
    /// fresh per-channel request id, recording the newest in
    /// [`Self::search_gen`]/[`Self::detail_gen`]. Handlers build these commands
    /// with a placeholder id (0); the single id source lives here so every
    /// dispatch site is tagged consistently and the latest-wins guard
    /// ([`Self::is_current_search`]/[`Self::is_current_detail`]) has an
    /// authoritative high-water mark. Preview loads carry their own `(kind, id)`
    /// tag and are untouched. Order is preserved within the (rare) multi-fetch
    /// batch so the last of a kind sets the high-water mark.
    fn stamp_request_ids(&mut self, commands: &mut [AppCommand]) {
        for command in commands.iter_mut() {
            match command {
                AppCommand::Search { req, .. } => {
                    self.request_seq += 1;
                    *req = self.request_seq;
                    self.search_gen = self.request_seq;
                }
                AppCommand::Browse { req, .. } => {
                    self.request_seq += 1;
                    *req = self.request_seq;
                    self.browse_gen = self.request_seq;
                }
                AppCommand::LoadDetail { req, .. } | AppCommand::LoadByRef { req, .. } => {
                    self.request_seq += 1;
                    *req = self.request_seq;
                    self.detail_gen = self.request_seq;
                    // A new detail load is starting; drop the previous freshness
                    // badge so it can't linger over the incoming object.
                    self.detail_freshness = None;
                }
                AppCommand::LoadDashboard { req } => {
                    self.request_seq += 1;
                    *req = self.request_seq;
                    self.dashboard_gen = self.request_seq;
                }
                AppCommand::LoadPrefixTree { req } => {
                    self.request_seq += 1;
                    *req = self.request_seq;
                    self.prefix_tree_gen = self.request_seq;
                }
                // Preview loads carry (kind,id); profile switches carry their own
                // monotonic switch id, stamped at initiation in `switch_to_index`
                // (it also sets the pending/high-water state). Neither rides the
                // per-channel search/detail request-id guard.
                AppCommand::LoadNavCounts
                | AppCommand::LoadPreview { .. }
                | AppCommand::OpenBrowser { .. }
                | AppCommand::Copy(_)
                | AppCommand::ArmRefresh(_)
                | AppCommand::SwitchProfile { .. }
                | AppCommand::TestConnect { .. } => {}
            }
        }
    }

    /// True when `req` is the newest full-search request spawned. A
    /// `SearchComplete` for an older id is from a superseded request and is
    /// dropped so a slow earlier search can't overwrite newer results.
    fn is_current_search(&self, req: RequestId) -> bool {
        req >= self.search_gen
    }

    fn is_current_browse(&self, req: RequestId) -> bool {
        req >= self.browse_gen
    }

    /// True when `req` is the newest full-detail request spawned (the
    /// `LoadDetail`/`LoadByRef` channel). An older `DetailLoaded` is dropped.
    fn is_current_detail(&self, req: RequestId) -> bool {
        req >= self.detail_gen
    }

    /// The event→commands transition itself. Result events that settle an
    /// in-flight fetch decrement the counter here; the commands they (don't)
    /// return are then accounted by [`Self::handle_event`].
    fn dispatch_event(&mut self, event: AppEvent) -> Vec<AppCommand> {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Resize(_, _) => Vec::new(),
            AppEvent::Tick => self.on_tick(),
            AppEvent::PreviewTick => {
                // Reuse the always-on preview tick to drive the spinner: advance
                // a frame only while something is in flight, so it's still at
                // rest (no busy-spin) when idle. The preview debounce flush is
                // independent and runs regardless.
                if self.loading() {
                    self.spinner.tick();
                }
                // Flush the Nav-rail live-browse first (it may replace the results,
                // which then dirties the preview), then the preview debounce.
                let mut commands = self.on_nav_browse_tick();
                commands.extend(self.on_preview_tick());
                self.tick_status_ttl();
                commands
            }
            AppEvent::SearchComplete { req, result } => {
                // A search result settles its in-flight fetch (clean or error),
                // counted down even when dropped as stale below — otherwise the
                // spinner would hang on a superseded request (mirrors the preview
                // path). Then drop a stale (older-id) response so a slow earlier
                // search can't clobber newer results.
                self.end_request();
                if !self.is_current_search(req) {
                    return Vec::new();
                }
                match result {
                    Ok(outcome) => {
                        if outcome.errors.is_empty() {
                            // A clean, complete result set is a confirmation.
                            self.set_status(
                                format!("{} result(s)", outcome.results.len()),
                                Severity::Success,
                            );
                        } else {
                            // Some endpoints failed: a degraded / partial result.
                            self.set_status(
                                format!(
                                    "{} result(s) (partial: {} endpoint(s) failed)",
                                    outcome.results.len(),
                                    outcome.errors.len()
                                ),
                                Severity::Warning,
                            );
                        }
                        // These results came from a search, not a Nav browse.
                        self.browse_kind = None;
                        self.results = outcome.results;
                        self.view = (0..self.results.len()).collect();
                        self.selected = self
                            .pending_reselect
                            .take()
                            .and_then(|(kind, id)| {
                                self.view.iter().position(|&i| {
                                    self.results[i].kind == kind && self.results[i].id == id
                                })
                            })
                            .unwrap_or(0);
                        // The highlighted result may have changed; let the
                        // debounce decide whether to (re)load the preview.
                        self.mark_preview_dirty();
                    }
                    Err(e) => self.set_status(format!("error: {e:#}"), Severity::Error),
                }
                Vec::new()
            }
            AppEvent::BrowseComplete { req, kind, result } => {
                // Settle the in-flight fetch (counted down even if dropped as
                // stale, so the spinner can't hang), then drop a superseded browse.
                self.end_request();
                if !self.is_current_browse(req) {
                    return Vec::new();
                }
                match result {
                    Ok(items) => {
                        self.set_status(format!("{} result(s)", items.len()), Severity::Success);
                        self.browse_kind = Some(kind);
                        self.last_query = None; // these results came from browse, not search
                        self.results = items;
                        self.view = (0..self.results.len()).collect();
                        self.selected = 0;
                        self.mark_preview_dirty();
                    }
                    Err(e) => self.set_status(format!("error: {e:#}"), Severity::Error),
                }
                Vec::new()
            }
            AppEvent::DetailLoaded { req, result } => {
                // The full-detail load (from Enter / a palette lookup) settled —
                // counted down even if dropped as stale, so the spinner can't hang
                // on a superseded request. A stale (older-id) response is dropped
                // so a slow earlier load can't navigate over a newer one.
                self.end_request();
                if !self.is_current_detail(req) {
                    return Vec::new();
                }
                match result {
                    Ok(view) => {
                        self.record_recent(view.kind, view.id, view.title.clone());
                        // A detail opened from a non-detail screen starts a fresh
                        // drill path; a detail→detail load (a related-link jump or a
                        // back-stack walk) keeps the path it already pushed/popped.
                        if self.screen != Screen::Detail {
                            self.detail_nav.clear();
                        }
                        self.navigate_to(Screen::Detail);
                        // Snapshot the outgoing detail, then restore this object's
                        // saved tab/scroll (summary/top if it's the first visit).
                        self.save_detail_view_state();
                        let key = (view.kind, view.id);
                        let max_tab = view.tabs.len();
                        self.detail = Some(view);
                        let (tab, scroll) =
                            self.detail_view_state.get(&key).copied().unwrap_or((0, 0));
                        self.detail_tab = tab.min(max_tab);
                        self.detail_scroll = scroll.min(self.detail_max_scroll());
                        // Position the row cursor on the first selectable row of the
                        // active section (a no-op for plain text sections).
                        self.reset_detail_row();
                        self.clear_status();
                    }
                    Err(e) => self.set_status(format!("error: {e:#}"), Severity::Error),
                }
                Vec::new()
            }
            AppEvent::DashboardLoaded { req, result } => {
                // Settle the spinner even when dropped as stale; an older-id load
                // (the user pressed `r`/reopened) never overwrites the newest.
                self.end_request();
                if req < self.dashboard_gen {
                    return Vec::new();
                }
                match result {
                    Ok(data) => {
                        self.dashboard = Some(data);
                        self.dashboard_error = None;
                        self.clear_status();
                    }
                    Err(e) => {
                        self.dashboard_error = Some(format!("{e:#}"));
                        self.set_status(format!("dashboard error: {e:#}"), Severity::Error);
                    }
                }
                Vec::new()
            }
            AppEvent::PrefixTreeLoaded { req, result } => {
                // Settle the spinner even when dropped as stale; an older-id load
                // (the user pressed `r`/reopened) never overwrites the newest.
                self.end_request();
                if req < self.prefix_tree_gen {
                    return Vec::new();
                }
                match result {
                    Ok(data) => {
                        self.prefix_tree = Some(data);
                        self.prefix_tree_error = None;
                        // A fresh tree resets the cursor; collapsed ids are kept so a
                        // refresh preserves the user's expand/collapse shape.
                        self.prefix_tree_selected = 0;
                        self.clamp_tree_selection();
                        self.clear_status();
                    }
                    Err(e) => {
                        self.prefix_tree_error = Some(format!("{e:#}"));
                        self.set_status(format!("prefix tree error: {e:#}"), Severity::Error);
                    }
                }
                Vec::new()
            }
            AppEvent::PreviewLoaded { kind, id, result } => {
                // Preview loads aren't counted as in-flight (they don't drive the
                // spinner — see `AppCommand::is_fetch`), so there's nothing to
                // settle here. Stale-response suppression still applies: only adopt
                // this load if it still matches the highlighted result; a response
                // for a selection the user has already scrolled past is dropped.
                if self.preview_selection() == Some((kind, id)) {
                    match result {
                        Ok(view) => {
                            self.preview = Some(view);
                            self.preview_for = Some((kind, id));
                            self.preview_scroll = 0;
                        }
                        // Keep the lightweight peek; surface the failure quietly.
                        Err(e) => self.set_status(format!("preview error: {e:#}"), Severity::Error),
                    }
                }
                Vec::new()
            }
            AppEvent::DetailFreshness { req, freshness } => {
                // Adopt freshness only for the newest detail request (a stale one
                // belongs to a superseded load). Not a fetch settle — `DetailLoaded`
                // already counted it down.
                if self.is_current_detail(req) {
                    self.detail_freshness = Some(freshness);
                }
                Vec::new()
            }
            AppEvent::ProfileSwitched { id, name, result } => {
                // The reconnect+re-probe settled — count it down even when dropped
                // as stale below, so the spinner can't hang on a superseded switch.
                self.end_request();
                // Latest-switch-wins, correlated by the monotonic switch `id` (not
                // the name): a result from a switch the user has already superseded
                // — even an older one to this same profile name — is dropped, since
                // its client/version is moot now. This must not touch
                // `pending_switch`/`pending_profile`: dropping a stale completion
                // can't clear a newer switch's pending state or flip anything.
                if !self.is_current_switch(id) {
                    return Vec::new();
                }
                self.pending_profile = None;
                self.pending_switch = None;
                match result {
                    Ok((client, version)) => {
                        // Atomically adopt the new instance: swap in its client and
                        // flip the header/`base_url`/index/version to the target
                        // profile together, then drop the old instance's data and
                        // bump the request generations so any straggler response
                        // from the previous instance lands stale. This is the ONLY
                        // place the header moves, so it always matches `client`.
                        self.client = client;
                        self.netbox_version = version;
                        if let Some(idx) = self.profiles.iter().position(|p| p.name == name) {
                            self.profile_index = idx;
                            self.base_url.clone_from(&self.profiles[idx].config.url);
                        }
                        self.profile_name.clone_from(&name);
                        // Re-key the cache to the new connection so a switch can
                        // never serve the previous profile's cached views. The
                        // store is shared, so switching back stays warm.
                        let partition = crate::cache::profile_partition(
                            &self.profile_name,
                            self.client.base_url().as_str(),
                        );
                        self.cache = self.cache.with_partition(partition);
                        self.clear_for_profile_switch();
                        self.set_status(
                            format!("switched to '{name}' (NetBox v{})", self.netbox_version),
                            Severity::Success,
                        );
                        // Refresh the Nav counts for the newly-connected instance.
                        return vec![AppCommand::LoadNavCounts];
                    }
                    // The new instance was unreachable/incompatible: the old client
                    // is still valid, so a failed switch is a no-op + error toast.
                    // The header was never flipped, so it still matches the connected
                    // instance — no phantom. The UI stays fully usable on the old one.
                    Err(e) => {
                        self.set_status(format!("profile '{name}' error: {e:#}"), Severity::Error);
                    }
                }
                Vec::new()
            }
            AppEvent::ConnectTested { id, result } => {
                // The test-connect probe settled — count it down even when dropped
                // as stale below, so the spinner can't hang on a superseded test.
                self.end_request();
                // Latest-test-wins: drop a probe the user has already superseded by
                // editing + re-testing (its result describes stale form contents).
                if id < self.test_gen {
                    return Vec::new();
                }
                // Land the result in the open form, if one is still showing. (The
                // user may have navigated away; then there's nothing to update.)
                if let Some(Modal::Config(modal)) = &mut self.modal
                    && let Some(form) = modal.form_mut()
                {
                    form.test = match result {
                        Ok(version) => TestState::Ok(version),
                        Err(e) => TestState::Failed(format!("{e:#}")),
                    };
                }
                Vec::new()
            }
            AppEvent::UpdateAvailable(version) => {
                // Store raw; the banner strips a leading `v` at render (xfr's fix).
                // `None` (up to date / skipped) just leaves the banner off.
                self.update_available = version;
                Vec::new()
            }
            AppEvent::NavCounts(counts) => {
                // Background nav-label totals; last write wins (no stale guard).
                self.nav_counts = counts.into_iter().collect();
                Vec::new()
            }
            AppEvent::Status(message) => {
                // An async status push (e.g. "copied …"/"opened …"): classify it
                // so confirmations and failures still get the right color.
                // Confirmations fade like other transient notices; failures
                // persist (until the next action) so they aren't missed.
                let severity = classify_status(&message);
                if severity == Severity::Error {
                    self.set_status(message, severity);
                } else {
                    self.set_transient_status(message, severity);
                }
                Vec::new()
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        if key.kind != KeyEventKind::Press {
            return Vec::new();
        }
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Search => self.handle_search_key(key),
            Mode::Command => self.handle_command_key(key),
        }
    }

    /// True when at least one fetching request is in flight, so the footer should
    /// show the loading spinner. Pure: just reads the [`Self::pending`] counter.
    pub fn loading(&self) -> bool {
        self.pending > 0
    }

    /// Mark one more fetching request as dispatched.
    fn begin_request(&mut self) {
        self.pending = self.pending.saturating_add(1);
    }

    /// Mark one in-flight fetching request as settled, clamping at 0 so a stray
    /// or duplicate result event can't drive the counter negative. When the last
    /// request settles the spinner is reset so the next one starts clean.
    fn end_request(&mut self) {
        self.pending = self.pending.saturating_sub(1);
        if self.pending == 0 {
            self.spinner.reset();
        }
    }

    /// Account for the fetching commands a handler is about to dispatch: each one
    /// bumps [`Self::pending`] so the spinner stays up until its result lands.
    /// Side-effect-free commands (open/copy) don't fetch and don't count.
    fn track_dispatched(&mut self, commands: &[AppCommand]) {
        for command in commands {
            if command.is_fetch() {
                self.begin_request();
            }
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // A modal floats over the live screen and consumes keys: while one is open
        // the key routes to it (Help = any-key-close; Config = its own handling)
        // and never also acts on the underlying screen. Ctrl+C still hard-quits
        // regardless, mirroring the rest of the app. Closing leaves `screen`/
        // history untouched.
        if self.modal.is_some() {
            if let KeyCode::Char('c') = key.code
                && ctrl
            {
                self.should_quit = true;
                return Vec::new();
            }
            return self.handle_modal_key(key);
        }
        match key.code {
            KeyCode::Char('c') if ctrl => self.should_quit = true,
            KeyCode::Char('q') => {
                if self.screen == Screen::Home {
                    self.should_quit = true;
                } else {
                    return self.go_back();
                }
            }
            KeyCode::Char('?') | KeyCode::F(1) => self.modal = Some(Modal::Help),
            // `S` opens the Config modal (Profiles section); also the palette
            // `config` verb. Free key — no clash with the bound set.
            KeyCode::Char('S') => self.open_config_modal(),
            // Dismiss the update banner (no-op when none is showing).
            KeyCode::Char('u') => self.update_available = None,
            // `Esc` on Home clears an active search (back to recents); otherwise it
            // navigates back. `b` is always plain back/navigation (kept distinct so
            // it never clears the search out from under an in-flight detail load).
            KeyCode::Esc => {
                if self.screen == Screen::Home && self.search_active() {
                    self.clear_search();
                } else {
                    return self.go_back();
                }
            }
            KeyCode::Char('b') => return self.go_back(),
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.search_input.reset();
                self.refilter();
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command_input.reset();
            }
            KeyCode::Char('t') => self.cycle_theme(),
            KeyCode::Char('r') => return self.refresh_current_view(),
            // Search filters: `f` opens the filter editor, `F` clears all filters.
            KeyCode::Char('f') => self.open_filter_modal(),
            KeyCode::Char('F') => return self.clear_filters(),
            // Overview dashboard.
            KeyCode::Char('D') => return self.open_dashboard(),
            // Prefix tree.
            KeyCode::Char('T') => return self.open_prefix_tree(),
            // Prefix-tree navigation + collapse/expand (only while on that screen,
            // so these keys are inert elsewhere). Movement is guarded ahead of the
            // generic select/scroll arms below.
            KeyCode::Char('j') | KeyCode::Down if self.screen == Screen::PrefixTree => {
                self.tree_move(1);
            }
            KeyCode::Char('k') | KeyCode::Up if self.screen == Screen::PrefixTree => {
                self.tree_move(-1);
            }
            KeyCode::Char('g') | KeyCode::Home if self.screen == Screen::PrefixTree => {
                self.prefix_tree_selected = 0;
            }
            KeyCode::Char('G') | KeyCode::End if self.screen == Screen::PrefixTree => {
                self.tree_select_last();
            }
            KeyCode::PageDown if self.screen == Screen::PrefixTree => {
                self.tree_move(self.tree_page());
            }
            KeyCode::PageUp if self.screen == Screen::PrefixTree => {
                self.tree_move(-self.tree_page());
            }
            // Space toggles the selected subtree; ←/h collapse, →/l expand.
            KeyCode::Char(' ') if self.screen == Screen::PrefixTree => self.tree_toggle(),
            KeyCode::Left | KeyCode::Char('h') if self.screen == Screen::PrefixTree => {
                self.tree_set_collapsed(true);
            }
            KeyCode::Right | KeyCode::Char('l') if self.screen == Screen::PrefixTree => {
                self.tree_set_collapsed(false);
            }
            // Profile switcher. `Tab` is taken on Home (pane focus), so the
            // configured-profile cycle rides `P` forward / `Ctrl+P` backward (a
            // free, mnemonic key); the palette `profile <name>` verb jumps to a
            // named one. Reconnects + re-probes the instance off the render thread.
            KeyCode::Char('p') if ctrl => return self.cycle_profile(false),
            KeyCode::Char('P') => return self.cycle_profile(true),
            // Tab / Shift+Tab cycle focus between the home split's panes, and on the
            // detail screen cycle the section tabs (summary → interfaces → …).
            KeyCode::Tab if self.screen == Screen::Home => self.cycle_focus(true),
            KeyCode::BackTab if self.screen == Screen::Home => self.cycle_focus(false),
            KeyCode::Tab if self.screen == Screen::Detail => self.cycle_detail_tab(true),
            KeyCode::BackTab if self.screen == Screen::Detail => self.cycle_detail_tab(false),
            // A detail section's letter key jumps to that tab. Dynamic (driven by
            // the loaded object's tabs) and placed ahead of the single-letter
            // global actions so a VRF's `t` (targets) wins over `t` (theme) while
            // on that detail; a key that isn't a tab of the current object falls
            // through to its global binding.
            KeyCode::Char(c) if !ctrl && self.detail_has_tab_key(c) => self.select_detail_tab(c),
            // The Nav pane owns movement when focused: j/k move the section cursor.
            KeyCode::Char('j') | KeyCode::Down if self.on_nav() => self.nav_move(true),
            KeyCode::Char('k') | KeyCode::Up if self.on_nav() => self.nav_move(false),
            KeyCode::Char('g') | KeyCode::Home if self.on_nav() => self.nav_jump(0),
            KeyCode::Char('G') | KeyCode::End if self.on_nav() => {
                self.nav_jump(NAV_SECTIONS.len() - 1);
            }
            // An interactive detail section (e.g. a VRF's prefix tree) takes the
            // movement keys to move its row cursor, ahead of the body-scroll route.
            KeyCode::Char('j') | KeyCode::Down if self.detail_list_active() => {
                self.detail_row_move(true);
            }
            KeyCode::Char('k') | KeyCode::Up if self.detail_list_active() => {
                self.detail_row_move(false);
            }
            KeyCode::Char('g') | KeyCode::Home if self.detail_list_active() => {
                self.detail_row_first();
            }
            KeyCode::Char('G') | KeyCode::End if self.detail_list_active() => {
                self.detail_row_last();
            }
            // Movement keys route to whatever owns scrolling/selection right now:
            // the detail screen scrolls its body; on Home the focused pane decides
            // (List → move the selection, Preview → scroll the preview body).
            KeyCode::Char('j') | KeyCode::Down if self.scrolls_body() => self.body_scroll_down(1),
            KeyCode::Char('k') | KeyCode::Up if self.scrolls_body() => self.body_scroll_up(1),
            KeyCode::Char('g') | KeyCode::Home if self.scrolls_body() => self.body_scroll_top(),
            KeyCode::Char('G') | KeyCode::End if self.scrolls_body() => self.body_scroll_bottom(),
            KeyCode::PageDown if self.scrolls_body() => {
                let page = self.body_page();
                self.body_scroll_down(page);
            }
            KeyCode::PageUp if self.scrolls_body() => {
                let page = self.body_page();
                self.body_scroll_up(page);
            }
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.select_last(),
            KeyCode::PageDown => self.select_page_down(),
            KeyCode::PageUp => self.select_page_up(),
            KeyCode::Enter => {
                // On the Nav pane, Enter browses the highlighted section.
                if self.on_nav() {
                    return self.select_nav();
                }
                if self.screen == Screen::Home
                    && let Some((kind, id)) = self.home_target()
                {
                    // Don't open a detail against the old client mid-switch.
                    if self.fence_during_switch() {
                        return Vec::new();
                    }
                    self.set_status("loading…", Severity::Info);
                    return vec![AppCommand::LoadDetail {
                        kind,
                        id,
                        req: 0,
                        force: false,
                    }];
                }
                // On the tree, Enter opens the selected prefix's detail.
                if self.screen == Screen::PrefixTree
                    && let Some(id) = self.tree_selected_node().map(|n| n.id)
                {
                    if self.fence_during_switch() {
                        return Vec::new();
                    }
                    self.set_status("loading…", Severity::Info);
                    return vec![AppCommand::LoadDetail {
                        kind: ObjectKind::Prefix,
                        id,
                        req: 0,
                        force: false,
                    }];
                }
                // In an interactive detail section, Enter opens the selected row —
                // pushing the current object onto the back-stack so `b`/`Esc`
                // returns, the same jump the `R` related-objects modal performs.
                if self.screen == Screen::Detail
                    && let Some((kind, id)) = self.detail_row_target()
                {
                    if self.fence_during_switch() {
                        return Vec::new();
                    }
                    if let Some(d) = &self.detail {
                        self.detail_nav.push((d.kind, d.id));
                    }
                    self.set_status("loading…", Severity::Info);
                    return vec![AppCommand::LoadDetail {
                        kind,
                        id,
                        req: 0,
                        force: false,
                    }];
                }
            }
            // Related-objects jump list (cross-object navigation) — detail only.
            KeyCode::Char('R') if self.screen == Screen::Detail => self.open_related_modal(),
            KeyCode::Char('o') => {
                if let Some(target) = self.action_target() {
                    return vec![AppCommand::OpenBrowser {
                        url: target.url,
                        command: self.open_browser_command.clone(),
                    }];
                }
            }
            KeyCode::Char('y') => {
                if let Some(target) = self.action_target() {
                    return vec![AppCommand::Copy(target.label)];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Open the Config modal on the Profiles section (the `S` key / palette
    /// `config`). A no-op while one is already open.
    /// Open the overview dashboard and kick off its load. A no-op if already on it
    /// (use `r` to refresh); fenced during a profile switch.
    fn open_dashboard(&mut self) -> Vec<AppCommand> {
        if self.screen == Screen::Dashboard || self.fence_during_switch() {
            return Vec::new();
        }
        self.navigate_to(Screen::Dashboard);
        self.dashboard_error = None;
        self.set_status("loading dashboard…", Severity::Info);
        vec![AppCommand::LoadDashboard { req: 0 }]
    }

    /// Open the prefix tree and kick off its load. A no-op if already on it (use
    /// `r` to refresh); fenced during a profile switch.
    fn open_prefix_tree(&mut self) -> Vec<AppCommand> {
        if self.screen == Screen::PrefixTree || self.fence_during_switch() {
            return Vec::new();
        }
        self.navigate_to(Screen::PrefixTree);
        self.prefix_tree_error = None;
        self.set_status("loading prefixes…", Severity::Info);
        vec![AppCommand::LoadPrefixTree { req: 0 }]
    }

    /// The prefix-tree nodes that are currently visible (not under a collapsed
    /// ancestor), as indices into the loaded `nodes`. Empty when nothing's loaded.
    fn tree_visible(&self) -> Vec<usize> {
        self.prefix_tree.as_ref().map_or_else(Vec::new, |t| {
            prefix_tree::visible_indices(&t.nodes, &self.prefix_tree_collapsed)
        })
    }

    /// The node the tree cursor is on, resolved through the visible-row mapping.
    fn tree_selected_node(&self) -> Option<&PrefixNode> {
        let tree = self.prefix_tree.as_ref()?;
        let visible = self.tree_visible();
        let node_idx = *visible.get(self.prefix_tree_selected)?;
        tree.nodes.get(node_idx)
    }

    /// Pin the cursor inside the visible-row range (it shrinks when a subtree
    /// collapses, or when a reload returns fewer rows).
    fn clamp_tree_selection(&mut self) {
        let len = self.tree_visible().len();
        if len == 0 {
            self.prefix_tree_selected = 0;
        } else if self.prefix_tree_selected >= len {
            self.prefix_tree_selected = len - 1;
        }
    }

    /// Move the cursor by `delta` visible rows (negative = up), saturating at both
    /// ends. Done in `usize` space so there are no wrapping casts.
    fn tree_move(&mut self, delta: isize) {
        let len = self.tree_visible().len();
        if len == 0 {
            self.prefix_tree_selected = 0;
            return;
        }
        let max = len - 1;
        let cur = self.prefix_tree_selected.min(max);
        self.prefix_tree_selected = if delta >= 0 {
            cur.saturating_add(usize::try_from(delta).unwrap_or(0))
                .min(max)
        } else {
            cur.saturating_sub(delta.unsigned_abs())
        };
    }

    /// Jump the cursor to the last visible row.
    fn tree_select_last(&mut self) {
        let len = self.tree_visible().len();
        self.prefix_tree_selected = len.saturating_sub(1);
    }

    /// One PgUp/PgDn step in rows: the live viewport, or a small fallback before
    /// the first render has measured it.
    fn tree_page(&self) -> isize {
        let fallback = isize::try_from(PAGE_JUMP_FALLBACK).unwrap_or(10);
        let h = self.prefix_tree_viewport;
        if h == 0 {
            fallback
        } else {
            isize::try_from(h.max(1)).unwrap_or(fallback)
        }
    }

    /// Stash the tree viewport height (visible rows) at render, for paging.
    pub fn sync_tree_viewport(&mut self, rows: u16) {
        self.prefix_tree_viewport = rows;
    }

    /// Toggle the selected prefix's collapsed state (no-op on a childless leaf),
    /// then re-clamp the cursor since the visible set just changed.
    fn tree_toggle(&mut self) {
        let Some(node) = self.tree_selected_node() else {
            return;
        };
        if !node.collapsible() {
            return;
        }
        let id = node.id;
        if !self.prefix_tree_collapsed.remove(&id) {
            self.prefix_tree_collapsed.insert(id);
        }
        self.clamp_tree_selection();
    }

    /// Explicitly collapse (`true`) or expand (`false`) the selected prefix's
    /// subtree (no-op on a leaf), then re-clamp the cursor.
    fn tree_set_collapsed(&mut self, collapsed: bool) {
        let Some(node) = self.tree_selected_node() else {
            return;
        };
        if !node.collapsible() {
            return;
        }
        let id = node.id;
        if collapsed {
            self.prefix_tree_collapsed.insert(id);
        } else {
            self.prefix_tree_collapsed.remove(&id);
        }
        self.clamp_tree_selection();
    }

    /// Open the `f` filter modal, seeded from the active filters. A no-op while
    /// another modal is open.
    fn open_filter_modal(&mut self) {
        if self.modal.is_none() {
            self.modal = Some(Modal::Filter(Box::new(FilterModal::new(&self.filters))));
        }
    }

    /// Open the `R` related-objects modal for the current detail. A no-op while
    /// another modal is open; a gentle status when the object has no navigable
    /// relations (so `R` never opens an empty list).
    fn open_related_modal(&mut self) {
        if self.modal.is_some() {
            return;
        }
        let Some(detail) = &self.detail else { return };
        if detail.links.is_empty() {
            self.set_transient_status("no related objects", Severity::Info);
            return;
        }
        let links = detail.links.clone();
        self.modal = Some(Modal::Related(Box::new(RelatedModal::new(links))));
    }

    fn open_config_modal(&mut self) {
        if self.modal.is_none() {
            // Seed the Settings "Connection" category from the active profile's
            // live knobs (absent `exclude_config_context` ⇒ true, the client's
            // default), so the form shows the current connection as it stands.
            let connection =
                self.profiles
                    .get(self.profile_index)
                    .map_or_else(ConnectionSeed::default, |p| ConnectionSeed {
                        page_size: p.config.page_size,
                        timeout_secs: p.config.timeout_secs,
                        exclude_config_context: p.config.exclude_config_context.unwrap_or(true),
                        api_vrf: p.config.api_preference(crate::config::ApiSurface::Vrf),
                        api_route_target: p
                            .config
                            .api_preference(crate::config::ApiSurface::RouteTarget),
                    });
            self.modal = Some(Modal::Config(Box::new(ConfigModal::new(
                self.theme.name(),
                self.refresh_secs,
                &self.open_browser_command,
                self.log_level.as_deref().unwrap_or(""),
                self.log_file.as_deref().unwrap_or(""),
                self.cache.enabled(),
                self.cache.ttl_secs(),
                connection,
            ))));
        }
    }

    /// Route a key to the open modal. Help is any-key-close (no command); the
    /// Config modal yields a [`ModalOutcome`] this turns into state changes /
    /// commands (test-connect, save, select, edit, delete, close).
    fn handle_modal_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        match self.modal {
            Some(Modal::Help) => {
                // Any key (incl. Esc) closes help; it issues no command.
                self.modal = None;
                Vec::new()
            }
            Some(Modal::Config(_)) => self.handle_config_modal_key(key),
            Some(Modal::Filter(_)) => self.handle_filter_modal_key(key),
            Some(Modal::Related(_)) => self.handle_related_modal_key(key),
            None => Vec::new(),
        }
    }

    /// Drive the related-objects modal: ↑/↓ (j/k) move the selection, Enter jumps
    /// to the selected object (pushing the current one onto the detail back-stack
    /// so `b`/`Esc` returns to it), Esc/q closes.
    fn handle_related_modal_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        let Some(Modal::Related(modal)) = &mut self.modal else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                modal.move_selection(-1);
                Vec::new()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                modal.move_selection(1);
                Vec::new()
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
                Vec::new()
            }
            KeyCode::Enter => {
                let target = modal.selected_link().map(|l| (l.kind, l.id));
                self.modal = None;
                let Some((kind, id)) = target else {
                    return Vec::new();
                };
                // Don't jump against the old client mid-switch.
                if self.fence_during_switch() {
                    return Vec::new();
                }
                // Remember the object we're leaving so `b`/`Esc` walks back to it.
                if let Some(d) = &self.detail {
                    self.detail_nav.push((d.kind, d.id));
                }
                self.set_status("loading…", Severity::Info);
                vec![AppCommand::LoadDetail {
                    kind,
                    id,
                    req: 0,
                    force: false,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Drive the filter modal: feed it the key, then act on its [`FilterOutcome`].
    /// Apply replaces the active filters and re-runs the last query.
    fn handle_filter_modal_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        let Some(Modal::Filter(modal)) = &mut self.modal else {
            return Vec::new();
        };
        match modal.handle_key(key) {
            FilterOutcome::None => Vec::new(),
            FilterOutcome::Close => {
                self.modal = None;
                Vec::new()
            }
            FilterOutcome::Apply(filters) => {
                self.modal = None;
                self.filters = *filters;
                self.after_filter_change()
            }
        }
    }

    /// Drive the Config modal: feed it the key with the live profile list + active
    /// name, then act on its [`ModalOutcome`].
    fn handle_config_modal_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        // Borrow the names (no per-keystroke String clones, M11). `active` is a
        // short owned copy so the `&mut self.modal` borrow below is clean.
        let names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
        let active = self.profile_name.clone();
        let Some(Modal::Config(modal)) = &mut self.modal else {
            return Vec::new();
        };
        // Snapshot whether a test was in flight, so a probe-relevant edit that
        // supersedes it (the form drops back to Idle) can bump the generation id —
        // dropping the now-stale in-flight result on arrival (H4).
        let was_testing = modal
            .form()
            .is_some_and(|f| f.test == crate::tui::config_modal::TestState::Testing);
        let outcome = modal.handle_key(key, &names, &active);
        if was_testing
            && let Some(Modal::Config(modal)) = &self.modal
            && modal
                .form()
                .is_some_and(|f| f.test == crate::tui::config_modal::TestState::Idle)
        {
            // The edit superseded the in-flight probe: advance the guard so its
            // ConnectTested is discarded when it lands.
            self.test_gen = self.test_seq + 1;
            self.test_seq = self.test_gen;
        }
        self.apply_modal_outcome(outcome)
    }

    /// Act on a Config-modal outcome. Performs no blocking I/O itself: file writes
    /// are synchronous-but-local (mirroring the existing `save_ui_theme` on the
    /// render thread); the network (test-connect, the switch reconnect) is
    /// dispatched as a command.
    fn apply_modal_outcome(&mut self, outcome: ModalOutcome) -> Vec<AppCommand> {
        match outcome {
            ModalOutcome::None => Vec::new(),
            ModalOutcome::Close => {
                self.modal = None;
                Vec::new()
            }
            ModalOutcome::TestConnect => self.modal_test_connect(),
            ModalOutcome::Save { use_it } => self.modal_save(use_it),
            ModalOutcome::Select(name) => self.modal_select(&name),
            ModalOutcome::Edit(name) => {
                self.modal_open_edit(&name);
                Vec::new()
            }
            ModalOutcome::Delete(name) => self.modal_delete(&name),
            ModalOutcome::ChangeTheme(name) => {
                // Hot-apply live through the same path as the `t` cycle / palette
                // `:theme` — including the NO_COLOR guard — so the Settings theme
                // change is previewed immediately and can't re-enable color under
                // NO_COLOR. Persistence happens on save.
                self.set_theme_by_name(&name);
                Vec::new()
            }
            ModalOutcome::SaveSettings => self.modal_save_settings(),
        }
    }

    /// Persist the Settings form and hot-apply each setting: write the changed
    /// `[ui]` fields (theme/refresh_secs/open_browser_command) format-preserving,
    /// re-arm the auto-refresh ticker at the new interval, and adopt the live
    /// browser command. Theme is only persisted when not in NO_COLOR mode (so a
    /// NO_COLOR session never writes a colored theme back). Returns an
    /// [`AppCommand::ArmRefresh`] when the interval changed, else nothing.
    fn modal_save_settings(&mut self) -> Vec<AppCommand> {
        // Snapshot the form values (ends the borrow of `self.modal`).
        let Some(Modal::Config(modal)) = &self.modal else {
            return Vec::new();
        };
        let theme_name = modal.settings.theme_name().to_string();
        let new_refresh = modal.settings.refresh_secs();
        let new_browser = modal.settings.browser_command();
        let new_log_level = modal.settings.log_level_value();
        let new_log_file = modal.settings.log_file_value();
        let new_cache_enabled = modal.settings.cache_enabled();
        let new_cache_ttl = modal.settings.cache_ttl_secs();
        // Active-profile connection knobs (a change reconnects; see below).
        let new_page_size = modal.settings.page_size();
        let new_timeout = modal.settings.timeout_secs();
        let new_exclude = modal.settings.exclude_config_context();
        let new_api_vrf = modal.settings.api_vrf();
        let new_api_route_target = modal.settings.api_route_target();

        let refresh_changed = new_refresh != self.refresh_secs;

        // Persist the changed fields in ONE format-preserving write (M8), so a
        // failure can't leave the file with theme updated but the rest stale. The
        // token is never involved here. Theme is skipped under NO_COLOR so we never
        // write a colored theme into the user's config (exit-time persist guard).
        // log_level/log_file span the top-level keys, so this uses the combined
        // `SettingField` writer (still one atomic, comment-preserving write).
        if let Some(path) = self.config_path.clone() {
            use crate::config::{SettingField, UiField};
            let mut fields = Vec::with_capacity(7);
            if !self.theme.is_no_color() {
                fields.push(SettingField::Ui(UiField::Theme(theme_name.clone())));
            }
            fields.push(SettingField::Ui(UiField::RefreshSecs(new_refresh)));
            fields.push(SettingField::Ui(UiField::OpenBrowserCommand(
                new_browser.clone(),
            )));
            fields.push(SettingField::LogLevel(new_log_level.clone()));
            fields.push(SettingField::LogFile(new_log_file.clone()));
            fields.push(SettingField::CacheEnabled(new_cache_enabled));
            fields.push(SettingField::CacheTtl(new_cache_ttl));
            if let Err(e) = crate::config::save_setting_fields(&path, &fields) {
                self.set_status(format!("save failed: {e:#}"), Severity::Error);
                return Vec::new();
            }
        }

        // Adopt the live values: the browser command applies to the next `o`; the
        // refresh interval re-arms the ticker. Theme already hot-applied on cycle.
        // log_level/log_file persist now but apply on the next launch (tracing
        // inits at startup); keep the live copies in sync so reopening shows them.
        self.open_browser_command = new_browser;
        self.refresh_secs = new_refresh;
        self.log_level = new_log_level;
        self.log_file = new_log_file;
        // Hot-apply the cache policy: rebuild it over the same store + partition so
        // the new on/off + TTL take effect immediately (warm entries are kept).
        let cache_cfg = crate::cache::CacheConfig::from_settings(&crate::config::CacheSettings {
            enabled: new_cache_enabled,
            ttl_secs: new_cache_ttl,
        });
        self.cache = self.cache.with_config(cache_cfg);

        self.modal = None;
        let mut commands = if refresh_changed {
            vec![AppCommand::ArmRefresh(new_refresh)]
        } else {
            Vec::new()
        };

        // Connection knobs live on the active profile (not `[ui]`), and the client
        // bakes timeout/page_size/exclude at construction — so a change is persisted
        // to that profile and hot-applied by reconnecting through the existing
        // switch path. Unchanged ⇒ no reconnect, just the plain "saved" confirmation.
        match self.apply_connection_settings(
            new_page_size,
            new_timeout,
            new_exclude,
            new_api_vrf,
            new_api_route_target,
        ) {
            Some(reconnect) => commands.extend(reconnect),
            None => self.set_status("settings saved", Severity::Success),
        }
        commands
    }

    /// Persist a change to the active profile's connection knobs (`page_size`,
    /// `timeout_secs`, `exclude_config_context`, and the `[api]` `vrf`/`route_target`
    /// backends) and reconnect so it takes effect live. Returns `None` when nothing
    /// changed (the caller shows the plain "saved" status); `Some(commands)` when a
    /// change was attempted — the reconnect commands, or empty if the persist failed
    /// (this method sets the error status). The profile's identity/auth fields are
    /// carried through unchanged.
    fn apply_connection_settings(
        &mut self,
        page_size: Option<usize>,
        timeout_secs: Option<u64>,
        exclude_config_context: bool,
        api_vrf: BackendPreference,
        api_route_target: BackendPreference,
    ) -> Option<Vec<AppCommand>> {
        use crate::config::ApiSurface;
        let idx = self.profile_index;
        let entry = self.profiles.get(idx)?;
        let cfg = &entry.config;
        let changed = cfg.page_size != page_size
            || cfg.timeout_secs != timeout_secs
            || cfg.exclude_config_context.unwrap_or(true) != exclude_config_context
            || cfg.api_preference(ApiSurface::Vrf) != api_vrf
            || cfg.api_preference(ApiSurface::RouteTarget) != api_route_target;
        if !changed {
            return None;
        }

        // Snapshot the unchanged identity/auth fields for the format-preserving
        // write (no rename: original == name == the active profile).
        let name = entry.name.clone();
        let url = cfg.url.clone();
        let token_env = cfg.token_env.clone();
        let auth_scheme = cfg.auth_scheme.unwrap_or(AuthScheme::Auto);
        let verify_tls = cfg.verify_tls.unwrap_or(true);

        let Some(path) = self.config_path.clone() else {
            self.set_status(
                "can't save connection settings: no config file path".to_string(),
                Severity::Error,
            );
            return Some(Vec::new());
        };
        if let Err(e) = Self::persist_profile(
            &path,
            Some(&name),
            &ProfileFormData {
                name: &name,
                url: &url,
                token_env: token_env.as_deref(),
                auth_scheme,
                verify_tls,
                timeout_secs,
                page_size,
                exclude_config_context,
                api_vrf,
                api_route_target,
                config_token: cfg.token.as_ref().map(ConfigToken::expose),
            },
        ) {
            self.set_status(format!("save failed: {e:#}"), Severity::Error);
            return Some(Vec::new());
        }

        // Reflect into the live profile entry so the reconnect rebuilds the client
        // with the new knobs, then ride the existing switch path.
        if let Some(entry) = self.profiles.get_mut(idx) {
            entry.config.timeout_secs = timeout_secs;
            entry.config.page_size = page_size;
            entry.config.exclude_config_context = Some(exclude_config_context);
            entry.config.api = build_api_config(api_vrf, api_route_target);
        }
        Some(self.switch_to_index(idx))
    }

    /// Build a [`ConnectRequest`] from the open form: the form's url/auth/tls plus
    /// the token to probe with. The probe token uses the SAME precedence as save /
    /// launch (M15) so the test reflects what a real connection would use:
    ///   typed token → form `token_env` → `NBOX_TOKEN` → config token.
    /// Returns `None` if no form is open.
    fn form_connect_request(&self) -> Option<ConnectRequest> {
        let Some(Modal::Config(modal)) = &self.modal else {
            return None;
        };
        let form = modal.form()?;
        let editing_entry = form
            .editing
            .as_deref()
            .and_then(|name| self.profiles.iter().find(|p| p.name == name));
        // Resolve the probe token through the shared helper so a test-connect uses
        // the SAME normalized precedence as a real launch/reconnect (M15): typed →
        // form `token_env` → `NBOX_TOKEN` → the stored config token, each with a
        // pasted `Bearer `/`Token ` prefix stripped.
        let typed = form.token();
        let token_env = form.token_env();
        let config_token = editing_entry
            .and_then(|entry| entry.config.token.as_ref())
            .map(|token| token.expose().to_string());
        let token = crate::config::resolve_probe_token(
            typed.as_deref(),
            token_env.as_deref(),
            config_token.as_deref(),
        );
        Some(ConnectRequest {
            url: form.url(),
            auth_scheme: form.auth_scheme,
            verify_tls: form.verify_tls,
            token,
        })
    }

    /// Dispatch a test-connect for the open form (id-guarded, spinner-tracked).
    fn modal_test_connect(&mut self) -> Vec<AppCommand> {
        let Some(req) = self.form_connect_request() else {
            return Vec::new();
        };
        self.test_seq += 1;
        self.test_gen = self.test_seq;
        vec![AppCommand::TestConnect {
            id: self.test_seq,
            req,
        }]
    }

    /// Persist the open form's profile (metadata + optional config token),
    /// add/update it in the live `profiles`, and — when `use_it` — set it active
    /// and ride the switch path to reconnect. Returns the switch command when
    /// switching, else nothing.
    fn modal_save(&mut self, use_it: bool) -> Vec<AppCommand> {
        // Snapshot the form fields (the borrow of `self.modal` must end before we
        // touch `self.profiles`/config).
        let Some(Modal::Config(modal)) = &self.modal else {
            return Vec::new();
        };
        let Some(form) = modal.form() else {
            return Vec::new();
        };
        let name = form.name();
        let url = form.url();
        let typed_token = form.token();
        let token_env = form.token_env();
        let auth_scheme = form.auth_scheme;
        let verify_tls = form.verify_tls;
        let timeout_secs = form.timeout_secs();
        let page_size = form.page_size();
        let exclude_config_context = form.exclude_config_context;
        let api_vrf = form.api_vrf;
        let api_route_target = form.api_route_target;
        let clear_token = form.clear_token;
        let original = form.editing.clone();
        let existing_config_token = original.as_deref().and_then(|original| {
            self.profiles
                .iter()
                .find(|p| p.name == original)
                .and_then(|entry| entry.config.token.as_ref())
                .map(|token| token.expose().to_string())
        });
        // The token lives in config.toml. A typed token replaces it; an explicit
        // Ctrl+X clears it; otherwise the existing stored token is kept (the field
        // starts blank on edit — the secret is never read back into the UI).
        let cleared_existing = clear_token && existing_config_token.is_some();
        let config_token = if clear_token {
            None
        } else {
            typed_token.or(existing_config_token)
        };

        // M7: a save with no backing config path can't persist anything; surface an
        // error rather than silently "succeeding" with an in-memory-only edit.
        let Some(path) = self.config_path.clone() else {
            self.set_status(
                "can't save: no config file path (pass --config or run `nbox config init`)"
                    .to_string(),
                Severity::Error,
            );
            return Vec::new();
        };

        // Write the metadata + optional config token to the config file
        // (format-preserving). On a rename the old TOML section is removed (H1).
        if let Err(e) = Self::persist_profile(
            &path,
            original.as_deref(),
            &ProfileFormData {
                name: &name,
                url: &url,
                token_env: token_env.as_deref(),
                auth_scheme,
                verify_tls,
                timeout_secs,
                page_size,
                exclude_config_context,
                api_vrf,
                api_route_target,
                config_token: config_token.as_deref(),
            },
        ) {
            self.set_status(format!("save failed: {e:#}"), Severity::Error);
            return Vec::new();
        }

        // Build the live profile entry from the form + reflect it into `profiles`.
        // The form now owns timeout/page_size/exclude/api too, so these come
        // straight off it (mirroring what persist_profile wrote to disk): an empty
        // numeric field is `None` (default), and a REST backend leaves `api` clean.
        let api = build_api_config(api_vrf, api_route_target);
        let config = ProfileConfig {
            url: url.clone(),
            token_env: token_env.clone(),
            auth_scheme: Some(auth_scheme),
            verify_tls: Some(verify_tls),
            timeout_secs,
            page_size,
            exclude_config_context: Some(exclude_config_context),
            api,
            token: config_token.map(ConfigToken::new),
            ..Default::default()
        };
        self.upsert_live_profile(original.as_deref(), &name, config);

        // Return to the list, selecting the saved profile.
        let idx = self
            .profiles
            .iter()
            .position(|p| p.name == name)
            .unwrap_or(0);
        if let Some(Modal::Config(modal)) = &mut self.modal {
            modal.show_list(idx);
        }

        if use_it {
            // Persist active + ride the existing switch path to reconnect. The
            // switch id-guard composes with any in-flight prior switch (a stale one
            // is dropped on arrival), and an explicit select persists active.
            if let Err(e) = crate::config::save_active_profile(&path, &name) {
                self.set_status(format!("set-active failed: {e:#}"), Severity::Error);
            }
            self.modal = None;
            return self.switch_to_index(idx);
        }
        if cleared_existing {
            self.set_status(
                format!("saved profile '{name}' — stored token cleared"),
                Severity::Info,
            );
        } else {
            self.set_status(format!("saved profile '{name}'"), Severity::Success);
        }
        Vec::new()
    }

    /// Persist a profile's editor metadata to the config file (format-preserving).
    /// The config token is written here (or cleared when `config_token` is `None`).
    ///
    /// `original` is the pre-edit name when editing (`None` on add). On a rename
    /// (`original` differs from `name`) the old `[profiles.<original>]` section is
    /// removed (H1) so a phantom profile can't return on the next launch; if the
    /// renamed profile was the active one, `active_profile` is repointed to the new
    /// name so the file stays self-consistent.
    fn persist_profile(
        path: &std::path::Path,
        original: Option<&str>,
        data: &ProfileFormData<'_>,
    ) -> anyhow::Result<()> {
        use crate::config::ApiSurface;
        let ProfileFormData {
            name,
            url,
            token_env,
            auth_scheme,
            verify_tls,
            timeout_secs,
            page_size,
            exclude_config_context,
            api_vrf,
            api_route_target,
            config_token,
        } = *data;
        let mut doc = crate::config::load_doc_or_new(path)?;
        // Rename: drop the old section and repoint active_profile if it named it.
        if let Some(orig) = original
            && orig != name
        {
            crate::config::remove_profile(&mut doc, orig)?;
            let active_was_orig = doc
                .get("active_profile")
                .and_then(|v| v.as_str())
                .is_some_and(|a| a == orig);
            if active_was_orig {
                crate::config::set_active_profile(&mut doc, name);
            }
        }
        crate::config::upsert_profile(&mut doc, name, url, None)?;
        crate::config::set_profile_token_env(&mut doc, name, token_env)?;
        crate::config::set_profile_auth_scheme(&mut doc, name, Some(auth_scheme))?;
        crate::config::set_profile_verify_tls(&mut doc, name, Some(verify_tls))?;
        // Empty numeric fields clear the key (built-in default); a positive value
        // writes it. `exclude_config_context` is written explicitly (like
        // verify_tls), so the form value is authoritative. REST backends drop the
        // `[api]` key (and an empty `[api]` table) to keep REST profiles clean.
        crate::config::set_profile_timeout_secs(&mut doc, name, timeout_secs)?;
        crate::config::set_profile_page_size(&mut doc, name, page_size)?;
        crate::config::set_profile_exclude_config_context(
            &mut doc,
            name,
            Some(exclude_config_context),
        )?;
        crate::config::set_profile_api_backend(&mut doc, name, ApiSurface::Vrf, api_vrf)?;
        crate::config::set_profile_api_backend(
            &mut doc,
            name,
            ApiSurface::RouteTarget,
            api_route_target,
        )?;
        crate::config::set_profile_token(&mut doc, name, config_token)?;
        // First profile in a fresh file becomes active.
        if doc.get("active_profile").is_none() {
            crate::config::set_active_profile(&mut doc, name);
        }
        crate::config::write_doc(path, &doc)?;
        Ok(())
    }

    /// Insert or replace a profile in the live `profiles` list. On a rename
    /// (editing with a changed name), the old entry is removed. Keeps
    /// `profile_index` pointed at the active profile by name.
    fn upsert_live_profile(&mut self, original: Option<&str>, name: &str, config: ProfileConfig) {
        let renamed_active = original.is_some_and(|orig| orig != name && orig == self.profile_name);
        // Remove the pre-edit entry on a rename.
        if let Some(orig) = original
            && orig != name
        {
            self.profiles.retain(|p| p.name != orig);
        }
        match self.profiles.iter_mut().find(|p| p.name == name) {
            Some(entry) => entry.config = config,
            None => self.profiles.push(ProfileEntry {
                name: name.to_string(),
                config,
            }),
        }
        if renamed_active {
            self.profile_name = name.to_string();
        }
        // Re-anchor the active index by name (the list may have grown/shrunk).
        if let Some(idx) = self
            .profiles
            .iter()
            .position(|p| p.name == self.profile_name)
        {
            self.profile_index = idx;
        }
    }

    /// Open the prefilled edit form for `name` (the modal owns rendering; the app
    /// fills it from the live `ProfileConfig`).
    fn modal_open_edit(&mut self, name: &str) {
        let Some(entry) = self.profiles.iter().find(|p| p.name == name) else {
            return;
        };
        let url = entry.config.url.clone();
        let token_env = entry.config.token_env.clone();
        let auth_scheme = entry.config.auth_scheme.unwrap_or(AuthScheme::Auto);
        let verify_tls = entry.config.verify_tls.unwrap_or(true);
        let timeout_secs = entry.config.timeout_secs;
        let page_size = entry.config.page_size;
        // Absent key ⇒ exclude (matches NetBox client's `unwrap_or(true)` default).
        let exclude_config_context = entry.config.exclude_config_context.unwrap_or(true);
        let api_vrf = entry.config.api_preference(crate::config::ApiSurface::Vrf);
        let api_route_target = entry
            .config
            .api_preference(crate::config::ApiSurface::RouteTarget);
        let config_token = entry.config.token.as_ref().map(ConfigToken::expose);
        if let Some(Modal::Config(modal)) = &mut self.modal {
            modal.open_edit_form(ProfileFormData {
                name,
                url: &url,
                token_env: token_env.as_deref(),
                auth_scheme,
                verify_tls,
                timeout_secs,
                page_size,
                exclude_config_context,
                api_vrf,
                api_route_target,
                config_token,
            });
        }
    }

    /// Select (switch to) `name` from the modal: persist it as active, close the
    /// modal, and ride the switch path to reconnect.
    fn modal_select(&mut self, name: &str) -> Vec<AppCommand> {
        let Some(idx) = self.profiles.iter().position(|p| p.name == name) else {
            self.set_status(format!("no profile named '{name}'"), Severity::Error);
            return Vec::new();
        };
        if let Some(path) = self.config_path.clone()
            && let Err(e) = crate::config::save_active_profile(&path, name)
        {
            self.set_status(format!("set-active failed: {e:#}"), Severity::Error);
        }
        self.modal = None;
        self.switch_to_index(idx)
    }

    /// Delete `name` from the modal: drop it from the config file and the live
    /// `profiles`. The active/last guards already ran in the modal.
    fn modal_delete(&mut self, name: &str) -> Vec<AppCommand> {
        if let Some(path) = self.config_path.clone()
            && let Err(e) = Self::remove_profile_file(&path, name)
        {
            self.set_status(format!("delete failed: {e:#}"), Severity::Error);
            return Vec::new();
        }
        self.profiles.retain(|p| p.name != name);
        // Re-anchor the active index + the modal's list selection.
        if let Some(idx) = self
            .profiles
            .iter()
            .position(|p| p.name == self.profile_name)
        {
            self.profile_index = idx;
        }
        if let Some(Modal::Config(modal)) = &mut self.modal {
            let sel = match &modal.profiles.mode {
                ProfilesMode::List { selected } => {
                    (*selected).min(self.profiles.len().saturating_sub(1))
                }
                _ => 0,
            };
            modal.show_list(sel);
        }
        self.set_status(format!("deleted profile '{name}'"), Severity::Success);
        Vec::new()
    }

    /// Remove a profile from the config file (format-preserving).
    fn remove_profile_file(path: &std::path::Path, name: &str) -> anyhow::Result<()> {
        let mut doc = crate::config::load_doc_or_new(path)?;
        crate::config::remove_profile(&mut doc, name)?;
        crate::config::write_doc(path, &doc)?;
        Ok(())
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        match key.code {
            // Esc cancels the search; Enter submits it. Everything else is text
            // editing, delegated to the cheese-backed input (chars, backspace,
            // delete, cursor movement, Ctrl+U clear, Ctrl+W word-delete).
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let query = self.search_input.value().trim().to_string();
                self.mode = Mode::Normal;
                if !query.is_empty() {
                    // Don't search the old client mid-switch.
                    if self.fence_during_switch() {
                        return Vec::new();
                    }
                    self.last_query = Some(query.clone());
                    // Submitting a search moves focus to the results so the user can
                    // navigate them straight away (the home now opens on the Nav
                    // rail, so this can't rely on List being the default focus).
                    self.focus = Focus::List;
                    self.set_status(format!("searching {query}…"), Severity::Info);
                    return vec![self.search_cmd(query)];
                }
            }
            _ => {
                // An edit refilters the live view; a non-editing key is ignored.
                if self.search_input.handle_key(key) {
                    self.refilter();
                }
            }
        }
        Vec::new()
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Vec<AppCommand> {
        match key.code {
            // Esc cancels the palette; Enter executes it. Everything else is text
            // editing, delegated to the cheese-backed input.
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let input = self.command_input.value().trim().to_string();
                self.mode = Mode::Normal;
                if !input.is_empty() {
                    match palette::parse(&input) {
                        Ok(cmd) => return self.apply_palette(cmd),
                        Err(e) => self.set_status(e, Severity::Error),
                    }
                }
            }
            _ => {
                self.command_input.handle_key(key);
            }
        }
        Vec::new()
    }

    /// Map a parsed palette command onto state changes / commands.
    /// Build an [`AppCommand::Search`] for `query` carrying the active filters. The
    /// `req` is a placeholder (`0`) — the dispatcher stamps the real channel id.
    fn search_cmd(&self, query: String) -> AppCommand {
        AppCommand::Search {
            query,
            req: 0,
            filters: self.filters.clone(),
        }
    }

    /// A compact `key=value` summary of the active filters (display order), or an
    /// empty string when none are set. Pure; reused by the status line (and, later,
    /// the chips bar).
    fn filters_summary(&self) -> String {
        let f = &self.filters;
        [
            ("status", &f.status),
            ("site", &f.site),
            ("region", &f.region),
            ("site-group", &f.site_group),
            ("location", &f.location),
            ("tenant", &f.tenant),
            ("role", &f.role),
            ("tag", &f.tag),
            ("vrf", &f.vrf),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join(" ")
    }

    /// Apply one `key=value` filter pair to [`Self::filters`]; an empty value
    /// clears the key. The four scope keys (site/region/site-group/location) are
    /// mutually exclusive, so setting one clears the others (the resolver enforces
    /// this too — this just keeps the active set coherent).
    fn apply_filter_pair(&mut self, key: &str, value: &str) {
        let v = {
            let t = value.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        let setting = v.is_some();
        let f = &mut self.filters;
        match key.to_ascii_lowercase().as_str() {
            "status" => f.status = v,
            "tenant" => f.tenant = v,
            "role" => f.role = v,
            "tag" => f.tag = v,
            "vrf" => f.vrf = v,
            "site" => {
                f.site = v;
                if setting {
                    f.region = None;
                    f.site_group = None;
                    f.location = None;
                }
            }
            "region" => {
                f.region = v;
                if setting {
                    f.site = None;
                    f.site_group = None;
                    f.location = None;
                }
            }
            "site-group" | "site_group" => {
                f.site_group = v;
                if setting {
                    f.site = None;
                    f.region = None;
                    f.location = None;
                }
            }
            "location" => {
                f.location = v;
                if setting {
                    f.site = None;
                    f.region = None;
                    f.site_group = None;
                }
            }
            _ => {}
        }
    }

    /// Set one or more filter pairs, then re-run the last query so results reflect
    /// the new filters (or stage them with a status when there's no query yet).
    fn set_filters(&mut self, pairs: Vec<(String, String)>) -> Vec<AppCommand> {
        if self.fence_during_switch() {
            return Vec::new();
        }
        for (k, v) in &pairs {
            self.apply_filter_pair(k, v);
        }
        self.after_filter_change()
    }

    /// Clear all active filters, then re-run the last query.
    fn clear_filters(&mut self) -> Vec<AppCommand> {
        if self.fence_during_switch() {
            return Vec::new();
        }
        self.filters = SearchFilters::default();
        self.after_filter_change()
    }

    /// Shared tail of a filter change: a status, plus a re-run of the last query so
    /// the change takes effect (filters apply to the next search if there is none).
    fn after_filter_change(&mut self) -> Vec<AppCommand> {
        let summary = self.filters_summary();
        match self.last_query.clone() {
            Some(query) => {
                let msg = if summary.is_empty() {
                    format!("filters cleared; refreshing '{query}'…")
                } else {
                    format!("filters [{summary}]; refreshing '{query}'…")
                };
                self.set_status(msg, Severity::Info);
                vec![self.search_cmd(query)]
            }
            None => {
                let msg = if summary.is_empty() {
                    "filters cleared".to_string()
                } else {
                    format!("filters [{summary}] — run a search to apply")
                };
                self.set_status(msg, Severity::Info);
                Vec::new()
            }
        }
    }

    fn apply_palette(&mut self, cmd: PaletteCommand) -> Vec<AppCommand> {
        // Fence palette fetches mid-switch so they don't hit the old client; the
        // theme/open/copy/profile verbs below are unaffected (no fetch, or the
        // switch itself).
        match cmd {
            PaletteCommand::Lookup { kind, value } => {
                if self.fence_during_switch() {
                    return Vec::new();
                }
                self.set_status(format!("loading {value}…"), Severity::Info);
                vec![AppCommand::LoadByRef {
                    kind,
                    value,
                    req: 0,
                    force: false,
                }]
            }
            PaletteCommand::Search(query) => {
                if self.fence_during_switch() {
                    return Vec::new();
                }
                self.last_query = Some(query.clone());
                self.set_status(format!("searching {query}…"), Severity::Info);
                vec![self.search_cmd(query)]
            }
            PaletteCommand::Open => match self.selected_result() {
                Some(r) => vec![AppCommand::OpenBrowser {
                    url: r.url.clone(),
                    command: self.open_browser_command.clone(),
                }],
                None => {
                    self.set_status("no selection", Severity::Warning);
                    Vec::new()
                }
            },
            PaletteCommand::Copy => match self.selected_result() {
                Some(r) => vec![AppCommand::Copy(r.display.clone())],
                None => {
                    self.set_status("no selection", Severity::Warning);
                    Vec::new()
                }
            },
            PaletteCommand::Theme(name) => {
                self.set_theme_by_name(&name);
                Vec::new()
            }
            PaletteCommand::Profile(name) => self.select_profile(&name),
            PaletteCommand::Config => {
                self.open_config_modal();
                Vec::new()
            }
            PaletteCommand::Refresh => {
                if self.fence_during_switch() {
                    return Vec::new();
                }
                match self.last_query.clone() {
                    Some(query) => {
                        self.set_status(format!("refreshing {query}…"), Severity::Info);
                        vec![self.search_cmd(query)]
                    }
                    None => {
                        self.set_status("nothing to refresh", Severity::Warning);
                        Vec::new()
                    }
                }
            }
            PaletteCommand::Filter(pairs) => self.set_filters(pairs),
            PaletteCommand::ClearFilters => self.clear_filters(),
            PaletteCommand::ClearSearch => {
                self.clear_search();
                Vec::new()
            }
        }
    }

    /// Push the current screen onto the history stack and switch to `screen`.
    fn navigate_to(&mut self, screen: Screen) {
        if self.screen != screen {
            self.history.push(self.screen);
            self.screen = screen;
        }
    }

    /// Go back one step. On a detail screen with a cross-object drill path, this
    /// walks back to the previous *object* (reloading it); otherwise it pops back
    /// to the previous *screen* (or Home). Returns the reload command, if any.
    fn go_back(&mut self) -> Vec<AppCommand> {
        if self.screen == Screen::Detail {
            self.save_detail_view_state();
            // Walk the object-level back-stack first (a related-link drill path),
            // staying on the detail screen and reloading the previous object.
            if let Some((kind, id)) = self.detail_nav.pop() {
                self.set_status("loading…", Severity::Info);
                return vec![AppCommand::LoadDetail {
                    kind,
                    id,
                    req: 0,
                    force: false,
                }];
            }
        }
        self.screen = self.history.pop().unwrap_or(Screen::Home);
        if self.screen == Screen::Home {
            // Back to the list/preview split: the highlighted row may differ from
            // whatever the preview last held, so reconcile it on the next tick —
            // otherwise the preview sits blank until a `j`/`k` nudge dirties it
            // (the "ghost loading" where you go down-then-up to load the top row).
            self.mark_preview_dirty();
        }
        Vec::new()
    }

    /// Snapshot the loaded detail's tab + scroll under its `(kind, id)`, so
    /// re-opening or refreshing the same object restores them.
    fn save_detail_view_state(&mut self) {
        if let Some(d) = &self.detail {
            self.detail_view_state
                .insert((d.kind, d.id), (self.detail_tab, self.detail_scroll));
        }
    }

    /// Whether a search is currently showing — results on screen or a remembered
    /// query. Drives whether `Esc`/`b` on Home clears the search vs. navigates back.
    fn search_active(&self) -> bool {
        !self.results.is_empty() || self.last_query.is_some()
    }

    /// Clear the active search: drop the results + query, suppress any in-flight
    /// search (bump the high-water mark so a late `SearchComplete` lands stale), and
    /// reset the `/` input — returning Home to the recents list. The counterpart to
    /// `F` (clear filters); recents and the active filters are left intact.
    fn clear_search(&mut self) {
        self.request_seq += 1;
        self.search_gen = self.request_seq;
        self.browse_kind = None;
        self.results.clear();
        self.view.clear();
        self.selected = 0;
        self.preview = None;
        self.preview_for = None;
        self.preview_dirty = false;
        self.preview_scroll = 0;
        self.last_query = None;
        self.pending_reselect = None;
        self.search_input.reset();
        // A confirmation, not state — fade it so it doesn't sit on the footer.
        self.set_transient_status("search cleared", Severity::Info);
    }

    /// Set the transient status message together with its severity, so the
    /// footer colors it via [`Theme::message_style`]. The one place message text
    /// and its color classification are kept in lockstep.
    fn set_status(&mut self, message: impl Into<String>, severity: Severity) {
        self.status = message.into();
        self.status_severity = severity;
        self.status_ttl = None;
    }

    /// Set a status message that clears itself after a short run of UI ticks.
    fn set_transient_status(&mut self, message: impl Into<String>, severity: Severity) {
        self.status = message.into();
        self.status_severity = severity;
        self.status_ttl = Some(TRANSIENT_STATUS_TICKS);
    }

    /// Clear the status line back to its neutral resting state.
    fn clear_status(&mut self) {
        self.status.clear();
        self.status_severity = Severity::Info;
        self.status_ttl = None;
    }

    /// Age a transient status by one fast UI tick, clearing it when its TTL ends.
    fn tick_status_ttl(&mut self) {
        let Some(remaining) = self.status_ttl else {
            return;
        };
        if remaining <= 1 {
            self.clear_status();
        } else {
            self.status_ttl = Some(remaining - 1);
        }
    }

    /// Flip focus between the home split's list and preview panes. Switching to
    /// the preview re-clamps its scroll in case the loaded body changed since.
    /// Cycle focus across the three home panes (Nav → List → Preview, wrapping).
    /// `forward` follows the left→right order; `Shift+Tab` reverses it.
    fn cycle_focus(&mut self, forward: bool) {
        use Focus::{List, Nav, Preview};
        self.focus = if forward {
            match self.focus {
                Nav => List,
                List => Preview,
                Preview => Nav,
            }
        } else {
            match self.focus {
                Nav => Preview,
                List => Nav,
                Preview => List,
            }
        };
    }

    /// True when the home Navigation pane currently owns the keyboard.
    fn on_nav(&self) -> bool {
        self.screen == Screen::Home && self.focus == Focus::Nav
    }

    /// Move the Nav-pane cursor one row, clamped to the section list. A real move
    /// dirties the live-browse so the highlighted kind's list loads on the next
    /// tick (debounced; see [`Self::on_nav_browse_tick`]).
    fn nav_move(&mut self, forward: bool) {
        let last = NAV_SECTIONS.len() - 1;
        let cur = self.nav_selected.min(last);
        let next = if forward {
            (cur + 1).min(last)
        } else {
            cur.saturating_sub(1)
        };
        if next != self.nav_selected {
            self.nav_selected = next;
            self.browse_dirty = true;
        }
    }

    /// Jump the Nav cursor to a section index (g/G), dirtying the live-browse when
    /// it actually moves.
    fn nav_jump(&mut self, index: usize) {
        let clamped = index.min(NAV_SECTIONS.len() - 1);
        if clamped != self.nav_selected {
            self.nav_selected = clamped;
            self.browse_dirty = true;
        }
    }

    /// Act on the highlighted Nav section: browse a kind into Results (moving focus
    /// there), or show the recents list. Returns any command to dispatch.
    fn select_nav(&mut self) -> Vec<AppCommand> {
        let section = NAV_SECTIONS[self.nav_selected.min(NAV_SECTIONS.len() - 1)];
        match section.object_kind() {
            Some(kind) => {
                if self.fence_during_switch() {
                    return Vec::new();
                }
                self.browse_kind = Some(kind);
                // Remember it for the exit-time persist → restored next launch.
                self.last_browsed = Some(kind);
                self.focus = Focus::List;
                self.set_status(format!("browsing {}…", section.label()), Severity::Info);
                vec![AppCommand::Browse { kind, req: 0 }]
            }
            None => {
                // Recent: drop browse/search results so the recents fallback shows.
                self.browse_kind = None;
                self.last_query = None;
                self.results.clear();
                self.view.clear();
                self.selected = 0;
                self.focus = Focus::List;
                Vec::new()
            }
        }
    }

    /// True when the active movement keys should scroll a body rather than move a
    /// selection: the detail screen always, or the home Preview pane when focused.
    fn scrolls_body(&self) -> bool {
        self.screen == Screen::Detail
            || (self.screen == Screen::Home && self.focus == Focus::Preview)
    }

    /// The (kind, id) of the result the preview should reflect — the highlighted
    /// search result or recent. `None` when nothing is selectable. This is the
    /// identity preview loads are tagged with for stale-response suppression.
    pub fn preview_selection(&self) -> Option<(ObjectKind, u64)> {
        self.home_target()
    }

    /// Note that the highlighted result may have changed, so the preview is now
    /// potentially out of date. The actual (de-duplicated, debounced) load is
    /// deferred to the next [`Self::on_preview_tick`] — never fired per keystroke.
    fn mark_preview_dirty(&mut self) {
        self.preview_dirty = true;
    }

    /// Test-only: force the dirty flag so the debounce flush can be exercised
    /// without driving a real selection change.
    #[cfg(test)]
    fn mark_preview_dirty_for_test(&mut self) {
        self.preview_dirty = true;
    }

    /// The debounce flush. Called on the fast always-on `PreviewTick`. When the
    /// selection has settled on something the preview doesn't already hold, issue
    /// exactly one [`AppCommand::LoadPreview`] tagged with that selection's
    /// identity. A burst of cursor movement coalesces into a single fetch here;
    /// no movement (or no change) issues nothing.
    fn on_preview_tick(&mut self) -> Vec<AppCommand> {
        // Only reconcile the preview while it's on screen and we're not mid-typing
        // a search/command (a fuzzy filter marks dirty; let the cursor settle), and
        // never while a profile switch is in flight (the preview would hit the old
        // client; it'll reconcile once the switch settles on the new instance).
        if self.screen != Screen::Home
            || self.mode != Mode::Normal
            || !self.preview_dirty
            || self.switch_in_flight()
        {
            return Vec::new();
        }
        self.preview_dirty = false;
        match self.preview_selection() {
            // Already showing this object → no fetch (idempotent on a still cursor).
            Some(target) if self.preview_for == Some(target) => Vec::new(),
            Some((kind, id)) => vec![AppCommand::LoadPreview { kind, id }],
            None => {
                // Nothing selectable: drop any stale preview so the pane shows
                // its placeholder instead of an orphaned object.
                self.preview = None;
                self.preview_for = None;
                self.preview_scroll = 0;
                Vec::new()
            }
        }
    }

    /// The live-browse debounce flush, run on the always-on `PreviewTick`. When
    /// the Nav-rail cursor has settled on a different kind than the results pane
    /// currently shows, issue exactly one [`AppCommand::Browse`] for it — without
    /// moving focus off the rail, so a reader can keep scrolling the rail and
    /// watch each kind's list (and its first item's preview) populate beside it.
    /// A burst of `j`/`k` coalesces into a single fetch here. The `Recent` section
    /// clears the results so its recents fallback shows.
    fn on_nav_browse_tick(&mut self) -> Vec<AppCommand> {
        // Track the rail cursor every tick so "settled" means it hasn't moved
        // since the previous tick. A continuous scroll keeps moving between ticks,
        // so this defers the fetch until movement stops — no flashing the list of
        // each section the cursor passes through.
        let settled = self.nav_selected == self.nav_tick_anchor;
        self.nav_tick_anchor = self.nav_selected;
        if !self.browse_dirty || !settled {
            return Vec::new();
        }
        self.browse_dirty = false;
        // Only auto-browse from the Nav rail, idle, and never mid profile switch
        // (it would hit the old client; the post-switch state reconciles instead).
        if !self.on_nav() || self.mode != Mode::Normal || self.switch_in_flight() {
            return Vec::new();
        }
        let section = NAV_SECTIONS[self.nav_selected.min(NAV_SECTIONS.len() - 1)];
        match section.object_kind() {
            Some(kind) => {
                // Already showing this kind from a browse (not a search) → no-op,
                // so a still cursor never re-fetches.
                if self.browse_kind == Some(kind) && self.last_query.is_none() {
                    return Vec::new();
                }
                self.browse_kind = Some(kind);
                self.last_query = None;
                // Hovering is browsing: remember it for the exit-time persist too.
                self.last_browsed = Some(kind);
                vec![AppCommand::Browse { kind, req: 0 }]
            }
            None => {
                // Recent: drop browse/search results so the recents fallback shows.
                if self.browse_kind.is_none()
                    && self.last_query.is_none()
                    && self.results.is_empty()
                {
                    return Vec::new();
                }
                self.browse_kind = None;
                self.last_query = None;
                self.results.clear();
                self.view.clear();
                self.selected = 0;
                self.mark_preview_dirty();
                Vec::new()
            }
        }
    }

    /// On an auto-refresh tick, re-run the last query (preserving the cursor)
    /// only when idle on the home screen — so it never fights user input.
    fn on_tick(&mut self) -> Vec<AppCommand> {
        // Skip the auto-refresh while a profile switch is in flight: it would
        // re-search the old client. The next tick after the switch settles picks up.
        if self.mode == Mode::Normal
            && self.screen == Screen::Home
            && !self.switch_in_flight()
            && let Some(query) = self.last_query.clone()
        {
            self.pending_reselect = self.selected_result().map(|r| (r.kind, r.id));
            return vec![self.search_cmd(query)];
        }
        Vec::new()
    }

    /// Manual refresh of whatever's on screen, bound to `r`. On the detail screen
    /// it reloads the current object; on home it re-runs the active search the
    /// same way the auto-refresh tick does — through the same `AppCommand::Search`
    /// dispatch, capturing the cursor in `pending_reselect` so the selection is
    /// preserved when the results land. With nothing to refresh (no detail loaded,
    /// or an empty home with no prior query) it's a safe no-op with a gentle
    /// status, never a stray fetch.
    fn refresh_current_view(&mut self) -> Vec<AppCommand> {
        // Don't reload against the old client mid-switch.
        if self.fence_during_switch() {
            return Vec::new();
        }
        match self.screen {
            Screen::Detail => match &self.detail {
                Some(d) => {
                    let (kind, id) = (d.kind, d.id);
                    self.set_status("refreshing…", Severity::Info);
                    // An explicit refresh always busts the cache and refetches.
                    vec![AppCommand::LoadDetail {
                        kind,
                        id,
                        req: 0,
                        force: true,
                    }]
                }
                None => Vec::new(),
            },
            Screen::Home => match self.last_query.clone() {
                Some(query) => {
                    self.pending_reselect = self.selected_result().map(|r| (r.kind, r.id));
                    self.set_status(format!("refreshing {query}…"), Severity::Info);
                    vec![self.search_cmd(query)]
                }
                None => {
                    self.set_status("nothing to refresh", Severity::Warning);
                    Vec::new()
                }
            },
            Screen::Dashboard => {
                self.set_status("refreshing dashboard…", Severity::Info);
                vec![AppCommand::LoadDashboard { req: 0 }]
            }
            Screen::PrefixTree => {
                self.set_status("refreshing prefixes…", Severity::Info);
                vec![AppCommand::LoadPrefixTree { req: 0 }]
            }
        }
    }

    /// Advance to the next built-in theme. A no-op under `NO_COLOR`: when the app
    /// was started in no-color mode (see [`Self::set_no_color`]) cycling to a
    /// colored theme would override the user's `NO_COLOR` request, so the no-color
    /// theme is left in place and a brief status explains why.
    pub fn cycle_theme(&mut self) {
        if self.no_color_blocks_theme_change() {
            return;
        }
        let list = Theme::list();
        self.theme_index = (self.theme_index + 1) % list.len();
        self.theme = Theme::by_name(list[self.theme_index]);
        self.set_transient_status(format!("theme: {}", self.theme.name()), Severity::Info);
    }

    fn set_theme_by_name(&mut self, name: &str) {
        // Same NO_COLOR guard as the `t` cycle path: the palette `:theme <name>`
        // must not re-enable color under NO_COLOR (and so can't persist a colored
        // theme on exit). Reuses the one guard so the two paths can't diverge.
        if self.no_color_blocks_theme_change() {
            return;
        }
        self.theme = Theme::by_name(name);
        self.theme_index = Theme::index_of(name);
        self.set_transient_status(format!("theme: {}", self.theme.name()), Severity::Info);
    }

    /// Shared NO_COLOR guard for both theme-change paths (`t` cycle and the
    /// palette `:theme <name>`). When the app was started in no-color mode (see
    /// [`Self::set_no_color`]) changing to a colored theme would override the
    /// user's `NO_COLOR` request — and would then persist a colored theme on exit
    /// — so we leave the no-color theme in place and explain why. Returns `true`
    /// when the change is blocked.
    fn no_color_blocks_theme_change(&mut self) -> bool {
        if self.theme.is_no_color() {
            self.set_status("NO_COLOR is set — theme change disabled", Severity::Info);
            true
        } else {
            false
        }
    }

    /// Cycle the active profile by `delta` (+1 forward, -1 backward), wrapping at
    /// both ends, and kick off a reconnect to it. A no-op with a brief status when
    /// fewer than two profiles are configured (nothing to cycle to). The header is
    /// NOT flipped here — it keeps pointing at the connected instance until the
    /// reconnect succeeds (see [`Self::switch_to_index`]), so the UI never lies
    /// about which server `client` is talking to. The network is spawned via
    /// [`AppCommand::SwitchProfile`]; the swap lands on [`AppEvent::ProfileSwitched`].
    fn cycle_profile(&mut self, forward: bool) -> Vec<AppCommand> {
        let len = self.profiles.len();
        if len < 2 {
            self.set_status("only one profile configured", Severity::Info);
            return Vec::new();
        }
        // Step from the pending target if a switch is in flight (so rapid cycling
        // advances as the user sees it), else from the connected profile. Wrap with
        // usize modular arithmetic: forward is +1 mod len; backward adds (len - 1)
        // mod len, so index 0 lands on the last profile without ever going negative.
        let from = self.pending_target_index().unwrap_or(self.profile_index);
        let step = if forward { 1 } else { len - 1 };
        let next = (from + step) % len;
        self.switch_to_index(next)
    }

    /// Jump to the profile named `name` (the palette `profile <name>` verb) and
    /// reconnect to it. An unknown name is a clear error status and no-op;
    /// selecting the already-active profile reconnects it (a cheap manual probe).
    fn select_profile(&mut self, name: &str) -> Vec<AppCommand> {
        match self.profiles.iter().position(|p| p.name == name) {
            Some(idx) => self.switch_to_index(idx),
            None => {
                self.set_status(format!("no profile named '{name}'"), Severity::Error);
                Vec::new()
            }
        }
    }

    /// Common path for cycling and named selection: kick off a reconnect to
    /// `profiles[idx]` WITHOUT touching the header — the header/`base_url`/client
    /// keep pointing at the currently connected instance until the switch
    /// succeeds (see the [`AppEvent::ProfileSwitched`] handler), so the UI can
    /// never claim to be on a server `client` isn't actually talking to. We only
    /// mint a switch id (latest-switch-wins, by id) + record the pending target
    /// and show a "switching…" status; the atomic swap (+ data clear +
    /// request-gen bump) happens on success. `idx` must be in range (callers
    /// guarantee it).
    fn switch_to_index(&mut self, idx: usize) -> Vec<AppCommand> {
        let entry = self.profiles[idx].clone();
        let name = entry.name.clone();
        // Mint a fresh switch id and record it as both the awaited completion and
        // the high-water mark, so a slower, superseded switch — even one to this
        // same profile name — is dropped on arrival. `pending_profile` keeps the
        // name for display + rapid-cycle stepping; the id drives correctness.
        self.switch_seq += 1;
        self.pending_switch = Some(self.switch_seq);
        self.switch_gen = self.switch_seq;
        self.pending_profile = Some(name.clone());
        self.set_status(format!("switching to '{name}'…"), Severity::Info);
        vec![AppCommand::SwitchProfile {
            id: self.switch_seq,
            name: name.clone(),
            config: entry.config,
            config_path: self.config_path.clone(),
        }]
    }

    /// True while a profile switch is in flight: the reconnect to a new instance
    /// has been dispatched but not yet settled. New search/detail/preview fetches
    /// are fenced during this window so they can't be issued against the old
    /// client mid-switch (see [`Self::fence_during_switch`]).
    fn switch_in_flight(&self) -> bool {
        self.pending_profile.is_some()
    }

    /// The `profiles` index of the in-flight switch's target, if any. Used so
    /// rapid cycling steps from the pending target rather than the (unchanged)
    /// connected profile.
    fn pending_target_index(&self) -> Option<usize> {
        let name = self.pending_profile.as_deref()?;
        self.profiles.iter().position(|p| p.name == name)
    }

    /// Guard for user-initiated fetches while a profile switch is in flight:
    /// returns `true` (and shows a brief status) when a fetch must be suppressed,
    /// so it isn't dispatched against the old client mid-switch. Returns `false`
    /// when no switch is pending and the caller should proceed normally.
    fn fence_during_switch(&mut self) -> bool {
        if self.switch_in_flight() {
            self.set_status("switching profile — try again in a moment", Severity::Info);
            true
        } else {
            false
        }
    }

    /// Wipe the data tied to the instance we're leaving and advance the search /
    /// detail request generations so any response still in flight for the old
    /// profile is dropped by the stale-response guard. Recents are cleared too:
    /// they reference the old instance's ids. The home cursor resets to the top.
    fn clear_for_profile_switch(&mut self) {
        // Advance both channels' high-water marks so outstanding requests land
        // stale (the guard compares `req >= *_gen`; bumping past every issued id
        // invalidates them). Preview loads carry (kind,id) and are cleared below.
        self.request_seq += 1;
        self.search_gen = self.request_seq;
        self.detail_gen = self.request_seq;

        self.results.clear();
        self.view.clear();
        self.recent.clear();
        self.selected = 0;
        self.browse_kind = None;
        // Land on a clean results pane: cancel any pending auto-browse and re-anchor
        // the nav tick so the switch can't trip a spurious browse on the new instance.
        self.browse_dirty = false;
        self.nav_tick_anchor = self.nav_selected;
        // Old instance's counts are moot; a LoadNavCounts refetch repopulates them.
        self.nav_counts.clear();
        self.detail = None;
        self.detail_view_state.clear();
        self.detail_tab = 0;
        self.detail_scroll = 0;
        self.preview = None;
        self.preview_for = None;
        self.preview_dirty = false;
        self.preview_scroll = 0;
        self.last_query = None;
        self.pending_reselect = None;
        // Back to the home screen on the new instance; the old history is moot.
        self.screen = Screen::Home;
        self.history.clear();
    }

    /// True when `id` is the latest switch initiated (the high-water mark). A
    /// `ProfileSwitched` with an older id is from a superseded switch (the user
    /// cycled again before it returned) and is dropped on arrival — by id, so an
    /// older switch to the *same* profile name can never settle a newer attempt.
    fn is_current_switch(&self, id: RequestId) -> bool {
        id >= self.switch_gen
    }

    /// Recompute the visible `view` by fuzzy-filtering results on `search_input`.
    fn refilter(&mut self) {
        let displays: Vec<&str> = self.results.iter().map(|r| r.display.as_str()).collect();
        self.view = crate::tui::fuzzy::rank(self.search_input.value(), &displays);
        self.selected = 0;
        // The highlighted result moved; let the debounce reconcile the preview.
        self.mark_preview_dirty();
    }

    /// The currently selected result (through the filtered view), if any.
    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.view
            .get(self.selected)
            .and_then(|&i| self.results.get(i))
    }

    /// The object that visible actions (`o` browser, `y` copy) should target. On
    /// Home this is the highlighted result; on Detail it is the loaded detail
    /// object, so actions never fall through to the hidden Home selection.
    fn action_target(&self) -> Option<ActionTarget> {
        match self.screen {
            Screen::Home => self.selected_result().map(|r| ActionTarget {
                label: r.display.clone(),
                url: r.url.clone(),
            }),
            Screen::Detail => self.detail.as_ref().map(|d| ActionTarget {
                label: d.title.clone(),
                url: object_web_url(&self.base_url, d.kind, d.id),
            }),
            // The dashboard has no single selected object to open/copy.
            Screen::Dashboard => None,
            // On the tree, target the selected prefix so `o`/`y` work there too.
            Screen::PrefixTree => self.tree_selected_node().map(|n| ActionTarget {
                label: n.prefix.clone(),
                url: object_web_url(&self.base_url, ObjectKind::Prefix, n.id),
            }),
        }
    }

    /// Switch the active detail tab by its key (`i`/`p`/`c`/`v`); pressing the
    /// active tab's key again returns to the summary. No-op off the detail screen.
    /// True when the active detail object has a tab bound to `key` — the guard for
    /// the contextual tab-jump keymap (so it only claims keys that are real tabs).
    fn detail_has_tab_key(&self, key: char) -> bool {
        self.screen == Screen::Detail
            && self
                .detail
                .as_ref()
                .is_some_and(|d| d.tabs.iter().any(|t| t.key == key))
    }

    fn select_detail_tab(&mut self, key: char) {
        if self.screen != Screen::Detail {
            return;
        }
        if let Some(detail) = &self.detail
            && let Some(pos) = detail.tabs.iter().position(|t| t.key == key)
        {
            let target = pos + 1;
            self.detail_tab = if self.detail_tab == target { 0 } else { target };
            // Each tab (and the summary) starts scrolled to the top, cursor on the
            // first selectable row of the new section.
            self.detail_scroll = 0;
            self.reset_detail_row();
        }
    }

    /// Cycle the active detail tab with `Tab`/`Shift-Tab`: summary (index 0) →
    /// each section → wrap. Gives the summary a place in the rotation (the
    /// per-section letters `i`/`p`/… have no summary equivalent). A no-op off the
    /// detail screen or when the object has no sections (summary only).
    fn cycle_detail_tab(&mut self, forward: bool) {
        if self.screen != Screen::Detail {
            return;
        }
        let Some(detail) = &self.detail else {
            return;
        };
        let n = detail.tabs.len() + 1; // summary + each section
        if n <= 1 {
            return;
        }
        self.detail_tab = if forward {
            (self.detail_tab + 1) % n
        } else {
            (self.detail_tab + n - 1) % n
        };
        // Each tab (and the summary) starts scrolled to the top, cursor on the
        // first selectable row of the new section.
        self.detail_scroll = 0;
        self.reset_detail_row();
    }

    // --- Interactive detail rows ---------------------------------------------
    //
    // Some detail sections (e.g. a VRF's prefix tree, its addresses) are lists of
    // navigable rows rather than scrollable text: `j`/`k` move a cursor and `Enter`
    // opens the selected row's target — the same drill the `R` modal does, reusing
    // the `b`/`Esc` back-stack. A section with no navigable rows scrolls as before.

    /// The active detail section's rows (summary slot when `detail_tab == 0`, else
    /// the tab's). Empty for plain text sections. Public for the renderer.
    pub fn active_detail_rows(&self) -> &[DetailRow] {
        const EMPTY: &[DetailRow] = &[];
        match &self.detail {
            Some(d) if self.detail_tab == 0 => &d.summary_rows,
            Some(d) => d.tabs.get(self.detail_tab - 1).map_or(EMPTY, |t| &t.rows),
            None => EMPTY,
        }
    }

    /// Row indices in the active section that can be opened (have a target).
    fn selectable_detail_rows(&self) -> Vec<usize> {
        self.active_detail_rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.target.is_some())
            .map(|(i, _)| i)
            .collect()
    }

    /// True when the detail screen's active section is an interactive list — at
    /// least one selectable row. Movement keys then move the cursor (not scroll).
    fn detail_list_active(&self) -> bool {
        self.screen == Screen::Detail
            && self.active_detail_rows().iter().any(|r| r.target.is_some())
    }

    /// Place the cursor on the first selectable row of the active section (or 0).
    fn reset_detail_row(&mut self) {
        self.detail_row = self.selectable_detail_rows().first().copied().unwrap_or(0);
    }

    /// The (kind, id) the selected detail row opens, if any.
    fn detail_row_target(&self) -> Option<(ObjectKind, u64)> {
        self.active_detail_rows()
            .get(self.detail_row)
            .and_then(|r| r.target)
    }

    /// Move the cursor to the next/previous selectable row, keeping it on screen.
    fn detail_row_move(&mut self, forward: bool) {
        let sel = self.selectable_detail_rows();
        let next = if forward {
            sel.iter().copied().find(|&i| i > self.detail_row)
        } else {
            sel.iter().rev().copied().find(|&i| i < self.detail_row)
        };
        if let Some(i) = next {
            self.detail_row = i;
            self.ensure_detail_row_visible();
        }
    }

    fn detail_row_first(&mut self) {
        if let Some(&i) = self.selectable_detail_rows().first() {
            self.detail_row = i;
            self.ensure_detail_row_visible();
        }
    }

    fn detail_row_last(&mut self) {
        if let Some(&i) = self.selectable_detail_rows().last() {
            self.detail_row = i;
            self.ensure_detail_row_visible();
        }
    }

    /// Scroll the list just enough to keep the selected row within the viewport.
    /// In a list section the scroll offset indexes rows directly (the header/tab
    /// bar sit in a fixed band above the scroll area), so the row index is the
    /// target line.
    fn ensure_detail_row_visible(&mut self) {
        let vp = self.detail_viewport.max(1);
        let row = u16::try_from(self.detail_row).unwrap_or(u16::MAX);
        if row < self.detail_scroll {
            self.detail_scroll = row;
        } else if row >= self.detail_scroll.saturating_add(vp) {
            self.detail_scroll = row.saturating_sub(vp).saturating_add(1);
        }
    }

    /// The body text for the active detail tab (summary when `detail_tab` is 0).
    pub fn detail_body(&self) -> &str {
        match &self.detail {
            Some(d) if self.detail_tab > 0 => d
                .tabs
                .get(self.detail_tab - 1)
                .map_or(d.body.as_str(), |t| t.body.as_str()),
            Some(d) => d.body.as_str(),
            None => "loading…",
        }
    }

    /// Total number of rendered content lines for the active detail view: the
    /// body's lines, plus the two-row tab bar (the bar + a blank spacer) when a
    /// device view has tabs. Mirrors what `render_detail` paints.
    pub fn detail_content_lines(&self) -> usize {
        // A header card lifts the header + tab bar into a fixed band (see
        // `render_detail`), so the scroll area is just the active section's content
        // — its navigable rows, or its body lines for a plain text section.
        if matches!(&self.detail, Some(d) if !d.header.is_empty()) {
            let rows = self.active_detail_rows();
            return if rows.is_empty() {
                self.detail_body().lines().count()
            } else {
                rows.len()
            };
        }
        let body_lines = self.detail_body().lines().count();
        let tab_rows = match &self.detail {
            Some(d) if !d.tabs.is_empty() => 2,
            _ => 0,
        };
        body_lines + tab_rows
    }

    /// The largest valid scroll offset: enough that the last content line sits at
    /// the bottom of the pane, never past it. 0 when the content fits (a no-op).
    /// Uses the last-known viewport height stashed at render time.
    fn detail_max_scroll(&self) -> u16 {
        let content = self.detail_content_lines() as u16;
        content.saturating_sub(self.detail_viewport)
    }

    /// A page of detail scroll: the visible height, minus one row of overlap so
    /// the reader keeps their place. At least one line even on a tiny pane.
    fn detail_page(&self) -> u16 {
        self.detail_viewport.saturating_sub(1).max(1)
    }

    fn detail_scroll_down(&mut self, lines: u16) {
        let max = self.detail_max_scroll();
        self.detail_scroll = self.detail_scroll.saturating_add(lines).min(max);
    }

    fn detail_scroll_up(&mut self, lines: u16) {
        self.detail_scroll = self.detail_scroll.saturating_sub(lines);
    }

    fn detail_scroll_top(&mut self) {
        self.detail_scroll = 0;
    }

    fn detail_scroll_bottom(&mut self) {
        self.detail_scroll = self.detail_max_scroll();
    }

    /// Stash the detail pane's inner height (content rows) so the pure scroll
    /// handler can clamp at the bottom, and re-clamp the current offset in case
    /// the pane shrank under it. Called from the render path each frame.
    pub fn sync_detail_viewport(&mut self, height: u16) {
        self.detail_viewport = height;
        self.detail_scroll = self.detail_scroll.min(self.detail_max_scroll());
    }

    // --- Body-scroll routing -------------------------------------------------
    //
    // The movement keys (j/k/g/G/PgUp/PgDn) scroll a body when [`Self::scrolls_body`]
    // is true. These dispatchers send that scroll to the detail screen's body or,
    // on Home, to the focused preview pane — reusing the same clamp-at-bottom
    // mechanics for both.

    fn body_scroll_down(&mut self, lines: u16) {
        if self.screen == Screen::Detail {
            self.detail_scroll_down(lines);
        } else {
            self.preview_scroll_down(lines);
        }
    }

    fn body_scroll_up(&mut self, lines: u16) {
        if self.screen == Screen::Detail {
            self.detail_scroll_up(lines);
        } else {
            self.preview_scroll_up(lines);
        }
    }

    fn body_scroll_top(&mut self) {
        if self.screen == Screen::Detail {
            self.detail_scroll_top();
        } else {
            self.preview_scroll = 0;
        }
    }

    fn body_scroll_bottom(&mut self) {
        if self.screen == Screen::Detail {
            self.detail_scroll_bottom();
        } else {
            self.preview_scroll = self.preview_max_scroll();
        }
    }

    /// A page of scroll for whichever body is active (detail vs preview).
    fn body_page(&self) -> u16 {
        if self.screen == Screen::Detail {
            self.detail_page()
        } else {
            self.preview_page()
        }
    }

    // --- Preview pane scrolling ----------------------------------------------
    //
    // Mirrors the detail-scroll machinery, scoped to the live preview body. The
    // preview shows the loaded detail's summary (tab 0); tabs aren't switchable
    // in the peek, so the line count is just the body's lines.

    /// The preview body shown in the right pane: the loaded detail's summary when
    /// it matches the highlighted row, otherwise a built-from-the-result
    /// lightweight peek (shown instantly while a full load is in flight, so the
    /// pane never displays a stale object's body under a moved cursor).
    ///
    /// Returns a [`Cow`] so the common case (a current detail) borrows `d.body`
    /// rather than cloning the whole string each frame (M10); only the placeholder
    /// path allocates. The render path fetches this once and reuses it for both
    /// line-counting and drawing.
    pub fn preview_body(&self) -> std::borrow::Cow<'_, str> {
        match &self.preview {
            Some(d) if self.preview_for == self.preview_selection() => {
                std::borrow::Cow::Borrowed(d.body.as_str())
            }
            _ => std::borrow::Cow::Owned(self.preview_placeholder()),
        }
    }

    /// The preview pane's title: the loaded detail's title when current, else a
    /// short label from the highlighted row, else a neutral "Preview".
    pub fn preview_title(&self) -> String {
        match &self.preview {
            Some(d) if self.preview_for == self.preview_selection() => d.title.clone(),
            _ => match self.selected_home_preview() {
                Some(stub) => format!("{} {}", stub.kind, stub.display),
                None => "Preview".to_string(),
            },
        }
    }

    /// A lightweight peek assembled from the data already in the `SearchResult`
    /// (kind, display, subtitle, url) — shown instantly with no fetch while the
    /// full detail loads, or as the resting content if the load hasn't fired yet.
    fn preview_placeholder(&self) -> String {
        match self.selected_home_preview() {
            Some(PreviewStub {
                kind,
                display,
                subtitle,
                url,
            }) => {
                use std::fmt::Write;
                let mut s = format!("{kind}: {display}\n");
                if let Some(sub) = subtitle {
                    let _ = writeln!(s, "{sub}");
                }
                if let Some(url) = url {
                    let _ = writeln!(s, "\n{url}");
                }
                s.push_str("\nLoading details…");
                s
            }
            None => "Nothing selected.\n\nPress / to search NetBox.".to_string(),
        }
    }

    /// The data for a lightweight preview of the highlighted home row: a search
    /// result carries a URL/subtitle; a recent carries only its title.
    fn selected_home_preview(&self) -> Option<PreviewStub<'_>> {
        if self.results.is_empty() {
            self.recent.get(self.selected).map(|r| PreviewStub {
                kind: r.kind.as_str(),
                display: &r.title,
                subtitle: None,
                url: None,
            })
        } else {
            self.selected_result().map(|r| PreviewStub {
                kind: r.kind.as_str(),
                display: &r.display,
                subtitle: r.subtitle.as_deref(),
                url: Some(&r.url),
            })
        }
    }

    /// Number of rendered preview content lines (the body only — no tab bar).
    pub fn preview_content_lines(&self) -> usize {
        self.preview_body().lines().count()
    }

    fn preview_max_scroll(&self) -> u16 {
        (self.preview_content_lines() as u16).saturating_sub(self.preview_viewport)
    }

    fn preview_page(&self) -> u16 {
        self.preview_viewport.saturating_sub(1).max(1)
    }

    fn preview_scroll_down(&mut self, lines: u16) {
        let max = self.preview_max_scroll();
        self.preview_scroll = self.preview_scroll.saturating_add(lines).min(max);
    }

    fn preview_scroll_up(&mut self, lines: u16) {
        self.preview_scroll = self.preview_scroll.saturating_sub(lines);
    }

    /// Stash the preview pane's inner height and re-clamp the offset, mirroring
    /// [`Self::sync_detail_viewport`]. Called from the render path each frame.
    pub fn sync_preview_viewport(&mut self, height: u16) {
        self.preview_viewport = height;
        self.preview_scroll = self.preview_scroll.min(self.preview_max_scroll());
    }

    /// Record an opened object at the front of the recents list (deduped, capped).
    fn record_recent(&mut self, kind: ObjectKind, id: u64, title: String) {
        self.recent.retain(|r| !(r.kind == kind && r.id == id));
        self.recent.insert(0, RecentItem { kind, id, title });
        self.recent.truncate(RECENT_CAP);
    }

    /// Length of the active home list: search results, or recents when empty.
    /// Public so the render path can show a `selected/len` row-position hint.
    pub fn home_len(&self) -> usize {
        if self.results.is_empty() {
            self.recent.len()
        } else {
            self.view.len()
        }
    }

    /// Number of recently-opened items — shown as the Nav `Recent` row's count.
    pub fn recent_count(&self) -> usize {
        self.recent.len()
    }

    /// The (kind, id) the home cursor points at — a result, or a recent.
    fn home_target(&self) -> Option<(ObjectKind, u64)> {
        if self.results.is_empty() {
            self.recent.get(self.selected).map(|r| (r.kind, r.id))
        } else {
            self.selected_result().map(|r| (r.kind, r.id))
        }
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.home_len() {
            self.selected += 1;
            self.mark_preview_dirty();
        }
    }

    fn select_prev(&mut self) {
        let before = self.selected;
        self.selected = self.selected.saturating_sub(1);
        if self.selected != before {
            self.mark_preview_dirty();
        }
    }

    fn select_first(&mut self) {
        if self.selected != 0 {
            self.selected = 0;
            self.mark_preview_dirty();
        }
    }

    fn select_last(&mut self) {
        let last = self.home_len().saturating_sub(1);
        if self.selected != last {
            self.selected = last;
            self.mark_preview_dirty();
        }
    }

    /// Rows the results-list cursor jumps on PgUp/PgDn: one screenful of the live
    /// list viewport, minus a row of overlap so the reader keeps their place.
    /// Before the first render (viewport still 0) the pre-render fallback applies;
    /// always at least one row.
    fn list_page(&self) -> usize {
        if self.list_viewport == 0 {
            PAGE_JUMP_FALLBACK
        } else {
            (self.list_viewport as usize).saturating_sub(1).max(1)
        }
    }

    fn select_page_down(&mut self) {
        let last = self.home_len().saturating_sub(1);
        let next = (self.selected + self.list_page()).min(last);
        if next != self.selected {
            self.selected = next;
            self.mark_preview_dirty();
        }
    }

    fn select_page_up(&mut self) {
        let next = self.selected.saturating_sub(self.list_page());
        if next != self.selected {
            self.selected = next;
            self.mark_preview_dirty();
        }
    }

    /// Stash the results-list pane's inner height (visible rows) so the pure
    /// PgUp/PgDn handler can page by one screenful. Called from the render path
    /// each frame. Mirrors [`Self::sync_detail_viewport`]/[`Self::sync_preview_viewport`].
    pub fn sync_list_viewport(&mut self, height: u16) {
        self.list_viewport = height;
    }

    /// Sync the stateful-table selection from the pure `selected` index so the
    /// render path scrolls the offset to keep the cursor visible. Empty tables
    /// select nothing (no panic); otherwise the index is clamped into range.
    /// `TableState` drives row visibility identically to the old `ListState`.
    pub fn sync_table_state(&mut self) {
        let len = self.home_len();
        if len == 0 {
            self.table_state.select(None);
        } else {
            self.selected = self.selected.min(len - 1);
            self.table_state.select(Some(self.selected));
        }
    }
}

/// The three aligned column cells for a search-result row: its kind, its display
/// label, and its site (taken from the result's `subtitle`, since the search
/// result carries no separate site field — the subtitle holds the site/scope).
/// Pure so the render path's row-building can be unit-tested without a terminal.
pub fn result_row_cells(result: &SearchResult) -> [String; 3] {
    [
        result.kind.as_str().to_string(),
        result.display.clone(),
        result.subtitle.clone().unwrap_or_default(),
    ]
}

/// A borrowed, lightweight view of the highlighted home row used to render the
/// preview placeholder before (or without) a full detail load.
struct PreviewStub<'a> {
    kind: &'a str,
    display: &'a str,
    subtitle: Option<&'a str>,
    url: Option<&'a str>,
}

struct ActionTarget {
    label: String,
    url: String,
}

/// Build the NetBox web URL for a detail object from the active profile base URL.
/// The base may itself live under a subpath (`https://host/netbox/`), matching the
/// client URL-join behavior.
fn object_web_url(base_url: &str, kind: ObjectKind, id: u64) -> String {
    let path = match kind {
        ObjectKind::Device => format!("dcim/devices/{id}/"),
        ObjectKind::Site => format!("dcim/sites/{id}/"),
        ObjectKind::IpAddress => format!("ipam/ip-addresses/{id}/"),
        ObjectKind::Prefix => format!("ipam/prefixes/{id}/"),
        ObjectKind::Vlan => format!("ipam/vlans/{id}/"),
        ObjectKind::Circuit => format!("circuits/circuits/{id}/"),
        ObjectKind::Aggregate => format!("ipam/aggregates/{id}/"),
        ObjectKind::Asn => format!("ipam/asns/{id}/"),
        ObjectKind::IpRange => format!("ipam/ip-ranges/{id}/"),
        ObjectKind::Tenant => format!("tenancy/tenants/{id}/"),
        ObjectKind::Contact => format!("tenancy/contacts/{id}/"),
        ObjectKind::Provider => format!("circuits/providers/{id}/"),
        ObjectKind::Vm => format!("virtualization/virtual-machines/{id}/"),
        ObjectKind::Cluster => format!("virtualization/clusters/{id}/"),
        ObjectKind::Rack => format!("dcim/racks/{id}/"),
        ObjectKind::Vrf => format!("ipam/vrfs/{id}/"),
        ObjectKind::RouteTarget => format!("ipam/route-targets/{id}/"),
        ObjectKind::Interface => format!("dcim/interfaces/{id}/"),
    };

    let mut base = base_url.to_string();
    if !base.ends_with('/') {
        base.push('/');
    }
    reqwest::Url::parse(&base)
        .and_then(|url| url.join(&path))
        .map_or_else(
            |_| format!("{}/{path}", base_url.trim_end_matches('/')),
            |url| url.to_string(),
        )
}

/// Classify a free-form status message (typically pushed from an async task,
/// e.g. "copied: …" / "open failed: …") into a [`Severity`] so the footer can
/// color it. Failures → error, confirmations (copied/opened/refreshed/done) →
/// success, anything else → neutral. Pure and case-insensitive.
fn classify_status(message: &str) -> Severity {
    let m = message.to_ascii_lowercase();
    // "partial" wins over "failed": a partial result names failed endpoints but
    // is a degraded, not a hard, failure.
    if m.contains("partial") {
        Severity::Warning
    } else if m.contains("error") || m.contains("failed") || m.contains("failure") {
        Severity::Error
    } else if m.starts_with("copied")
        || m.starts_with("opened")
        || m.contains("refreshed")
        || m.contains("done")
    {
        Severity::Success
    } else {
        Severity::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileConfig;

    fn app() -> App {
        let profile = ProfileConfig {
            url: "http://localhost".into(),
            ..Default::default()
        };
        let client = NetBoxClient::new(&profile, None).unwrap();
        App::new(
            client,
            "default",
            "test".into(),
            "http://localhost".into(),
            "4.5.5".into(),
            None,
        )
    }

    fn result(id: u64, display: &str) -> SearchResult {
        SearchResult {
            kind: ObjectKind::Device,
            id,
            display: display.into(),
            subtitle: None,
            url: format!("http://nb/dcim/devices/{id}/"),
            score: 100,
        }
    }

    fn result_of(kind: ObjectKind, id: u64, display: &str) -> SearchResult {
        SearchResult {
            kind,
            display: display.into(),
            ..result(id, display)
        }
    }

    #[test]
    fn result_row_cells_maps_kind_display_and_site() {
        // The aligned table's three columns come from the result: kind, display,
        // and site — where "site" is the subtitle (the search result has no
        // dedicated site field; the subtitle carries the site/scope).
        let r = SearchResult {
            subtitle: Some("iad1".into()),
            ..result(1, "edge01")
        };
        assert_eq!(
            result_row_cells(&r),
            [
                "device".to_string(),
                "edge01".to_string(),
                "iad1".to_string()
            ]
        );
    }

    #[test]
    fn result_row_cells_site_is_blank_without_subtitle() {
        // No subtitle → the SITE cell is empty (not "None"); the column still
        // aligns because the empty string keeps its place in the row.
        let r = result(2, "core02"); // subtitle is None
        assert_eq!(
            result_row_cells(&r),
            ["device".to_string(), "core02".to_string(), String::new()]
        );
    }

    fn detail_view(kind: ObjectKind, id: u64) -> DetailView {
        DetailView {
            kind,
            id,
            title: format!("{} {id}", kind.as_str()),
            body: String::new(),
            tabs: Vec::new(),
            links: Vec::new(),
            header: Vec::new(),
            summary_label: String::new(),
            summary_rows: Vec::new(),
        }
    }

    #[test]
    fn update_available_sets_banner_and_u_dismisses_it() {
        let mut a = app();
        assert!(a.update_available.is_none());
        // A newer release drives the banner (raw version stored; the banner strips
        // the leading `v` at render).
        a.handle_event(AppEvent::UpdateAvailable(Some("v0.3.0".into())));
        assert_eq!(a.update_available.as_deref(), Some("v0.3.0"));
        // `u` dismisses it.
        a.handle_event(press(KeyCode::Char('u')));
        assert!(a.update_available.is_none());
        // An up-to-date / skipped result leaves no banner.
        a.handle_event(AppEvent::UpdateAvailable(None));
        assert!(a.update_available.is_none());
    }

    #[test]
    fn refresh_on_detail_forces_cache_bust() {
        let mut a = app();
        // Land on the detail screen with a loaded object.
        a.handle_event(AppEvent::DetailLoaded {
            req: a.detail_gen,
            result: Ok(detail_view(ObjectKind::Device, 7)),
        });
        assert_eq!(a.screen, Screen::Detail);
        // `r` must reload with force=true so a refresh never re-serves the cache.
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(
            matches!(
                cmds.as_slice(),
                [AppCommand::LoadDetail {
                    kind: ObjectKind::Device,
                    id: 7,
                    force: true,
                    ..
                }]
            ),
            "refresh forces a cache bust, got {cmds:?}"
        );
    }

    #[test]
    fn opening_detail_from_home_does_not_force_bust() {
        let mut a = app();
        set_results(&mut a, vec![result(7, "edge01")]);
        // Normal navigation uses the cache (force=false).
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail { force: false, .. }]
        ));
    }

    #[test]
    fn detail_freshness_adopted_when_current_ignored_when_stale() {
        use crate::cache::Source;
        let mut a = app();
        a.detail_gen = 5;
        a.handle_event(AppEvent::DetailFreshness {
            req: 5,
            freshness: Freshness {
                source: Source::Cache,
                age: 12,
            },
        });
        assert_eq!(a.detail_freshness.map(|f| f.age), Some(12));
        // A freshness for a superseded (older) detail request is ignored.
        a.handle_event(AppEvent::DetailFreshness {
            req: 4,
            freshness: Freshness {
                source: Source::Cache,
                age: 99,
            },
        });
        assert_eq!(
            a.detail_freshness.map(|f| f.age),
            Some(12),
            "a stale freshness must not overwrite the current one"
        );
    }

    fn press(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Deliver a clean search result as if it were the freshest in-flight one:
    /// tagged with the app's current `search_gen` so the latest-wins guard
    /// adopts it (whether or not a search was actually dispatched first).
    fn set_results(a: &mut App, items: Vec<SearchResult>) {
        let req = a.search_gen;
        a.handle_event(AppEvent::SearchComplete {
            req,
            result: Ok(SearchOutcome {
                results: items,
                errors: Vec::new(),
            }),
        });
        // Mirror a user-submitted search: the results pane is focused (the submit
        // path sets this; SearchComplete on its own doesn't, so tests that drive
        // list navigation get the realistic focus here).
        a.focus = Focus::List;
    }

    /// Deliver `result` on the search channel tagged as the current request, so
    /// it passes the latest-wins guard. For the stale-drop path a test stamps an
    /// older `req` explicitly.
    fn search_complete(a: &mut App, result: anyhow::Result<SearchOutcome>) {
        let req = a.search_gen;
        a.handle_event(AppEvent::SearchComplete { req, result });
    }

    /// Deliver a detail load tagged as the current request (passes the guard).
    fn detail_loaded(a: &mut App, result: anyhow::Result<DetailView>) {
        let req = a.detail_gen;
        a.handle_event(AppEvent::DetailLoaded { req, result });
    }

    #[test]
    fn detail_rows_navigate_and_open() {
        use crate::domain::detail::{DetailRow, DetailTab};
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Vrf,
                id: 4,
                title: "vrf customer-prod".into(),
                body: String::new(),
                tabs: vec![DetailTab {
                    key: 'a',
                    label: "addresses·1".into(),
                    body: String::new(),
                    rows: vec![DetailRow::link(
                        "10.0.0.1".into(),
                        ObjectKind::IpAddress,
                        11,
                    )],
                }],
                links: Vec::new(),
                header: vec!["RD 65000:100".into()],
                summary_label: "prefixes·2".into(),
                summary_rows: vec![
                    DetailRow::link("10.0.0.0/24".into(), ObjectKind::Prefix, 21),
                    DetailRow::link("10.0.1.0/24".into(), ObjectKind::Prefix, 22),
                    DetailRow::plain("… 1 more".into()),
                ],
            }),
        );
        a.sync_detail_viewport(10);
        // The cursor lands on the first selectable summary row.
        assert_eq!(a.detail_row, 0);
        // `j` advances to the next selectable row …
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.detail_row, 1);
        // … and stops before the non-selectable footer row.
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.detail_row, 1);
        // Enter opens the selected prefix (pushing the VRF onto the back-stack).
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(
                cmds.as_slice(),
                [AppCommand::LoadDetail {
                    kind: ObjectKind::Prefix,
                    id: 22,
                    ..
                }]
            ),
            "got: {cmds:?}"
        );
        assert_eq!(a.detail_nav.last(), Some(&(ObjectKind::Vrf, 4)));
    }

    #[test]
    fn detail_tab_switch_resets_row_cursor() {
        use crate::domain::detail::{DetailRow, DetailTab};
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Vrf,
                id: 4,
                title: "vrf v".into(),
                body: String::new(),
                tabs: vec![DetailTab {
                    key: 'a',
                    label: "addresses·2".into(),
                    body: String::new(),
                    rows: vec![
                        DetailRow::link("10.0.0.1".into(), ObjectKind::IpAddress, 11),
                        DetailRow::link("10.0.0.2".into(), ObjectKind::IpAddress, 12),
                    ],
                }],
                links: Vec::new(),
                header: vec!["RD 65000:100".into()],
                summary_label: "prefixes·2".into(),
                summary_rows: vec![
                    DetailRow::link("10.0.0.0/24".into(), ObjectKind::Prefix, 21),
                    DetailRow::link("10.0.1.0/24".into(), ObjectKind::Prefix, 22),
                ],
            }),
        );
        a.sync_detail_viewport(10);
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.detail_row, 1);
        // Switching to the addresses tab resets the cursor to its first row, and
        // Enter there opens that address.
        a.handle_event(press(KeyCode::Char('a')));
        assert_eq!(a.detail_tab, 1);
        assert_eq!(a.detail_row, 0);
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail {
                kind: ObjectKind::IpAddress,
                id: 11,
                ..
            }]
        ));
    }

    #[test]
    fn classify_status_maps_messages_to_severity() {
        assert_eq!(classify_status("copied: edge01"), Severity::Success);
        assert_eq!(classify_status("opened in browser"), Severity::Success);
        assert_eq!(
            classify_status("copy failed: no clipboard"),
            Severity::Error
        );
        assert_eq!(classify_status("open failed: x"), Severity::Error);
        assert_eq!(classify_status("error: 404 not found"), Severity::Error);
        assert_eq!(
            classify_status("3 result(s) (partial: 1 endpoint(s) failed)"),
            Severity::Warning
        );
        assert_eq!(classify_status("searching edge…"), Severity::Info);
        assert_eq!(classify_status("theme: nord"), Severity::Info);
    }

    #[test]
    fn search_complete_sets_success_severity_clean_and_warning_partial() {
        let mut a = app();
        // A clean result set is a success-colored confirmation.
        set_results(&mut a, results_n(2));
        assert_eq!(a.status_severity, Severity::Success);
        // A partial result (some endpoints failed) is warning-colored.
        search_complete(
            &mut a,
            Ok(SearchOutcome {
                results: vec![result(1, "edge01")],
                errors: vec!["dcim/devices: 500".into()],
            }),
        );
        assert_eq!(a.status_severity, Severity::Warning);
        assert!(a.status.contains("partial"));
    }

    #[test]
    fn request_error_sets_error_severity() {
        let mut a = app();
        search_complete(&mut a, Err(anyhow::anyhow!("403 forbidden")));
        assert_eq!(a.status_severity, Severity::Error);
        detail_loaded(&mut a, Err(anyhow::anyhow!("404 not found")));
        assert_eq!(a.status_severity, Severity::Error);
    }

    #[test]
    fn async_status_event_is_classified() {
        let mut a = app();
        a.handle_event(AppEvent::Status("copied: edge01".into()));
        assert_eq!(a.status_severity, Severity::Success);
        a.handle_event(AppEvent::Status("copy failed: x".into()));
        assert_eq!(a.status_severity, Severity::Error);
    }

    #[test]
    fn async_confirmation_fades_but_failure_persists() {
        let mut a = app();
        // A success confirmation (e.g. `o` → "opened in browser") arms a TTL so
        // it fades from the status line; ticking it down clears it.
        a.handle_event(AppEvent::Status("opened in browser".into()));
        assert_eq!(a.status_ttl, Some(TRANSIENT_STATUS_TICKS));
        for _ in 0..TRANSIENT_STATUS_TICKS {
            a.tick_status_ttl();
        }
        assert!(a.status.is_empty(), "confirmation should have faded");

        // A failure persists (no TTL) until the next action replaces it.
        a.handle_event(AppEvent::Status("open failed: nope".into()));
        assert_eq!(a.status_ttl, None);
        a.tick_status_ttl();
        assert_eq!(a.status, "open failed: nope", "failure must not fade");
    }

    // --- Loading spinner / in-flight counter --------------------------------

    #[test]
    fn dispatched_search_sets_loading_and_result_clears_it() {
        let mut a = app();
        assert!(!a.loading(), "idle at rest");
        // Submitting a search dispatches a fetch → loading.
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { .. }]));
        assert!(a.loading(), "a dispatched search is in flight");
        assert_eq!(a.pending, 1);
        // The matching result clears it.
        set_results(&mut a, results_n(2));
        assert!(!a.loading(), "the result settles the fetch");
        assert_eq!(a.pending, 0);
    }

    #[test]
    fn enter_load_detail_sets_loading_until_detail_arrives() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        assert!(!a.loading());
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(cmds.as_slice(), [AppCommand::LoadDetail { .. }]));
        assert!(a.loading());
        detail_loaded(&mut a, Ok(preview_view(1, "body")));
        assert!(!a.loading());
    }

    #[test]
    fn preview_load_does_not_drive_the_spinner() {
        let mut a = app();
        set_results(&mut a, results_n(3));
        assert!(!a.loading());
        // The debounce flush issues a preview fetch — background work that must
        // NOT pulse the spinner (scrolling the list stays calm).
        let cmds = preview_tick(&mut a);
        assert!(matches!(cmds.as_slice(), [AppCommand::LoadPreview { .. }]));
        assert!(!a.loading(), "preview loads don't raise the spinner");
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "body")),
        });
        assert!(!a.loading());
        assert!(a.preview.is_some(), "preview body adopted");
    }

    #[test]
    fn stale_preview_response_is_dropped() {
        // A preview response for a selection the cursor has moved past is dropped;
        // previews never touch the spinner regardless.
        let mut a = app();
        set_results(&mut a, results_n(3));
        let _ = preview_tick(&mut a); // LoadPreview for id 1
        assert!(!a.loading(), "previews don't drive the spinner");
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 2,
            result: Ok(preview_view(2, "stale")),
        });
        assert!(a.preview.is_none(), "stale body not adopted");
    }

    #[test]
    fn pending_counter_never_goes_negative() {
        // A result event with nothing in flight must not underflow the counter.
        let mut a = app();
        assert_eq!(a.pending, 0);
        detail_loaded(&mut a, Err(anyhow::anyhow!("404")));
        assert_eq!(a.pending, 0, "clamped at zero, no underflow");
        assert!(!a.loading());
    }

    #[test]
    fn concurrent_fetches_require_all_to_resolve_before_idle() {
        // Two user-visible fetches in flight (a refresh search + a detail open);
        // idle is only reached once *both* resolve (the counter, not a bool).
        let mut a = app();
        set_results(&mut a, results_n(3)); // settles to idle
        a.last_query = Some("edge".into());
        // A refresh tick dispatches a Search (counts as one).
        let refresh = a.handle_event(AppEvent::Tick);
        assert!(matches!(refresh.as_slice(), [AppCommand::Search { .. }]));
        assert_eq!(a.pending, 1);
        // Opening a detail is a second tracked fetch.
        let open = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(open.as_slice(), [AppCommand::LoadDetail { .. }]));
        assert_eq!(a.pending, 2);
        assert!(a.loading());
        // The detail resolves first — still loading (search outstanding).
        detail_loaded(&mut a, Ok(detail_view(ObjectKind::Device, 1)));
        assert_eq!(a.pending, 1);
        assert!(a.loading(), "still loading: the search is outstanding");
        // The search resolves — now idle.
        set_results(&mut a, results_n(3));
        assert_eq!(a.pending, 0);
        assert!(!a.loading());
    }

    #[test]
    fn spinner_advances_only_while_loading_and_stops_when_idle() {
        let mut a = app();
        // Idle: a preview tick must not advance the spinner (no busy-spin).
        let resting = a.spinner.frame().to_string();
        a.handle_event(AppEvent::PreviewTick);
        assert_eq!(a.spinner.frame(), resting, "idle spinner is still");

        // Put a real fetch in flight (a refresh search), then ticks animate it.
        set_results(&mut a, results_n(3));
        a.last_query = Some("edge".into());
        let _ = a.handle_event(AppEvent::Tick); // dispatch a Search → loading
        assert!(a.loading());
        let before = a.spinner.frame().to_string();
        a.handle_event(AppEvent::PreviewTick);
        assert_ne!(
            a.spinner.frame(),
            before,
            "loading spinner advances on tick"
        );

        // Resolve it: the spinner stops advancing again.
        set_results(&mut a, results_n(3));
        assert!(!a.loading());
        let idle = a.spinner.frame().to_string();
        a.handle_event(AppEvent::PreviewTick);
        assert_eq!(a.spinner.frame(), idle, "idle again: spinner holds still");
    }

    #[test]
    fn open_and_copy_are_not_tracked_as_loading() {
        // Fire-and-forget side effects (open/copy) don't count as in-flight
        // fetches, so they never raise the spinner.
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        let _ = a.handle_event(press(KeyCode::Char('o')));
        assert!(!a.loading(), "OpenBrowser is not a tracked fetch");
        let _ = a.handle_event(press(KeyCode::Char('y')));
        assert!(!a.loading(), "Copy is not a tracked fetch");
    }

    #[test]
    fn q_quits_from_home() {
        let mut a = app();
        assert!(a.handle_event(press(KeyCode::Char('q'))).is_empty());
        assert!(a.should_quit);
    }

    /// True iff the Help modal is open.
    fn help_open(a: &App) -> bool {
        matches!(a.modal, Some(Modal::Help))
    }

    /// True iff the Config modal is open.
    fn config_open(a: &App) -> bool {
        matches!(a.modal, Some(Modal::Config(_)))
    }

    #[test]
    fn help_toggles_open_and_any_key_closes_it_without_quitting() {
        // Help is an overlay (the `modal` enum), not a screen: ?/F1 open it without
        // changing the underlying screen or pushing history; while open, any key
        // closes it and q (consumed by the modal) does NOT quit the app.
        let mut a = app();
        assert!(a.modal.is_none());
        a.handle_event(press(KeyCode::Char('?')));
        assert!(help_open(&a));
        assert_eq!(a.screen, Screen::Home, "help doesn't change the screen");
        assert!(a.history.is_empty(), "help doesn't push history");
        // q while help is open closes the modal, not the app.
        a.handle_event(press(KeyCode::Char('q')));
        assert!(a.modal.is_none());
        assert!(!a.should_quit);
        assert_eq!(a.screen, Screen::Home);

        // F1 toggles it open too, and ?/F1 again (a fresh handler call) re-toggles
        // by way of any-key-close.
        a.handle_event(press(KeyCode::F(1)));
        assert!(help_open(&a));
        a.handle_event(press(KeyCode::F(1)));
        assert!(
            a.modal.is_none(),
            "any key (incl. F1) closes the open modal"
        );

        // Esc also closes it.
        a.handle_event(press(KeyCode::Char('?')));
        assert!(help_open(&a));
        a.handle_event(press(KeyCode::Esc));
        assert!(a.modal.is_none());
    }

    #[test]
    fn open_help_consumes_the_key_without_acting_on_underlying_screen() {
        // With help open, an arbitrary key (here `j`, normally a selection move on
        // Home) only closes the modal — it must NOT also move the selection or
        // disturb the underlying screen.
        let mut a = app();
        set_results(&mut a, results_n(3));
        assert_eq!(a.selected, 0);
        a.handle_event(press(KeyCode::Char('?')));
        assert!(help_open(&a));
        let cmds = a.handle_event(press(KeyCode::Char('j')));
        assert!(cmds.is_empty(), "an any-key-close issues no command");
        assert!(a.modal.is_none(), "the key closed the modal");
        assert_eq!(a.selected, 0, "the key did not move the selection");
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn slash_enters_search_and_enter_emits_command() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        assert_eq!(a.mode, Mode::Search);
        for c in "edge01".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Normal);
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "edge01"));
        assert_eq!(a.last_query.as_deref(), Some("edge01"));
    }

    #[test]
    fn typing_in_search_fuzzy_filters_the_view() {
        let mut a = app();
        set_results(
            &mut a,
            vec![
                result(1, "edge01"),
                result(2, "core02"),
                result(3, "edge-rtr"),
            ],
        );
        assert_eq!(a.view.len(), 3);

        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        // Only the two "edge" results remain in the filtered view.
        assert_eq!(a.view.len(), 2);
        let visible: Vec<u64> = a.view.iter().map(|&i| a.results[i].id).collect();
        assert!(visible.contains(&1) && visible.contains(&3));
        assert!(!visible.contains(&2));
    }

    #[test]
    fn esc_cancels_search_without_emitting_a_command() {
        // Esc must leave search mode and issue no command, with the typed text
        // discarded (the next `/` opens a fresh, empty line).
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        assert_eq!(a.search_input.value(), "edge");
        let cmds = a.handle_event(press(KeyCode::Esc));
        assert!(cmds.is_empty());
        assert_eq!(a.mode, Mode::Normal);
        // Reopening search starts empty.
        a.handle_event(press(KeyCode::Char('/')));
        assert_eq!(a.search_input.value(), "");
    }

    #[test]
    fn search_cursor_editing_inserts_mid_string() {
        // The cheese-backed cursor lets Home/Left position the insertion point so
        // characters land mid-string, driven entirely through the pure handler.
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "ege".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        // Move to the start, step right once (between 'e' and 'g'), insert 'd'.
        a.handle_event(press(KeyCode::Home));
        a.handle_event(press(KeyCode::Right));
        a.handle_event(press(KeyCode::Char('d')));
        assert_eq!(a.search_input.value(), "edge");
        // Backspace removes the char before the cursor (the 'd' just typed).
        a.handle_event(press(KeyCode::Backspace));
        assert_eq!(a.search_input.value(), "ege");
        // Delete (forward) at this position removes the 'g' under the cursor.
        a.handle_event(press(KeyCode::Delete));
        assert_eq!(a.search_input.value(), "ee");
    }

    #[test]
    fn search_enter_after_cursor_editing_submits_full_value() {
        // After mid-string editing, Enter still submits the whole buffer as the
        // search query — the submit semantics are unchanged by the cursor work.
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge1".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        a.handle_event(press(KeyCode::Home));
        a.handle_event(press(KeyCode::Char('x'))); // prepend
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "xedge1"));
        assert_eq!(a.last_query.as_deref(), Some("xedge1"));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn esc_cancels_command_palette() {
        // Esc closes the palette, runs nothing, and discards the typed command.
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "device edge01".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        assert_eq!(a.command_input.value(), "device edge01");
        let cmds = a.handle_event(press(KeyCode::Esc));
        assert!(cmds.is_empty());
        assert_eq!(a.mode, Mode::Normal);
        a.handle_event(press(KeyCode::Char(':')));
        assert_eq!(a.command_input.value(), "");
    }

    #[test]
    fn command_cursor_editing_then_enter_executes_edited_command() {
        // Cursor editing in the palette feeds into the same parse → AppCommand
        // flow: build "device edge01" by inserting the digit mid-token, submit.
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "device edge0".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        a.handle_event(press(KeyCode::Char('1')));
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadByRef { kind: ObjectKind::Device, value, .. }] if value == "edge01"
        ));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn enter_on_result_loads_detail_and_back_returns_home() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail {
                kind: ObjectKind::Device,
                id: 1,
                ..
            }]
        ));

        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: "name: edge01".into(),
                tabs: Vec::new(),
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        assert_eq!(a.screen, Screen::Detail);
        // Opening recorded it in recents.
        assert_eq!(a.recent.len(), 1);
        assert_eq!(a.recent[0].id, 1);

        a.handle_event(press(KeyCode::Char('b')));
        assert_eq!(a.screen, Screen::Home);
        // Back to the list marks the preview dirty so the highlighted row loads on
        // the next tick — no "go down then up" nudge to clear the ghost state.
        assert!(
            a.preview_dirty,
            "returning to the list should reconcile the preview"
        );
    }

    #[test]
    fn enter_on_new_kind_result_loads_detail_with_that_kind() {
        // The newer search kinds (circuit/aggregate/ASN/IP-range) must dispatch
        // a LoadDetail carrying their own ObjectKind + id — the same glue the
        // device path uses — so they open in the TUI like every other kind.
        for kind in [
            ObjectKind::Circuit,
            ObjectKind::Aggregate,
            ObjectKind::Asn,
            ObjectKind::IpRange,
        ] {
            let mut a = app();
            set_results(&mut a, vec![result_of(kind, 7, "thing")]);
            let cmds = a.handle_event(press(KeyCode::Enter));
            match cmds.as_slice() {
                [AppCommand::LoadDetail { kind: k, id, .. }] => {
                    assert_eq!(*k, kind, "wrong kind dispatched for {kind:?}");
                    assert_eq!(*id, 7);
                }
                other => panic!("expected LoadDetail for {kind:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn new_kind_detail_renders_non_empty_body() {
        // Once a new-kind detail loads, its (tab-less) view body must be the
        // string the render path paints — non-empty and unaffected by the
        // device-only tab machinery.
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Asn,
                id: 3,
                title: "asn 64500".into(),
                body: "asn: 64500\nrir: ARIN".into(),
                tabs: Vec::new(),
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        assert_eq!(a.screen, Screen::Detail);
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_body(), "asn: 64500\nrir: ARIN");
        // No sub-resource tabs for these kinds, so a device tab key is a no-op.
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_body(), "asn: 64500\nrir: ARIN");
        // Reopening recorded it under its own kind.
        assert_eq!(a.recent[0].kind, ObjectKind::Asn);
    }

    #[test]
    fn recents_dedup_and_reopen_from_home() {
        let mut a = app();
        let load = |a: &mut App, id, title: &str| {
            detail_loaded(
                a,
                Ok(DetailView {
                    kind: ObjectKind::Device,
                    id,
                    title: title.into(),
                    body: String::new(),
                    tabs: Vec::new(),
                    links: Vec::new(),
                    header: Vec::new(),
                    summary_label: String::new(),
                    summary_rows: Vec::new(),
                }),
            );
            a.handle_event(press(KeyCode::Char('b')));
        };
        load(&mut a, 1, "device a");
        load(&mut a, 2, "device b");
        load(&mut a, 1, "device a"); // reopening 1 moves it to front, no dup
        assert_eq!(a.recent.len(), 2);
        assert_eq!(a.recent[0].id, 1);

        // No search results → Home shows recents in the Results pane; with that
        // pane focused, Enter reopens the selected one. (The home opens on the Nav
        // rail; selecting Recent there lists recents and moves focus here.)
        assert!(a.results.is_empty());
        a.focus = Focus::List;
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail { id: 1, .. }]
        ));
    }

    #[test]
    fn palette_lookup_emits_load_by_ref() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "device edge01".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Normal);
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadByRef { kind: ObjectKind::Device, value, .. }] if value == "edge01"
        ));
    }

    #[test]
    fn esc_on_home_clears_an_active_search() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        a.last_query = Some("edge".into());
        assert!(a.search_active());
        // Esc on Home with results clears the search (back to recents), not go_back.
        a.handle_event(press(KeyCode::Esc));
        assert!(a.results.is_empty(), "results cleared");
        assert_eq!(a.last_query, None, "query forgotten");
        assert!(!a.search_active());
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn d_opens_dashboard_and_load_settles_then_back() {
        let mut a = app();
        let cmds = a.handle_event(press(KeyCode::Char('D')));
        assert_eq!(a.screen, Screen::Dashboard);
        let req = match cmds.as_slice() {
            [AppCommand::LoadDashboard { req }] => *req,
            other => panic!("expected LoadDashboard, got {other:?}"),
        };
        let data = crate::netbox::dashboard::DashboardData {
            device_total: 5,
            ..Default::default()
        };
        a.handle_event(AppEvent::DashboardLoaded {
            req,
            result: Ok(data),
        });
        assert_eq!(a.dashboard.as_ref().unwrap().device_total, 5);
        assert!(!a.loading(), "the load settled the spinner");
        // `b` returns to Home.
        a.handle_event(press(KeyCode::Char('b')));
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn stale_dashboard_load_is_dropped() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('D')));
        // Reopen path bumps the generation; simulate a stale (older-id) result.
        a.dashboard_gen = 9;
        a.handle_event(AppEvent::DashboardLoaded {
            req: 3,
            result: Ok(crate::netbox::dashboard::DashboardData {
                device_total: 99,
                ..Default::default()
            }),
        });
        assert!(a.dashboard.is_none(), "a stale dashboard load is dropped");
    }

    fn tree_node(id: u64, cidr: &str, depth: u64, children: u64) -> PrefixNode {
        PrefixNode {
            id,
            prefix: cidr.into(),
            vrf: None,
            status: Some("active".into()),
            depth,
            children,
            utilization: None,
            description: String::new(),
        }
    }

    /// Deliver a prefix-tree load tagged as the current request (passes the guard).
    fn load_tree(a: &mut App, nodes: Vec<PrefixNode>) {
        let req = a.prefix_tree_gen;
        let total = nodes.len();
        a.handle_event(AppEvent::PrefixTreeLoaded {
            req,
            result: Ok(PrefixTreeData { nodes, total }),
        });
    }

    #[test]
    fn t_opens_prefix_tree_and_load_settles_then_back() {
        let mut a = app();
        let cmds = a.handle_event(press(KeyCode::Char('T')));
        assert_eq!(a.screen, Screen::PrefixTree);
        let req = match cmds.as_slice() {
            [AppCommand::LoadPrefixTree { req }] => *req,
            other => panic!("expected LoadPrefixTree, got {other:?}"),
        };
        a.handle_event(AppEvent::PrefixTreeLoaded {
            req,
            result: Ok(PrefixTreeData {
                nodes: vec![tree_node(1, "10.0.0.0/8", 0, 0)],
                total: 1,
            }),
        });
        assert_eq!(a.prefix_tree.as_ref().unwrap().nodes.len(), 1);
        assert!(!a.loading(), "the load settled the spinner");
        a.handle_event(press(KeyCode::Char('b')));
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn stale_prefix_tree_load_is_dropped() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('T')));
        a.prefix_tree_gen = 9;
        a.handle_event(AppEvent::PrefixTreeLoaded {
            req: 3,
            result: Ok(PrefixTreeData {
                nodes: vec![tree_node(1, "10.0.0.0/8", 0, 0)],
                total: 1,
            }),
        });
        assert!(
            a.prefix_tree.is_none(),
            "a stale prefix-tree load is dropped"
        );
    }

    #[test]
    fn prefix_tree_movement_clamps_within_visible() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('T')));
        load_tree(
            &mut a,
            vec![
                tree_node(1, "10.0.0.0/8", 0, 0),
                tree_node(2, "10.1.0.0/8", 0, 0),
            ],
        );
        assert_eq!(a.prefix_tree_selected, 0);
        // Up at the top stays put.
        a.handle_event(press(KeyCode::Char('k')));
        assert_eq!(a.prefix_tree_selected, 0);
        // Down moves; down again clamps at the last row.
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.prefix_tree_selected, 1);
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(
            a.prefix_tree_selected, 1,
            "can't move past the last visible row"
        );
    }

    #[test]
    fn prefix_tree_collapse_hides_children() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('T')));
        load_tree(
            &mut a,
            vec![
                tree_node(1, "10.0.0.0/8", 0, 1),
                tree_node(2, "10.0.0.0/16", 1, 1),
                tree_node(3, "10.0.0.0/24", 2, 0),
                tree_node(4, "10.1.0.0/16", 1, 0),
            ],
        );
        assert_eq!(a.tree_visible().len(), 4, "all visible initially");
        // Cursor on the /8 (id 1); Space collapses its whole subtree.
        a.handle_event(press(KeyCode::Char(' ')));
        assert_eq!(a.tree_visible().len(), 1, "the subtree is hidden");
        assert_eq!(
            a.prefix_tree_selected, 0,
            "cursor clamps onto the collapsed root"
        );
        // Right expands it again.
        a.handle_event(press(KeyCode::Right));
        assert_eq!(a.tree_visible().len(), 4, "expanded back");
    }

    #[test]
    fn enter_on_prefix_tree_opens_the_selected_prefix() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('T')));
        load_tree(&mut a, vec![tree_node(7, "10.0.0.0/8", 0, 0)]);
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(
                cmds.as_slice(),
                [AppCommand::LoadDetail {
                    kind: ObjectKind::Prefix,
                    id: 7,
                    ..
                }]
            ),
            "Enter opens the selected prefix's detail, got {cmds:?}"
        );
    }

    #[test]
    fn refresh_on_prefix_tree_reloads() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('T')));
        load_tree(&mut a, vec![tree_node(1, "10.0.0.0/8", 0, 0)]);
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(
            matches!(cmds.as_slice(), [AppCommand::LoadPrefixTree { .. }]),
            "r refreshes the tree, got {cmds:?}"
        );
    }

    fn detail_with_links(kind: ObjectKind, id: u64, links: Vec<ObjectLink>) -> DetailView {
        DetailView {
            kind,
            id,
            title: format!("{} {id}", kind.as_str()),
            body: String::new(),
            tabs: Vec::new(),
            links,
            header: Vec::new(),
            summary_label: String::new(),
            summary_rows: Vec::new(),
        }
    }

    #[test]
    fn r_opens_related_modal_and_enter_jumps_with_back_stack() {
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(detail_with_links(
                ObjectKind::Device,
                1,
                vec![
                    ObjectLink {
                        kind: ObjectKind::Site,
                        id: 5,
                        relation: "site".into(),
                        label: "iad1".into(),
                    },
                    ObjectLink {
                        kind: ObjectKind::Rack,
                        id: 7,
                        relation: "rack".into(),
                        label: "R1".into(),
                    },
                ],
            )),
        );
        assert_eq!(a.screen, Screen::Detail);

        // `R` opens the related-objects modal.
        a.handle_event(press(KeyCode::Char('R')));
        assert!(matches!(a.modal, Some(Modal::Related(_))));

        // Move to the rack and Enter: jump to it, push the device onto the stack.
        a.handle_event(press(KeyCode::Down));
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(
                cmds.as_slice(),
                [AppCommand::LoadDetail {
                    kind: ObjectKind::Rack,
                    id: 7,
                    ..
                }]
            ),
            "Enter jumps to the selected (rack) object, got {cmds:?}"
        );
        assert!(a.modal.is_none(), "the modal closes on jump");
        assert_eq!(
            a.detail_nav,
            vec![(ObjectKind::Device, 1)],
            "the object jumped from is on the back-stack"
        );

        // The rack detail lands; the stack is preserved while on it.
        detail_loaded(
            &mut a,
            Ok(detail_with_links(ObjectKind::Rack, 7, Vec::new())),
        );
        assert_eq!(a.detail_nav, vec![(ObjectKind::Device, 1)]);

        // `b` walks back to the device and pops the stack.
        let back = a.handle_event(press(KeyCode::Char('b')));
        assert!(
            matches!(
                back.as_slice(),
                [AppCommand::LoadDetail {
                    kind: ObjectKind::Device,
                    id: 1,
                    ..
                }]
            ),
            "b reloads the previous object, got {back:?}"
        );
        assert!(a.detail_nav.is_empty(), "the back-stack is popped");
    }

    #[test]
    fn r_with_no_related_objects_is_a_noop() {
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(detail_with_links(ObjectKind::Asn, 3, Vec::new())),
        );
        a.handle_event(press(KeyCode::Char('R')));
        assert!(
            a.modal.is_none(),
            "R opens nothing when there are no relations"
        );
    }

    #[test]
    fn opening_a_detail_from_home_clears_the_back_stack() {
        let mut a = app();
        a.detail_nav.push((ObjectKind::Site, 99)); // a stale path from before
        detail_loaded(
            &mut a,
            Ok(detail_with_links(ObjectKind::Device, 1, Vec::new())),
        );
        assert!(
            a.detail_nav.is_empty(),
            "a fresh detail from a non-detail screen starts a new drill path"
        );
    }

    #[test]
    fn palette_clear_search_resets_results() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        a.last_query = Some("edge".into());
        a.apply_palette(PaletteCommand::ClearSearch);
        assert!(a.results.is_empty());
        assert_eq!(a.last_query, None);
    }

    #[test]
    fn palette_filter_sets_scopes_exclusively_and_reruns_query() {
        let mut a = app();
        a.last_query = Some("edge".into());
        // Set two filters → re-runs the last query carrying them.
        let cmds = a.apply_palette(PaletteCommand::Filter(vec![
            ("status".into(), "active".into()),
            ("site".into(), "dc1".into()),
        ]));
        assert_eq!(a.filters.status.as_deref(), Some("active"));
        assert_eq!(a.filters.site.as_deref(), Some("dc1"));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::Search { query, filters, .. }]
                if query == "edge" && filters.site.as_deref() == Some("dc1")
        ));
        // Scope is mutually exclusive: setting region clears the site.
        a.apply_palette(PaletteCommand::Filter(vec![(
            "region".into(),
            "us-east".into(),
        )]));
        assert_eq!(a.filters.region.as_deref(), Some("us-east"));
        assert_eq!(
            a.filters.site, None,
            "setting a scope clears the sibling scopes"
        );
        // An empty value clears just that key.
        a.apply_palette(PaletteCommand::Filter(vec![(
            "status".into(),
            String::new(),
        )]));
        assert_eq!(a.filters.status, None);
        assert_eq!(a.filters.region.as_deref(), Some("us-east"));
        // ClearFilters wipes everything.
        a.apply_palette(PaletteCommand::ClearFilters);
        assert_eq!(a.filters.region, None);
        assert_eq!(a.filters_summary(), "");
    }

    #[test]
    fn palette_theme_changes_theme_in_place() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char(':')));
        for c in "theme nord".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty());
        assert_eq!(a.theme.name(), "nord");
        assert_eq!(a.status, "theme: nord");
        assert_eq!(a.status_ttl, Some(TRANSIENT_STATUS_TICKS));
    }

    #[test]
    fn palette_theme_respects_no_color_and_cannot_reenable_color() {
        // M1: the palette `:theme <name>` path must honour NO_COLOR the same way
        // the `t` cycle does — it cannot re-enable color, and so cannot persist a
        // colored theme on exit (the persist guard keys off theme.name()).
        let mut a = app();
        a.set_no_color();
        assert!(a.theme.is_no_color());

        a.handle_event(press(KeyCode::Char(':')));
        for c in "theme nord".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty());
        assert!(
            a.theme.is_no_color(),
            "still no-color: :theme can't override NO_COLOR"
        );
        assert_eq!(a.theme.name(), "no_color");
        // The exit-time persist guard (theme.name() != initial_theme) stays a
        // no-op, so no colored theme can be written back under NO_COLOR.
        assert_eq!(a.theme.name(), a.initial_theme);
        assert_eq!(a.status, "NO_COLOR is set — theme change disabled");

        // Control: without NO_COLOR the palette path still changes the theme.
        let mut b = app();
        b.handle_event(press(KeyCode::Char(':')));
        for c in "theme nord".chars() {
            b.handle_event(press(KeyCode::Char(c)));
        }
        b.handle_event(press(KeyCode::Enter));
        assert_eq!(
            b.theme.name(),
            "nord",
            "palette theme works without NO_COLOR"
        );
    }

    #[test]
    fn o_and_y_emit_open_and_copy() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        let open = a.handle_event(press(KeyCode::Char('o')));
        assert!(
            matches!(open.as_slice(), [AppCommand::OpenBrowser { url, .. }] if url == "http://nb/dcim/devices/1/")
        );
        let copy = a.handle_event(press(KeyCode::Char('y')));
        assert!(matches!(copy.as_slice(), [AppCommand::Copy(v)] if v == "edge01"));
    }

    #[test]
    fn detail_o_and_y_target_the_loaded_detail_not_the_home_selection() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "edge01")]);
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 99,
                title: "device edge99".into(),
                body: String::new(),
                tabs: Vec::new(),
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        assert_eq!(a.screen, Screen::Detail);

        let open = a.handle_event(press(KeyCode::Char('o')));
        assert!(
            matches!(open.as_slice(), [AppCommand::OpenBrowser { url, .. }] if url == "http://localhost/dcim/devices/99/")
        );
        let copy = a.handle_event(press(KeyCode::Char('y')));
        assert!(matches!(copy.as_slice(), [AppCommand::Copy(v)] if v == "device edge99"));
    }

    #[test]
    fn detail_web_url_preserves_netbox_base_subpaths() {
        assert_eq!(
            object_web_url("https://nb.example/netbox", ObjectKind::Vm, 7),
            "https://nb.example/netbox/virtualization/virtual-machines/7/"
        );
        assert_eq!(
            object_web_url("not a url", ObjectKind::IpRange, 3),
            "not a url/ipam/ip-ranges/3/"
        );
    }

    #[test]
    fn ctrl_w_deletes_last_word() {
        // Word-delete is delegated to the cheese-backed input, but it must still
        // work through the pure search handler: type a two-word query, Ctrl+W
        // eats the trailing word (and its space).
        let mut a = app();
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge router".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        // The trailing word is gone; the separating space remains, matching the
        // previous trim-last-word behavior.
        assert_eq!(a.search_input.value(), "edge ");
    }

    #[test]
    fn theme_cycles() {
        let mut a = app();
        let before = a.theme_index;
        a.handle_event(press(KeyCode::Char('t')));
        assert_ne!(a.theme_index, before);
        assert_eq!(a.status, format!("theme: {}", a.theme.name()));
        assert_eq!(a.status_ttl, Some(TRANSIENT_STATUS_TICKS));
    }

    #[test]
    fn transient_theme_status_clears_on_preview_ticks() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('t')));
        assert_eq!(a.status, format!("theme: {}", a.theme.name()));

        for _ in 0..TRANSIENT_STATUS_TICKS - 1 {
            a.handle_event(AppEvent::PreviewTick);
            assert!(
                !a.status.is_empty(),
                "theme status should survive until its TTL expires"
            );
        }

        a.handle_event(AppEvent::PreviewTick);
        assert!(a.status.is_empty());
        assert_eq!(a.status_severity, Severity::Info);
        assert_eq!(a.status_ttl, None);
    }

    #[test]
    fn sticky_status_replaces_and_cancels_transient_status() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('t')));
        assert!(a.status_ttl.is_some());

        detail_loaded(&mut a, Err(anyhow::anyhow!("boom")));

        assert_eq!(a.status, "error: boom");
        assert_eq!(a.status_severity, Severity::Error);
        assert_eq!(a.status_ttl, None);
        for _ in 0..TRANSIENT_STATUS_TICKS {
            a.handle_event(AppEvent::PreviewTick);
        }
        assert_eq!(a.status, "error: boom");
    }

    #[test]
    fn set_no_color_switches_to_monochrome_theme_and_pins_initial() {
        let mut a = app();
        assert!(!a.theme.is_no_color());
        a.set_no_color();
        assert!(a.theme.is_no_color());
        assert_eq!(a.theme.name(), "no_color");
        // initial_theme is pinned to the same sentinel so the exit-time persist
        // guard (theme.name() != initial_theme) stays a no-op: NO_COLOR must not
        // overwrite the user's configured theme.
        assert_eq!(a.initial_theme, a.theme.name());
    }

    #[test]
    fn cycle_theme_is_disabled_under_no_color() {
        let mut a = app();
        a.set_no_color();
        assert!(a.theme.is_no_color());
        let name_before = a.theme.name().to_string();

        a.cycle_theme();

        // The no-color theme stays put (no swap to a colored theme) and a status
        // explains why, instead of overriding the user's NO_COLOR request.
        assert!(a.theme.is_no_color(), "still no-color after cycle");
        assert_eq!(a.theme.name(), name_before);
        assert_eq!(a.status, "NO_COLOR is set — theme change disabled");

        // Control: a normal (colored) theme still cycles to a different one.
        let mut b = app();
        let before = b.theme.name().to_string();
        b.cycle_theme();
        assert_ne!(b.theme.name(), before, "a colored theme cycles normally");
    }

    #[test]
    fn tick_refreshes_last_query_only_when_idle_on_home() {
        let mut a = app();
        // No last query → tick does nothing.
        assert!(a.handle_event(AppEvent::Tick).is_empty());

        a.last_query = Some("edge".into());
        let cmds = a.handle_event(AppEvent::Tick);
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "edge"));

        // While typing a search, a tick must not fire a refresh.
        a.mode = Mode::Search;
        assert!(a.handle_event(AppEvent::Tick).is_empty());
    }

    #[test]
    fn refresh_preserves_selection_by_id() {
        let mut a = app();
        set_results(&mut a, vec![result(1, "a"), result(2, "b"), result(3, "c")]);
        a.selected = 2; // select id=3
        a.last_query = Some("x".into());

        let _ = a.handle_event(AppEvent::Tick); // captures pending_reselect = (Device, 3)
        // New results arrive in a different order; cursor should track id=3.
        set_results(&mut a, vec![result(3, "c"), result(1, "a"), result(2, "b")]);
        assert_eq!(a.selected_result().map(|r| r.id), Some(3));
    }

    #[test]
    fn r_on_home_with_results_dispatches_refresh_search_preserving_selection() {
        // `r` re-runs the active search through the same Search dispatch the
        // auto-refresh tick uses, capturing the cursor so it's preserved when
        // the (possibly reordered) results land.
        let mut a = app();
        set_results(&mut a, vec![result(1, "a"), result(2, "b"), result(3, "c")]);
        a.last_query = Some("edge".into());
        a.selected = 2; // id = 3

        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { query: q, .. }] if q == "edge"));
        assert!(a.loading(), "refresh is a tracked fetch");
        assert!(a.status.contains("refreshing"));
        // Results return reordered; the cursor still tracks id=3.
        set_results(&mut a, vec![result(3, "c"), result(1, "a"), result(2, "b")]);
        assert_eq!(a.selected_result().map(|r| r.id), Some(3));
    }

    #[test]
    fn r_on_detail_dispatches_a_reload_of_the_current_object() {
        // `r` on the detail screen reloads the open object through the same
        // LoadDetail dispatch Enter uses, so the spinner/status flow is shared.
        let mut a = app();
        set_results(&mut a, vec![result(7, "edge07")]);
        a.handle_event(press(KeyCode::Enter));
        detail_loaded(&mut a, Ok(preview_view(7, "body")));
        assert_eq!(a.screen, Screen::Detail);

        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadDetail {
                kind: ObjectKind::Device,
                id: 7,
                ..
            }]
        ));
        assert!(a.loading(), "a detail reload is a tracked fetch");
        assert!(a.status.contains("refreshing"));
    }

    #[test]
    fn r_on_empty_home_is_a_safe_noop() {
        // Nothing searched yet: `r` must not fire a stray fetch, only leave a
        // gentle status.
        let mut a = app();
        assert!(a.last_query.is_none());
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(cmds.is_empty(), "no query → no fetch");
        assert!(!a.loading());
        assert_eq!(a.status_severity, Severity::Warning);
        assert!(a.status.contains("nothing to refresh"));
    }

    fn results_n(n: u64) -> Vec<SearchResult> {
        (1..=n).map(|i| result(i, &format!("dev{i}"))).collect()
    }

    #[test]
    fn j_k_clamp_at_both_ends() {
        let mut a = app();
        set_results(&mut a, results_n(3));
        // k at the top stays at 0 (no underflow).
        a.handle_event(press(KeyCode::Up));
        assert_eq!(a.selected, 0);
        // j walks down and stops at the last row.
        for _ in 0..10 {
            a.handle_event(press(KeyCode::Down));
        }
        assert_eq!(a.selected, 2);
        // k walks back up to the top.
        for _ in 0..10 {
            a.handle_event(press(KeyCode::Up));
        }
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn g_and_capital_g_jump_to_first_and_last() {
        let mut a = app();
        set_results(&mut a, results_n(50));
        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.selected, 49);
        a.handle_event(press(KeyCode::Char('g')));
        assert_eq!(a.selected, 0);
        // Home/End are aliases for g/G.
        a.handle_event(press(KeyCode::End));
        assert_eq!(a.selected, 49);
        a.handle_event(press(KeyCode::Home));
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_down_and_up_jump_and_clamp() {
        // Updated for the viewport-aware page jump: with a stashed list viewport
        // of 21 the step is one screenful minus a row of overlap = 20 (was the
        // old fixed PAGE_JUMP of 10). The jump/clamp behaviour is otherwise
        // unchanged — this test now exercises the live-viewport math, not a
        // hard-coded constant.
        let mut a = app();
        set_results(&mut a, results_n(50));
        a.sync_list_viewport(21); // page = 21 - 1 = 20
        let page = 20;
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, page); // 0 → 20
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, page * 2); // 20 → 40 (still room before the last row)
        // PgUp steps back by a page (40 → 20).
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, page);
        // PgUp clamps at the top without underflow.
        a.handle_event(press(KeyCode::PageUp));
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
        // PgDn clamps at the last row.
        for _ in 0..20 {
            a.handle_event(press(KeyCode::PageDown));
        }
        assert_eq!(a.selected, 49);
    }

    #[test]
    fn page_jump_uses_live_list_viewport() {
        // A tall viewport pages by ~one screenful (viewport - 1 of overlap).
        let mut a = app();
        set_results(&mut a, results_n(100));
        a.sync_list_viewport(20); // page = 19
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, 19);
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);

        // A short viewport pages by a correspondingly smaller step.
        let mut b = app();
        set_results(&mut b, results_n(100));
        b.sync_list_viewport(5); // page = 4
        b.handle_event(press(KeyCode::PageDown));
        assert_eq!(b.selected, 4);
        b.handle_event(press(KeyCode::PageUp));
        assert_eq!(b.selected, 0);
    }

    #[test]
    fn page_jump_falls_back_before_first_render() {
        // Before any render the viewport is 0, so the pre-render fallback applies.
        let mut a = app();
        set_results(&mut a, results_n(50));
        assert_eq!(a.list_viewport, 0);
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, PAGE_JUMP_FALLBACK);
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_jump_minimum_one_row_on_tiny_viewport() {
        // A 1-row viewport still advances at least one row (never stalls).
        let mut a = app();
        set_results(&mut a, results_n(10));
        a.sync_list_viewport(1); // page = max(0, 1) = 1
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, 1);
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_jump_clamps_at_both_ends_with_viewport() {
        let mut a = app();
        set_results(&mut a, results_n(50));
        a.sync_list_viewport(11); // page = 10
        // PgDn clamps at the last row no matter how many pages we ask for.
        for _ in 0..20 {
            a.handle_event(press(KeyCode::PageDown));
        }
        assert_eq!(a.selected, 49);
        // PgUp clamps at the top.
        for _ in 0..20 {
            a.handle_event(press(KeyCode::PageUp));
        }
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn page_jump_empty_list_is_a_noop() {
        let mut a = app();
        a.sync_list_viewport(20);
        a.handle_event(press(KeyCode::PageDown));
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.selected, 0);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), None);
    }

    #[test]
    fn selection_past_viewport_stays_valid_and_syncs_table_state() {
        // Walk the cursor well past any plausible viewport height; the index
        // must stay in range and the stateful selection must track it so
        // ratatui scrolls the offset to keep the row visible.
        let mut a = app();
        set_results(&mut a, results_n(100));
        for _ in 0..40 {
            a.handle_event(press(KeyCode::Down));
        }
        assert_eq!(a.selected, 40);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), Some(40));
        // And the selected row resolves to a real result.
        assert_eq!(a.selected_result().map(|r| r.id), Some(41));
    }

    #[test]
    fn empty_list_movement_is_a_noop_and_selects_nothing() {
        let mut a = app();
        // No results, no recents: every movement key is harmless.
        for key in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Char('G'),
            KeyCode::Char('g'),
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            a.handle_event(press(key));
        }
        assert_eq!(a.selected, 0);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), None);
    }

    #[test]
    fn single_item_list_does_not_panic_and_selects_row_zero() {
        let mut a = app();
        set_results(&mut a, results_n(1));
        a.handle_event(press(KeyCode::Down));
        a.handle_event(press(KeyCode::Char('G')));
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.selected, 0);
        a.sync_table_state();
        assert_eq!(a.table_state.selected(), Some(0));
    }

    #[test]
    fn sync_clamps_stale_selection_into_range() {
        // If the list shrinks under a stale cursor, sync must clamp rather than
        // hand ratatui an out-of-range selection.
        let mut a = app();
        set_results(&mut a, results_n(10));
        a.selected = 8;
        set_results(&mut a, results_n(3)); // SearchComplete resets selected to 0…
        a.selected = 9; // …but simulate a stale index anyway.
        a.sync_table_state();
        assert_eq!(a.selected, 2);
        assert_eq!(a.table_state.selected(), Some(2));
    }

    /// Load a tab-less detail whose body is `n` lines, then stash a viewport so
    /// the scroll handler can clamp at the bottom.
    fn detail_with_body_lines(a: &mut App, n: usize, viewport: u16) {
        let body = (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        detail_loaded(
            a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body,
                tabs: Vec::new(),
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        a.sync_detail_viewport(viewport);
    }

    #[test]
    fn detail_scroll_restores_on_re_entry() {
        let mut a = app();
        // 30 lines in a 10-row pane → scrollable.
        detail_with_body_lines(&mut a, 30, 10);
        for _ in 0..5 {
            a.handle_event(press(KeyCode::Char('j')));
        }
        let scrolled = a.detail_scroll;
        assert!(scrolled > 0, "scrolled down");
        // Back to Home (snapshots the detail's scroll), then re-open the same object.
        a.handle_event(press(KeyCode::Char('b')));
        assert_eq!(a.screen, Screen::Home);
        detail_with_body_lines(&mut a, 30, 10);
        assert_eq!(
            a.detail_scroll, scrolled,
            "the per-object scroll is restored on re-entry"
        );
    }

    #[test]
    fn detail_scroll_down_and_up_clamp_at_both_ends() {
        let mut a = app();
        // 30 lines of content in a 10-row pane → max offset is 20.
        detail_with_body_lines(&mut a, 30, 10);
        assert_eq!(a.detail_scroll, 0);

        // Up at the top is a no-op (no underflow).
        a.handle_event(press(KeyCode::Up));
        assert_eq!(a.detail_scroll, 0);
        a.handle_event(press(KeyCode::Char('k')));
        assert_eq!(a.detail_scroll, 0);

        // j / Down scroll one line each.
        a.handle_event(press(KeyCode::Char('j')));
        a.handle_event(press(KeyCode::Down));
        assert_eq!(a.detail_scroll, 2);

        // Walk past the bottom; the offset clamps at content - viewport = 20.
        for _ in 0..50 {
            a.handle_event(press(KeyCode::Char('j')));
        }
        assert_eq!(a.detail_scroll, 20);

        // k walks back up and clamps at the top.
        for _ in 0..50 {
            a.handle_event(press(KeyCode::Char('k')));
        }
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_page_jump_and_clamp() {
        let mut a = app();
        // 100 lines in a 10-row pane → a page is viewport-1 = 9, max offset 90.
        detail_with_body_lines(&mut a, 100, 10);

        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.detail_scroll, 9);
        a.handle_event(press(KeyCode::PageDown));
        assert_eq!(a.detail_scroll, 18);

        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.detail_scroll, 9);

        // PgUp clamps at the top.
        a.handle_event(press(KeyCode::PageUp));
        a.handle_event(press(KeyCode::PageUp));
        assert_eq!(a.detail_scroll, 0);

        // PgDn clamps at the bottom (max = 90).
        for _ in 0..50 {
            a.handle_event(press(KeyCode::PageDown));
        }
        assert_eq!(a.detail_scroll, 90);
    }

    #[test]
    fn detail_g_and_capital_g_jump_to_top_and_bottom() {
        let mut a = app();
        detail_with_body_lines(&mut a, 50, 10); // max offset = 40

        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.detail_scroll, 40);
        a.handle_event(press(KeyCode::Char('g')));
        assert_eq!(a.detail_scroll, 0);

        // Home / End are aliases for g / G.
        a.handle_event(press(KeyCode::End));
        assert_eq!(a.detail_scroll, 40);
        a.handle_event(press(KeyCode::Home));
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_short_content_scrolling_is_a_noop() {
        let mut a = app();
        // 5 lines comfortably fit a 20-row pane → nothing scrolls.
        detail_with_body_lines(&mut a, 5, 20);
        for key in [
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('G'),
            KeyCode::End,
            KeyCode::PageDown,
        ] {
            a.handle_event(press(key));
            assert_eq!(
                a.detail_scroll, 0,
                "key {key:?} must not scroll short content"
            );
        }
    }

    #[test]
    fn detail_empty_body_is_safe() {
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: String::new(),
                tabs: Vec::new(),
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        a.sync_detail_viewport(10);
        // Every scroll key is a harmless no-op on an empty body.
        for key in [
            KeyCode::Char('j'),
            KeyCode::Char('G'),
            KeyCode::PageDown,
            KeyCode::Char('k'),
        ] {
            a.handle_event(press(key));
        }
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_clamps_when_viewport_unknown() {
        // Before the first render the viewport is 0, so max scroll equals the
        // full content length — but we must never panic or overflow.
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "t".into(),
                body: "a\nb\nc".into(),
                tabs: Vec::new(),
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        assert_eq!(a.detail_viewport, 0);
        for _ in 0..10 {
            a.handle_event(press(KeyCode::Char('j')));
        }
        // Content is 3 lines; with a 0 viewport max scroll is 3.
        assert_eq!(a.detail_scroll, 3);
    }

    #[test]
    fn detail_scroll_resets_on_navigation_to_new_object() {
        let mut a = app();
        detail_with_body_lines(&mut a, 50, 10);
        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.detail_scroll, 40);

        // Opening a different object resets the offset to the top.
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 2,
                title: "device edge02".into(),
                body: "fresh".into(),
                tabs: Vec::new(),
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_resets_on_tab_switch() {
        use crate::domain::detail::DetailTab;
        let long = |label: &str| {
            (0..40)
                .map(|i| format!("{label} {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: long("summary"),
                tabs: vec![DetailTab {
                    key: 'i',
                    label: "interfaces".into(),
                    body: long("iface"),
                    rows: Vec::new(),
                }],
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        a.sync_detail_viewport(10);
        a.handle_event(press(KeyCode::Char('G')));
        assert!(a.detail_scroll > 0);

        // Switching to the interfaces tab starts at the top.
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 1);
        assert_eq!(a.detail_scroll, 0);

        // Scroll within the tab, then toggle back to summary — also top.
        a.sync_detail_viewport(10);
        a.handle_event(press(KeyCode::Char('G')));
        assert!(a.detail_scroll > 0);
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_scroll, 0);
    }

    #[test]
    fn detail_scroll_keys_do_not_collide_with_device_tab_keys() {
        use crate::domain::detail::DetailTab;
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: (0..40)
                    .map(|i| format!("s {i}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                tabs: vec![DetailTab {
                    key: 'i',
                    label: "interfaces".into(),
                    body: "iface body".into(),
                    rows: Vec::new(),
                }],
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        a.sync_detail_viewport(10);
        // The tab key still switches tabs (and resets scroll)…
        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 1);
        // …while j/k scroll without disturbing the active tab.
        a.sync_detail_viewport(10);
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.detail_tab, 1);
        assert_eq!(a.detail_scroll, 0); // 10-line body fits a 10-row pane → no-op
    }

    #[test]
    fn home_movement_keys_still_move_selection_not_scroll() {
        // Guarding the detail-scroll arms on Screen::Detail must not change the
        // home list behaviour: j/G there still move the selection cursor.
        let mut a = app();
        set_results(&mut a, results_n(20));
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.selected, 1);
        assert_eq!(a.detail_scroll, 0);
        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.selected, 19);
    }

    #[test]
    fn device_tabs_select_and_toggle() {
        use crate::domain::detail::DetailTab;
        let tab = |key, label: &str| DetailTab {
            key,
            label: label.into(),
            body: format!("{label} body"),
            rows: Vec::new(),
        };
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: "summary".into(),
                tabs: vec![tab('i', "interfaces"), tab('p', "ips"), tab('v', "vlans")],
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        assert_eq!(a.screen, Screen::Detail);
        assert_eq!(a.detail_tab, 0);
        assert_eq!(a.detail_body(), "summary");

        a.handle_event(press(KeyCode::Char('i')));
        assert_eq!(a.detail_tab, 1);
        assert_eq!(a.detail_body(), "interfaces body");

        a.handle_event(press(KeyCode::Char('v')));
        assert_eq!(a.detail_tab, 3);
        assert_eq!(a.detail_body(), "vlans body");

        // Pressing the active tab's key again returns to the summary.
        a.handle_event(press(KeyCode::Char('v')));
        assert_eq!(a.detail_tab, 0);

        // A tab key with no matching section is a no-op (no cables here).
        a.handle_event(press(KeyCode::Char('c')));
        assert_eq!(a.detail_tab, 0);
    }

    #[test]
    fn tab_cycles_detail_sections_including_summary() {
        use crate::domain::detail::DetailTab;
        let tab = |key, label: &str| DetailTab {
            key,
            label: label.into(),
            body: format!("{label} body"),
            rows: Vec::new(),
        };
        let mut a = app();
        detail_loaded(
            &mut a,
            Ok(DetailView {
                kind: ObjectKind::Device,
                id: 1,
                title: "device edge01".into(),
                body: "summary".into(),
                tabs: vec![tab('i', "interfaces"), tab('p', "ips")],
                links: Vec::new(),
                header: Vec::new(),
                summary_label: String::new(),
                summary_rows: Vec::new(),
            }),
        );
        assert_eq!(a.detail_tab, 0, "starts on summary");
        // Tab cycles summary(0) → i(1) → p(2) → wraps back to summary.
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.detail_tab, 1);
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.detail_tab, 2);
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.detail_tab, 0, "wraps back to summary");
        // Shift-Tab steps backward (to the last section).
        a.handle_event(press(KeyCode::BackTab));
        assert_eq!(a.detail_tab, 2);
    }

    // --- Home split: focus + preview ----------------------------------------

    /// Drive the debounce flush: the preview load only fires on a `PreviewTick`,
    /// never per keystroke.
    fn preview_tick(a: &mut App) -> Vec<AppCommand> {
        a.handle_event(AppEvent::PreviewTick)
    }

    fn preview_view(id: u64, body: &str) -> DetailView {
        DetailView {
            kind: ObjectKind::Device,
            id,
            title: format!("device {id}"),
            body: body.into(),
            tabs: Vec::new(),
            links: Vec::new(),
            header: Vec::new(),
            summary_label: String::new(),
            summary_rows: Vec::new(),
        }
    }

    #[test]
    fn tab_and_backtab_cycle_focus() {
        let mut a = app();
        // The home opens on the Browse (Nav) rail.
        assert_eq!(a.focus, Focus::Nav);
        set_results(&mut a, results_n(3));
        // A search moved focus to the results; from there Tab cycles left→right
        // across the three panes (Nav → List → Preview), wrapping.
        a.focus = Focus::Nav;
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::List);
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Preview);
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Nav);
        // Shift+Tab (BackTab) cycles in reverse.
        a.handle_event(press(KeyCode::BackTab));
        assert_eq!(a.focus, Focus::Preview);
        a.handle_event(press(KeyCode::BackTab));
        assert_eq!(a.focus, Focus::List);
        a.handle_event(press(KeyCode::BackTab));
        assert_eq!(a.focus, Focus::Nav);
    }

    #[test]
    fn nav_jk_moves_the_section_cursor_clamped() {
        let mut a = app();
        a.focus = Focus::Nav;
        a.nav_selected = 0;
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.nav_selected, 1);
        a.handle_event(press(KeyCode::Char('k')));
        assert_eq!(a.nav_selected, 0);
        // Clamp at the top.
        a.handle_event(press(KeyCode::Char('k')));
        assert_eq!(a.nav_selected, 0);
        // `G` jumps to the last section (Recent).
        a.handle_event(press(KeyCode::Char('G')));
        assert_eq!(a.nav_selected, NAV_SECTIONS.len() - 1);
    }

    #[test]
    fn nav_enter_on_a_kind_browses_and_focuses_results() {
        let mut a = app();
        a.focus = Focus::Nav;
        a.nav_selected = 0; // Devices
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(
                cmds.as_slice(),
                [AppCommand::Browse {
                    kind: ObjectKind::Device,
                    ..
                }]
            ),
            "got: {cmds:?}"
        );
        assert_eq!(a.browse_kind, Some(ObjectKind::Device));
        assert_eq!(a.focus, Focus::List);
        // Browsing a kind records it for the exit-time persist.
        assert_eq!(a.last_browsed, Some(ObjectKind::Device));
    }

    #[test]
    fn with_last_browsed_restores_cursor_and_primes_startup_browse() {
        let a = app().with_last_browsed(Some("vrf".to_string()));
        let vrf_idx = NAV_SECTIONS
            .iter()
            .position(|s| *s == NavSection::Vrfs)
            .unwrap();
        assert_eq!(a.nav_selected, vrf_idx);
        assert_eq!(a.browse_kind, Some(ObjectKind::Vrf));
        assert_eq!(a.startup_browse(), Some(ObjectKind::Vrf));
        assert_eq!(a.last_browsed, Some(ObjectKind::Vrf));
        // initial pin == loaded value, so a no-op session won't rewrite the key.
        assert_eq!(a.initial_last_browsed.as_deref(), Some("vrf"));
    }

    #[test]
    fn with_last_browsed_none_or_unknown_keeps_recent_default() {
        let recent = NAV_SECTIONS.len() - 1;

        let none = app().with_last_browsed(None);
        assert_eq!(none.nav_selected, recent);
        assert!(none.browse_kind.is_none());
        assert!(none.startup_browse().is_none());

        // An unknown/stale slug doesn't restore a browse (cursor stays on Recent),
        // but is still pinned as `initial` so exit clears it rather than thrashing.
        let bogus = app().with_last_browsed(Some("nonsense".to_string()));
        assert_eq!(bogus.nav_selected, recent);
        assert!(bogus.browse_kind.is_none());
        assert_eq!(bogus.initial_last_browsed.as_deref(), Some("nonsense"));
    }

    #[test]
    fn nav_enter_on_recent_clears_browse_and_shows_recents() {
        let mut a = app();
        set_results(&mut a, results_n(3));
        a.browse_kind = Some(ObjectKind::Device);
        a.focus = Focus::Nav;
        a.nav_selected = NAV_SECTIONS.len() - 1; // Recent
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty());
        assert_eq!(a.browse_kind, None);
        assert!(a.view.is_empty());
        assert_eq!(a.focus, Focus::List);
    }

    #[test]
    fn browse_complete_populates_results_and_titles_them() {
        let mut a = app();
        a.browse_gen = 1; // a browse is in flight
        a.handle_event(AppEvent::BrowseComplete {
            req: 1,
            kind: ObjectKind::Rack,
            result: Ok(vec![result_of(ObjectKind::Rack, 5, "ci-rack-1")]),
        });
        assert_eq!(a.browse_kind, Some(ObjectKind::Rack));
        assert_eq!(a.view.len(), 1);
        assert_eq!(a.results[0].display, "ci-rack-1");
    }

    #[test]
    fn stale_browse_result_is_dropped() {
        let mut a = app();
        a.browse_gen = 5; // newer browse already issued
        a.handle_event(AppEvent::BrowseComplete {
            req: 2, // older
            kind: ObjectKind::Device,
            result: Ok(vec![result_of(ObjectKind::Device, 1, "old")]),
        });
        assert!(a.results.is_empty(), "a superseded browse must be dropped");
    }

    #[test]
    fn nav_jk_live_browses_after_the_cursor_settles() {
        let mut a = app(); // opens focus=Nav, cursor on Recent
        // Jump the rail cursor to Devices (top). Movement only marks dirty.
        let cmds = a.handle_event(press(KeyCode::Char('g')));
        assert!(cmds.is_empty(), "movement itself dispatches nothing");
        assert_eq!(a.nav_selected, 0);

        // First tick after a move: the cursor isn't settled yet (it moved since
        // the previous tick's anchor), so the debounce defers the fetch.
        assert!(
            preview_tick(&mut a).is_empty(),
            "a just-moved cursor doesn't fetch on the same tick"
        );

        // Next tick with the cursor still: exactly one Browse for the kind, and
        // focus stays on the rail so the reader can keep scrolling.
        let cmds = preview_tick(&mut a);
        assert!(
            matches!(
                cmds.as_slice(),
                [AppCommand::Browse {
                    kind: ObjectKind::Device,
                    ..
                }]
            ),
            "a settled cursor live-browses its kind; got {cmds:?}"
        );
        assert_eq!(a.focus, Focus::Nav, "live-browse never steals focus");
        assert_eq!(a.browse_kind, Some(ObjectKind::Device));
        assert_eq!(a.last_browsed, Some(ObjectKind::Device));

        // A still cursor on an already-shown kind doesn't re-fetch.
        a.browse_dirty = true;
        assert!(
            preview_tick(&mut a).is_empty(),
            "no re-fetch while parked on the shown kind"
        );
    }

    #[test]
    fn live_browse_defers_through_a_continuous_scroll() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('g'))); // → Devices(0)
        assert!(preview_tick(&mut a).is_empty()); // moved this tick, defer
        a.handle_event(press(KeyCode::Char('j'))); // → Prefixes(1), still scrolling
        assert!(
            preview_tick(&mut a).is_empty(),
            "still moving between ticks: keep deferring, no flash of intermediate lists"
        );
        // Cursor now still: a single Browse for the final kind, not the ones passed.
        let cmds = preview_tick(&mut a);
        assert!(
            matches!(
                cmds.as_slice(),
                [AppCommand::Browse {
                    kind: ObjectKind::Prefix,
                    ..
                }]
            ),
            "one fetch for the kind the cursor stopped on; got {cmds:?}"
        );
    }

    #[test]
    fn live_browse_on_recent_clears_the_results() {
        let mut a = app();
        set_results(&mut a, results_n(3));
        a.preview_dirty = false; // ignore the preview the seeded results dirtied
        a.browse_kind = Some(ObjectKind::Device);
        a.focus = Focus::Nav;
        a.nav_selected = 0; // Devices
        a.nav_tick_anchor = 0;
        // Move to Recent (bottom) and let it settle.
        a.handle_event(press(KeyCode::Char('G')));
        assert!(preview_tick(&mut a).is_empty()); // moved this tick, defer
        let cmds = preview_tick(&mut a);
        assert!(cmds.is_empty(), "Recent fetches nothing; got {cmds:?}");
        assert_eq!(a.browse_kind, None);
        assert!(a.view.is_empty(), "Recent clears the browse results");
        assert_eq!(a.focus, Focus::Nav);

        // With the browse results gone, the home target falls back to Recent.
        // Seed a recent and confirm `home_target` resolves to it (selection 0).
        assert!(a.results.is_empty() && a.selected == 0);
        let r = result_of(ObjectKind::Vlan, 42, "vlan-42");
        a.recent.push(RecentItem {
            kind: r.kind,
            id: r.id,
            title: r.display.clone(),
        });
        assert_eq!(a.home_target(), Some((ObjectKind::Vlan, 42)));
    }

    #[test]
    fn live_browse_only_fires_on_the_nav_rail() {
        let mut a = app();
        a.handle_event(press(KeyCode::Char('g'))); // dirty the live-browse
        a.focus = Focus::List; // …but focus left the rail before it settled
        a.nav_tick_anchor = a.nav_selected; // cursor is settled
        assert!(
            preview_tick(&mut a).is_empty(),
            "off the Nav rail the live-browse stays quiet"
        );
    }

    #[test]
    fn nav_counts_event_populates_the_map() {
        let mut a = app();
        a.handle_event(AppEvent::NavCounts(vec![
            (ObjectKind::Device, 13),
            (ObjectKind::Rack, 1),
        ]));
        assert_eq!(a.nav_counts.get(&ObjectKind::Device), Some(&13));
        assert_eq!(a.nav_counts.get(&ObjectKind::Rack), Some(&1));
    }

    #[test]
    fn list_focus_moves_selection_preview_focus_scrolls_preview() {
        let mut a = app();
        set_results(&mut a, results_n(5));
        // Load a tall preview so there's something to scroll.
        let _ = preview_tick(&mut a); // dirty from SearchComplete → issues a load
        let body = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, &body)),
        });
        a.sync_preview_viewport(10);

        // List focused (default): j/k move the selection, preview unscrolled.
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.selected, 1);
        assert_eq!(a.preview_scroll, 0);

        // Move back to id=1 and let its preview reload+settle so the body is tall.
        a.handle_event(press(KeyCode::Char('k')));
        assert_eq!(a.selected, 0);
        let _ = preview_tick(&mut a);
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, &body)),
        });
        a.sync_preview_viewport(10);

        // Focus the preview: now j/k scroll its body; the selection holds still.
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Preview);
        let sel = a.selected;
        a.handle_event(press(KeyCode::Char('j')));
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.preview_scroll, 2);
        assert_eq!(
            a.selected, sel,
            "preview scroll must not move the selection"
        );
        // g/G jump to top/bottom of the preview body.
        a.handle_event(press(KeyCode::Char('G')));
        assert!(a.preview_scroll > 0);
        a.handle_event(press(KeyCode::Char('g')));
        assert_eq!(a.preview_scroll, 0);
    }

    #[test]
    fn preview_loads_only_on_settle_not_per_keystroke() {
        // A burst of cursor movement must NOT fire a preview load per key — only
        // the debounce tick flushes a single load for the settled selection.
        let mut a = app();
        set_results(&mut a, results_n(10));
        // Each j returns no command (no per-keystroke fetch).
        for _ in 0..5 {
            let cmds = a.handle_event(press(KeyCode::Char('j')));
            assert!(cmds.is_empty(), "movement must not issue a network command");
        }
        assert_eq!(a.selected, 5);
        // The tick coalesces the whole burst into one load for the settled row.
        let cmds = preview_tick(&mut a);
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::LoadPreview {
                kind: ObjectKind::Device,
                id: 6
            }]
        ));
        // A second tick with the cursor unmoved issues nothing (idempotent).
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 6,
            result: Ok(preview_view(6, "body")),
        });
        assert!(preview_tick(&mut a).is_empty());
    }

    #[test]
    fn stale_preview_response_is_dropped_matching_one_is_shown() {
        let mut a = app();
        set_results(&mut a, results_n(3)); // ids 1,2,3; selected = 0 (id 1)
        let _ = preview_tick(&mut a); // issues LoadPreview for id 1

        // A stale response for a *different* selection (id 2) must be ignored.
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 2,
            result: Ok(preview_view(2, "STALE body for id 2")),
        });
        assert_eq!(a.preview_for, None, "stale response must not be adopted");
        assert!(a.preview.is_none());

        // The matching response (id 1) is shown.
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "FRESH body for id 1")),
        });
        assert_eq!(a.preview_for, Some((ObjectKind::Device, 1)));
        assert_eq!(a.preview_body(), "FRESH body for id 1");
    }

    #[test]
    fn preview_body_falls_back_to_lightweight_peek_when_cursor_moves() {
        // After loading id 1's preview, moving to id 2 must not show id 1's body;
        // the pane falls back to the lightweight peek for the new row.
        let mut a = app();
        set_results(&mut a, results_n(3));
        let _ = preview_tick(&mut a);
        a.handle_event(AppEvent::PreviewLoaded {
            kind: ObjectKind::Device,
            id: 1,
            result: Ok(preview_view(1, "full id 1 body")),
        });
        assert_eq!(a.preview_body(), "full id 1 body");
        a.handle_event(press(KeyCode::Char('j'))); // now on id 2
        assert_ne!(a.preview_body(), "full id 1 body");
        assert!(a.preview_body().contains("dev2")); // lightweight peek of the row
    }

    #[test]
    fn empty_results_focus_and_movement_are_safe() {
        let mut a = app();
        // No results, no recents. Tab focus, scroll-in-preview, and select keys
        // are all harmless no-ops. The home opens on Nav, so two Tabs reach the
        // Preview pane (Nav → List → Preview).
        a.handle_event(press(KeyCode::Tab));
        a.handle_event(press(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Preview);
        for key in [
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Char('G'),
            KeyCode::Char('g'),
            KeyCode::PageDown,
            KeyCode::PageUp,
        ] {
            a.handle_event(press(key));
        }
        assert_eq!(a.preview_scroll, 0);
        assert_eq!(a.selected, 0);
        // A tick with no selectable target clears any preview and issues nothing.
        a.mark_preview_dirty_for_test();
        let cmds = preview_tick(&mut a);
        assert!(cmds.is_empty());
        assert!(a.preview.is_none());
        // The preview body shows a tasteful placeholder, not a panic.
        assert!(a.preview_body().contains("Nothing selected"));
    }

    #[test]
    fn preview_tick_is_inert_off_home_screen() {
        // A pending dirty flag must not fire a preview fetch while reading detail.
        let mut a = app();
        set_results(&mut a, results_n(3));
        detail_loaded(&mut a, Ok(preview_view(1, "detail")));
        assert_eq!(a.screen, Screen::Detail);
        a.mark_preview_dirty_for_test();
        assert!(preview_tick(&mut a).is_empty());
    }

    // --- Bug B: stale full-search / full-detail response suppression ----------

    /// Dispatch a search through the real key path so a request id is stamped,
    /// returning that id (the newest spawned on the search channel).
    fn dispatch_search(a: &mut App, query: &str) -> RequestId {
        a.handle_event(press(KeyCode::Char('/')));
        for c in query.chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        match cmds.as_slice() {
            [AppCommand::Search { req, .. }] => *req,
            other => panic!("expected a Search command, got {other:?}"),
        }
    }

    #[test]
    fn stale_search_complete_is_dropped_newest_wins() {
        // Two searches are spawned; the EARLIER one's result lands LAST. The
        // guard must drop it so the newer search's results aren't clobbered.
        let mut a = app();
        let first = dispatch_search(&mut a, "alpha");
        let second = dispatch_search(&mut a, "beta");
        assert!(second > first, "ids are monotonic per channel");
        assert_eq!(a.pending, 2, "both searches are in flight");

        // The NEWER search resolves first and is adopted.
        a.handle_event(AppEvent::SearchComplete {
            req: second,
            result: Ok(SearchOutcome {
                results: vec![result(2, "beta-hit")],
                errors: Vec::new(),
            }),
        });
        assert_eq!(a.selected_result().map(|r| r.id), Some(2));
        assert_eq!(a.pending, 1, "the second search settled");

        // The STALE earlier search lands afterwards: it must be dropped, leaving
        // the newer results in place — but still counted down (no hung spinner).
        a.handle_event(AppEvent::SearchComplete {
            req: first,
            result: Ok(SearchOutcome {
                results: vec![result(1, "alpha-hit")],
                errors: Vec::new(),
            }),
        });
        assert_eq!(
            a.selected_result().map(|r| r.id),
            Some(2),
            "stale search must not overwrite the newer results"
        );
        assert_eq!(a.pending, 0, "stale result still settles its fetch");
        assert!(!a.loading());
    }

    #[test]
    fn stale_detail_loaded_is_dropped_newest_wins() {
        // Two detail loads are spawned (Enter on two different rows); the EARLIER
        // one's result lands LAST and must not navigate over the newer object.
        let mut a = app();
        set_results(&mut a, vec![result(10, "ten"), result(20, "twenty")]);

        // Enter on row 0 (id 10) → first detail request.
        let first = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { id: 10, req, .. }] => *req,
            other => panic!("expected LoadDetail for id 10, got {other:?}"),
        };
        // Go back, move to row 1 (id 20), Enter → second (newer) detail request.
        a.handle_event(press(KeyCode::Char('b')));
        a.handle_event(press(KeyCode::Char('j')));
        let second = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { id: 20, req, .. }] => *req,
            other => panic!("expected LoadDetail for id 20, got {other:?}"),
        };
        assert!(second > first);
        assert_eq!(a.pending, 2);

        // The newer load (id 20) resolves first and is shown.
        a.handle_event(AppEvent::DetailLoaded {
            req: second,
            result: Ok(preview_view(20, "twenty body")),
        });
        assert_eq!(a.detail.as_ref().map(|d| d.id), Some(20));

        // The stale earlier load (id 10) lands afterwards: dropped, so it does not
        // navigate/overwrite the newer detail — but still counted down.
        a.handle_event(AppEvent::DetailLoaded {
            req: first,
            result: Ok(preview_view(10, "ten body")),
        });
        assert_eq!(
            a.detail.as_ref().map(|d| d.id),
            Some(20),
            "stale detail must not overwrite the newer object"
        );
        assert_eq!(a.pending, 0);
        assert!(!a.loading());
    }

    #[test]
    fn detail_and_search_channels_are_independent() {
        // The detail guard must not drop a search result (and vice versa): the two
        // channels keep separate high-water marks even off one monotonic source.
        let mut a = app();
        set_results(&mut a, vec![result(5, "five")]);
        // Spawn a detail load (bumps detail_gen, not search_gen).
        let detail_req = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { req, .. }] => *req,
            other => panic!("expected LoadDetail, got {other:?}"),
        };
        // A fresh search result (current on the search channel) is still adopted
        // even though detail_gen is now higher than search_gen.
        search_complete(
            &mut a,
            Ok(SearchOutcome {
                results: vec![result(6, "six")],
                errors: Vec::new(),
            }),
        );
        assert_eq!(a.selected_result().map(|r| r.id), Some(6));
        // And the detail load resolves on its own channel.
        a.handle_event(AppEvent::DetailLoaded {
            req: detail_req,
            result: Ok(preview_view(5, "five body")),
        });
        assert_eq!(a.detail.as_ref().map(|d| d.id), Some(5));
    }

    #[test]
    fn refresh_then_stale_prior_search_keeps_refreshed_results() {
        // The recents/refresh path also rides the guard: a manual refresh (`r`)
        // spawns a newer search; a slow result from before the refresh is dropped.
        let mut a = app();
        set_results(&mut a, vec![result(1, "a"), result(2, "b")]);
        a.last_query = Some("edge".into());
        // Pretend a search was in flight before the refresh.
        let stale = a.search_gen; // current high-water; the refresh will exceed it
        // Manual refresh dispatches a newer search (bumps search_gen).
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(matches!(cmds.as_slice(), [AppCommand::Search { .. }]));
        assert!(a.search_gen > stale);
        // The refreshed results land and are adopted.
        set_results(&mut a, vec![result(3, "fresh")]);
        assert_eq!(a.selected_result().map(|r| r.id), Some(3));
        // A straggler search from before the refresh is dropped.
        a.handle_event(AppEvent::SearchComplete {
            req: stale,
            result: Ok(SearchOutcome {
                results: vec![result(99, "straggler")],
                errors: Vec::new(),
            }),
        });
        assert_eq!(
            a.selected_result().map(|r| r.id),
            Some(3),
            "a pre-refresh straggler must not clobber the refreshed results"
        );
    }

    // --- Bug A: ambiguous palette IP lookup surfaces as an error -------------

    #[test]
    fn ambiguous_detail_error_surfaces_as_error_status() {
        // The palette IP path resolves through the ambiguity-aware resolver; an
        // ambiguous ref returns a typed Ambiguous error on DetailLoaded, which the
        // TUI must surface as an error status (the same path a NotFound takes),
        // never silently navigating to a guessed object.
        let mut a = app();
        let ambiguous = anyhow::Error::from(crate::error::NboxError::Ambiguous {
            noun: "IP address".into(),
            value: "10.0.0.1".into(),
            matches: "10.0.0.1/24 (vrf-a), 10.0.0.1/24 (vrf-b)".into(),
        });
        detail_loaded(&mut a, Err(ambiguous));
        assert_eq!(
            a.screen,
            Screen::Home,
            "ambiguity must not navigate to detail"
        );
        assert!(a.detail.is_none(), "no object is adopted on ambiguity");
        assert_eq!(a.status_severity, Severity::Error);
        assert!(a.status.contains("ambiguous"));
    }

    // --- Item 7: profile switcher -------------------------------------------

    /// Build an `App` whose configured profiles are `names`, with the first as the
    /// active one (matching `App::new`'s "test" → falls back to index 0 unless a
    /// name matches; here the active `profile_name` is set to `names[0]`).
    fn app_with_profiles(names: &[&str]) -> App {
        let mut a = app();
        let profiles: Vec<ProfileEntry> = names
            .iter()
            .map(|n| ProfileEntry {
                name: (*n).to_string(),
                config: ProfileConfig {
                    url: format!("http://{n}.example"),
                    ..Default::default()
                },
            })
            .collect();
        // Point the active profile (name + url) at the first entry so the fixture
        // starts with the header matching the connected instance — the invariant
        // these tests guard. set_profiles then indexes to it.
        if let Some(first) = profiles.first() {
            a.profile_name.clone_from(&first.name);
            a.base_url.clone_from(&first.config.url);
        }
        a.set_profiles(profiles);
        a
    }

    /// Deliver a successful profile-switch result tagged with the current pending
    /// switch's id + name (passes the latest-switch-wins guard), with the given
    /// version.
    fn profile_switched_ok(a: &mut App, version: &str) {
        let name = a.pending_profile.clone().expect("a switch must be pending");
        let id = a.pending_switch.expect("a switch must be pending");
        let profile = ProfileConfig {
            url: "http://x".into(),
            ..Default::default()
        };
        let client = NetBoxClient::new(&profile, None).unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            id,
            name,
            result: Ok((client, version.to_string())),
        });
    }

    #[test]
    fn cycle_profile_advances_and_wraps_forward() {
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.profile_name, "alpha");

        // P dispatches a reconnect to the next profile but does NOT flip the
        // header — that waits for success, so the UI never lies about the client.
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "beta"
        ));
        assert_eq!(a.profile_index, 0, "header stays on the connected profile");
        assert_eq!(a.profile_name, "alpha", "header doesn't flip until success");
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        assert!(a.loading(), "a reconnect is a tracked fetch");

        // The switch settles → the header flips atomically to the new profile.
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 1);
        assert_eq!(a.profile_name, "beta");
        assert_eq!(a.base_url, "http://beta.example");

        // Continue forward to the last, then wrap to the first (each settles).
        a.handle_event(press(KeyCode::Char('P')));
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_name, "gamma");
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "alpha"
        ));
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 0, "wraps past the end to the first");
        assert_eq!(a.profile_name, "alpha");
    }

    #[test]
    fn cycle_profile_backward_wraps_to_last() {
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        // Ctrl+P steps backward; from the first it wraps to the last.
        let cmds = a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
        // Still on the connected profile until the reconnect succeeds.
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.profile_name, "alpha");
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 2);
        assert_eq!(a.profile_name, "gamma");
    }

    #[test]
    fn cycle_profile_steps_from_pending_target_when_switch_in_flight() {
        // Rapid cycling before the first reconnect settles must advance from the
        // pending target, not the (still-connected) profile, so the user's sense
        // of "where am I cycling" matches.
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        a.handle_event(press(KeyCode::Char('P'))); // → beta (pending)
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        assert_eq!(a.profile_name, "alpha", "header still on alpha");
        let cmds = a.handle_event(press(KeyCode::Char('P'))); // step from beta → gamma
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
    }

    #[test]
    fn cycle_profile_single_profile_is_a_graceful_noop() {
        let mut a = app_with_profiles(&["only"]);
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(cmds.is_empty(), "nothing to switch to");
        assert_eq!(a.profile_name, "only");
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.status, "only one profile configured");
        assert!(!a.loading(), "a no-op switch is not a tracked fetch");
    }

    #[test]
    fn cycle_profile_with_no_profiles_is_a_noop() {
        // An app whose profile list was never populated never panics on the key.
        let mut a = app();
        assert!(a.profiles.is_empty());
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(cmds.is_empty());
        assert_eq!(a.status, "only one profile configured");
    }

    #[test]
    fn palette_profile_verb_jumps_to_named_profile() {
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        a.handle_event(press(KeyCode::Char(':')));
        for c in "profile gamma".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        // Header waits for the reconnect; the switch is pending meanwhile.
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
        assert_eq!(a.profile_name, "alpha");
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_index, 2);
        assert_eq!(a.profile_name, "gamma");
    }

    #[test]
    fn palette_profile_unknown_name_is_a_clear_error() {
        let mut a = app_with_profiles(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char(':')));
        for c in "profile nope".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty(), "an unknown profile dispatches nothing");
        assert_eq!(a.profile_name, "alpha", "active profile is unchanged");
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.status_severity, Severity::Error);
        assert!(a.status.contains("no profile named 'nope'"));
    }

    #[test]
    fn profile_switched_ok_updates_version_when_current() {
        let mut a = app_with_profiles(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('P'))); // switching to beta
        assert!(a.loading());
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        // Until it settles the header stays on the connected profile (alpha).
        assert_eq!(a.profile_name, "alpha");
        assert_eq!(a.base_url, "http://alpha.example");
        profile_switched_ok(&mut a, "4.7.0");
        // On success the whole header flips atomically to match the new client.
        assert_eq!(a.netbox_version, "4.7.0");
        assert_eq!(a.profile_name, "beta");
        assert_eq!(a.profile_index, 1);
        assert_eq!(a.base_url, "http://beta.example");
        assert!(a.pending_profile.is_none(), "the switch settled");
        assert!(!a.loading());
        assert_eq!(a.status_severity, Severity::Success);
        assert!(a.status.contains("beta"));
    }

    #[test]
    fn profile_switch_failure_leaves_no_phantom_and_keeps_ui_usable() {
        // The invariant under test: a FAILED switch is a no-op + error toast. The
        // header must still match the instance `client` is connected to (alpha),
        // never the requested-but-unreachable one — no phantom.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('P')));
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        let name = a.pending_profile.clone().unwrap();
        let id = a.pending_switch.unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            id,
            name,
            result: Err(anyhow::anyhow!("connection refused")),
        });
        // Header + url + index + version all stay on the still-connected alpha.
        assert_eq!(a.profile_name, "alpha", "header stays on the connected one");
        assert_eq!(a.profile_index, 0);
        assert_eq!(a.base_url, "http://alpha.example");
        assert_eq!(
            a.netbox_version, "4.5.5",
            "unchanged: the old client is intact"
        );
        assert!(
            a.pending_profile.is_none(),
            "the switch is no longer pending"
        );
        assert_eq!(a.status_severity, Severity::Error);
        assert!(a.status.contains("connection refused"));
        assert!(!a.loading(), "a failed switch still settles the fetch");
        // The UI is usable again on the old instance: fetches are no longer fenced.
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(cmds.as_slice(), [AppCommand::Search { query, .. }] if query == "edge"),
            "a search dispatches normally after a failed switch"
        );
    }

    #[test]
    fn switching_profile_clears_old_instance_data() {
        // Old-profile results/recents/detail must not linger after a switch — they
        // reference the instance we're leaving.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        set_results(&mut a, results_n(3));
        detail_loaded(&mut a, Ok(preview_view(1, "alpha body")));
        a.handle_event(press(KeyCode::Char('b'))); // back to home
        assert!(!a.results.is_empty());
        assert!(!a.recent.is_empty());

        a.handle_event(press(KeyCode::Char('P'))); // switch to beta (pending)
        // The old instance's data is still shown while the switch is in flight (we
        // remain connected to it); it's dropped atomically only once the new
        // instance is adopted, so a failed switch wouldn't have wiped a live UI.
        assert!(
            !a.results.is_empty(),
            "old data kept until the switch succeeds"
        );
        profile_switched_ok(&mut a, "4.5.0");
        assert!(a.results.is_empty(), "old results dropped on success");
        assert!(a.view.is_empty());
        assert!(a.recent.is_empty(), "old recents dropped");
        assert!(a.detail.is_none(), "old detail dropped");
        assert!(a.last_query.is_none());
        assert_eq!(a.screen, Screen::Home);
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn switching_profile_drops_stale_old_profile_search_response() {
        // A search dispatched against the OLD profile is still in flight when the
        // switch SUCCEEDS. After the switch lands, that old-instance result must be
        // dropped (the success path bumps the search high-water mark past it), not
        // painted onto the new instance.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        let stale = dispatch_search(&mut a, "edge"); // in flight on alpha
        assert!(a.loading());

        // Switch to beta and let it succeed: success bumps the gen past `stale`.
        a.handle_event(press(KeyCode::Char('P')));
        profile_switched_ok(&mut a, "4.5.0");
        assert!(a.search_gen > stale, "success advanced the search gen");

        // The old-profile search finally lands — it must be dropped as stale.
        a.handle_event(AppEvent::SearchComplete {
            req: stale,
            result: Ok(SearchOutcome {
                results: vec![result(99, "alpha-straggler")],
                errors: Vec::new(),
            }),
        });
        assert!(
            a.results.is_empty(),
            "a stale old-profile search must not populate the new instance"
        );
    }

    #[test]
    fn switching_profile_drops_stale_old_profile_detail_response() {
        // Same for a full-detail load in flight on the old profile across a switch.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        set_results(&mut a, vec![result(10, "ten")]);
        let stale = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::LoadDetail { req, .. }] => *req,
            other => panic!("expected LoadDetail, got {other:?}"),
        };
        a.handle_event(press(KeyCode::Char('P'))); // switch to beta
        profile_switched_ok(&mut a, "4.5.0"); // success bumps the detail gen
        assert!(a.detail_gen > stale);
        a.handle_event(AppEvent::DetailLoaded {
            req: stale,
            result: Ok(preview_view(10, "alpha detail")),
        });
        assert!(
            a.detail.is_none(),
            "a stale old-profile detail must not navigate the new instance"
        );
        assert_eq!(a.screen, Screen::Home);
    }

    #[test]
    fn fetches_are_fenced_while_a_switch_is_in_flight() {
        // While a switch is pending the old client must not be queried: a search
        // submit / Enter / refresh / palette fetch is suppressed (not dispatched),
        // with a brief status, until the switch settles.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        set_results(&mut a, vec![result(1, "edge01")]);
        a.handle_event(press(KeyCode::Char('P'))); // switch to beta (pending)
        assert!(a.switch_in_flight());

        // A search submit is fenced.
        a.handle_event(press(KeyCode::Char('/')));
        for c in "edge".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty(), "search is fenced mid-switch");
        assert!(a.status.contains("switching profile"));

        // Enter to open a detail is fenced.
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty(), "open-detail is fenced mid-switch");

        // Manual refresh (r) is fenced.
        let cmds = a.handle_event(press(KeyCode::Char('r')));
        assert!(cmds.is_empty(), "refresh is fenced mid-switch");

        // The background preview/auto-refresh ticks are fenced silently.
        a.mark_preview_dirty_for_test();
        assert!(
            a.handle_event(AppEvent::PreviewTick).is_empty(),
            "preview tick is fenced mid-switch"
        );
        a.last_query = Some("edge".into());
        assert!(
            a.handle_event(AppEvent::Tick).is_empty(),
            "auto-refresh tick is fenced mid-switch"
        );

        // After the switch settles, fetches flow again.
        profile_switched_ok(&mut a, "4.5.0");
        assert!(!a.switch_in_flight());
        a.handle_event(press(KeyCode::Char('/')));
        for c in "core".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(
            matches!(cmds.as_slice(), [AppCommand::Search { query, .. }] if query == "core"),
            "searches dispatch again once the switch settles"
        );
    }

    #[test]
    fn superseded_profile_switch_result_is_dropped() {
        // Cycling twice quickly: the first switch's reconnect returns LAST. It must
        // be dropped (latest-switch-wins, by id) so it can't overwrite the second
        // one's client/version with a superseded profile's.
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);
        let beta_id = match a.handle_event(press(KeyCode::Char('P'))).as_slice() {
            [AppCommand::SwitchProfile { id, name, .. }] if name == "beta" => *id,
            other => panic!("expected SwitchProfile beta, got {other:?}"),
        };
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        let gamma_id = match a.handle_event(press(KeyCode::Char('P'))).as_slice() {
            [AppCommand::SwitchProfile { id, name, .. }] if name == "gamma" => *id,
            other => panic!("expected SwitchProfile gamma, got {other:?}"),
        };
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));
        assert!(gamma_id > beta_id, "the later switch has a higher id");
        assert_eq!(a.pending, 2, "both reconnects are in flight");

        // The newer (gamma) switch resolves first and is adopted.
        let client = NetBoxClient::new(
            &ProfileConfig {
                url: "http://x".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            id: gamma_id,
            name: "gamma".into(),
            result: Ok((client.clone(), "4.9.0".into())),
        });
        assert_eq!(a.netbox_version, "4.9.0");
        assert!(a.pending_profile.is_none());

        // The stale beta switch lands afterwards: dropped by id, leaving gamma.
        a.handle_event(AppEvent::ProfileSwitched {
            id: beta_id,
            name: "beta".into(),
            result: Ok((client, "4.4.0".into())),
        });
        assert_eq!(
            a.netbox_version, "4.9.0",
            "a superseded switch must not overwrite the current version"
        );
        assert_eq!(a.profile_name, "gamma");
        assert_eq!(a.pending, 0, "stale switch still settles its fetch");
    }

    #[test]
    fn stale_reswitch_to_same_name_is_dropped_by_id() {
        // The race the id guard exists for: switch to beta, then gamma, then beta
        // AGAIN. The FIRST beta's reconnect returns LATE — and shares beta's name
        // with the now-current (second) beta switch. A name-based guard would let
        // the stale first beta settle the second; the id guard drops it, since its
        // id is older than the high-water mark. Only the second beta settles.
        let mut a = app_with_profiles(&["alpha", "beta", "gamma"]);

        // alpha → beta (first beta switch; capture its id for the late delivery).
        let beta1_id = match a.handle_event(press(KeyCode::Char('P'))).as_slice() {
            [AppCommand::SwitchProfile { id, name, .. }] if name == "beta" => *id,
            other => panic!("expected SwitchProfile beta, got {other:?}"),
        };
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));

        // beta → gamma (steps forward from the pending target).
        assert!(matches!(
            a.handle_event(press(KeyCode::Char('P'))).as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        assert_eq!(a.pending_profile.as_deref(), Some("gamma"));

        // gamma → beta AGAIN (the second beta switch; this is the current one).
        // Jump by name via the palette so the target is unambiguously beta,
        // whatever the cycle direction would land on, and so it rides the same
        // `handle_event` accounting (bumping the in-flight fetch counter).
        a.handle_event(press(KeyCode::Char(':')));
        for c in "profile beta".chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
        let beta2_id = match a.handle_event(press(KeyCode::Enter)).as_slice() {
            [AppCommand::SwitchProfile { id, name, .. }] if name == "beta" => *id,
            other => panic!("expected SwitchProfile beta (again), got {other:?}"),
        };
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        assert!(
            beta2_id > beta1_id,
            "the re-switch to the same name has a strictly newer id"
        );
        assert_eq!(a.pending_switch, Some(beta2_id), "awaiting the second beta");
        assert_eq!(a.pending, 3, "all three reconnects are in flight");

        // The connected instance is still alpha — nothing has settled yet.
        assert_eq!(
            a.profile_name, "alpha",
            "header still on the connected alpha"
        );
        assert_eq!(a.base_url, "http://alpha.example");

        let client = NetBoxClient::new(
            &ProfileConfig {
                url: "http://x".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();

        // The FIRST beta's reconnect lands LATE — same name as the current pending
        // switch, but an older id. It MUST be dropped: it can neither flip the
        // header nor clear the still-pending second beta.
        a.handle_event(AppEvent::ProfileSwitched {
            id: beta1_id,
            name: "beta".into(),
            result: Ok((client.clone(), "4.1.0".into())),
        });
        assert_eq!(
            a.profile_name, "alpha",
            "a stale same-name reconnect must not flip the header"
        );
        assert_eq!(a.base_url, "http://alpha.example");
        assert_ne!(
            a.netbox_version, "4.1.0",
            "stale version must not be adopted"
        );
        assert_eq!(
            a.pending_profile.as_deref(),
            Some("beta"),
            "a dropped stale switch must not clear the newer pending state"
        );
        assert_eq!(
            a.pending_switch,
            Some(beta2_id),
            "still awaiting second beta"
        );
        assert_eq!(
            a.pending, 2,
            "the stale reconnect still settles its own fetch"
        );

        // The SECOND beta (the current switch) settles: now the header flips.
        a.handle_event(AppEvent::ProfileSwitched {
            id: beta2_id,
            name: "beta".into(),
            result: Ok((client, "4.9.0".into())),
        });
        assert_eq!(
            a.profile_name, "beta",
            "the current switch settles the header"
        );
        assert_eq!(a.profile_index, 1);
        assert_eq!(a.base_url, "http://beta.example");
        assert_eq!(a.netbox_version, "4.9.0");
        assert!(a.pending_profile.is_none(), "the current switch settled");
        assert!(a.pending_switch.is_none());
    }

    #[test]
    fn single_switch_settles_normally_on_ok_and_err() {
        // A plain single switch still settles correctly on both paths, with the
        // header-matches-connected-client invariant held throughout.
        // Ok path: header flips to the target only on success.
        let mut a = app_with_profiles(&["alpha", "beta"]);
        let id = match a.handle_event(press(KeyCode::Char('P'))).as_slice() {
            [AppCommand::SwitchProfile { id, name, .. }] if name == "beta" => *id,
            other => panic!("expected SwitchProfile beta, got {other:?}"),
        };
        // Pending: header still on the connected alpha (invariant holds).
        assert_eq!(a.profile_name, "alpha");
        assert_eq!(a.base_url, "http://alpha.example");
        assert_eq!(a.pending_switch, Some(id));
        a.handle_event(AppEvent::ProfileSwitched {
            id,
            name: "beta".into(),
            result: Ok((
                NetBoxClient::new(
                    &ProfileConfig {
                        url: "http://x".into(),
                        ..Default::default()
                    },
                    None,
                )
                .unwrap(),
                "4.8.0".into(),
            )),
        });
        assert_eq!(
            a.profile_name, "beta",
            "success flips the header to the target"
        );
        assert_eq!(
            a.base_url, "http://beta.example",
            "header matches new client"
        );
        assert_eq!(a.netbox_version, "4.8.0");
        assert!(a.pending_switch.is_none());
        assert!(a.pending_profile.is_none());

        // Err path (from the new connected instance, beta): a failed switch leaves
        // the header on the connected beta — no phantom.
        let id = match a.handle_event(press(KeyCode::Char('P'))).as_slice() {
            [AppCommand::SwitchProfile { id, name, .. }] if name == "alpha" => *id,
            other => panic!("expected SwitchProfile alpha, got {other:?}"),
        };
        assert_eq!(
            a.profile_name, "beta",
            "header on the connected beta while pending"
        );
        a.handle_event(AppEvent::ProfileSwitched {
            id,
            name: "alpha".into(),
            result: Err(anyhow::anyhow!("unreachable")),
        });
        assert_eq!(
            a.profile_name, "beta",
            "a failed switch leaves the header put"
        );
        assert_eq!(a.base_url, "http://beta.example", "no phantom on failure");
        assert_eq!(a.netbox_version, "4.8.0", "old client intact");
        assert!(a.pending_switch.is_none(), "the failed switch settled");
        assert_eq!(a.status_severity, Severity::Error);
    }

    #[test]
    fn set_profiles_indexes_the_active_profile() {
        // When the active profile isn't the first entry, set_profiles points the
        // index at it so the first cycle steps from the right place.
        let mut a = app();
        a.profile_name = "beta".into();
        a.set_profiles(vec![
            ProfileEntry {
                name: "alpha".into(),
                config: ProfileConfig::default(),
            },
            ProfileEntry {
                name: "beta".into(),
                config: ProfileConfig::default(),
            },
            ProfileEntry {
                name: "gamma".into(),
                config: ProfileConfig::default(),
            },
        ]);
        assert_eq!(a.profile_index, 1, "indexed to the active profile");
        // Forward from beta targets gamma; the header flips once it succeeds.
        let cmds = a.handle_event(press(KeyCode::Char('P')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "gamma"
        ));
        profile_switched_ok(&mut a, "4.5.0");
        assert_eq!(a.profile_name, "gamma");
    }

    // --- Phase B: in-app Config modal (Profiles editor) ---------------------

    use crate::tui::config_modal::{ConfigSection, ProfileForm};

    /// An app with profiles AND a real on-disk config file, so the save/delete
    /// persistence paths run. Returns the app + the temp file path (kept alive by
    /// the returned `TempDir`).
    fn app_with_config(names: &[&str]) -> (App, tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Seed the file with the profiles + the first as active.
        let mut doc = crate::config::load_doc_or_new(&path).unwrap();
        for n in names {
            crate::config::upsert_profile(&mut doc, n, &format!("https://{n}"), None).unwrap();
        }
        if let Some(first) = names.first() {
            crate::config::set_active_profile(&mut doc, first);
        }
        crate::config::write_doc(&path, &doc).unwrap();

        let mut a = app_with_profiles(names);
        a.config_path = Some(path.clone());
        (a, dir, path)
    }

    /// Type a string into the open Config form's focused field.
    fn type_form(a: &mut App, s: &str) {
        for c in s.chars() {
            a.handle_event(press(KeyCode::Char(c)));
        }
    }

    #[test]
    fn s_key_opens_config_modal_on_profiles() {
        let mut a = app_with_profiles(&["a", "b"]);
        assert!(a.modal.is_none());
        a.handle_event(press(KeyCode::Char('S')));
        assert!(config_open(&a), "S opens the Config modal");
        if let Some(Modal::Config(m)) = &a.modal {
            assert_eq!(m.section, ConfigSection::Profiles);
        }
        // Esc closes it.
        a.handle_event(press(KeyCode::Esc));
        assert!(a.modal.is_none());
    }

    #[test]
    fn palette_config_verb_opens_the_modal() {
        let mut a = app_with_profiles(&["a"]);
        let cmds = a.apply_palette(PaletteCommand::Config);
        assert!(cmds.is_empty());
        assert!(config_open(&a));
    }

    #[test]
    fn config_modal_consumes_keys_and_does_not_act_on_underlying_screen() {
        // With the Config modal open, `j` moves the modal list selection, NOT the
        // home results selection underneath.
        let mut a = app_with_profiles(&["a", "b"]);
        set_results(&mut a, results_n(3));
        assert_eq!(a.selected, 0);
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('j')));
        assert_eq!(a.selected, 0, "home selection untouched while modal open");
        if let Some(Modal::Config(m)) = &a.modal {
            assert!(matches!(
                m.profiles.mode,
                ProfilesMode::List { selected: 1 }
            ));
        } else {
            panic!("config modal should still be open");
        }
    }

    #[test]
    fn ctrl_c_quits_even_with_a_modal_open() {
        let mut a = app_with_profiles(&["a"]);
        a.handle_event(press(KeyCode::Char('S')));
        assert!(config_open(&a));
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(a.should_quit, "Ctrl+C hard-quits regardless of the modal");
    }

    #[test]
    fn add_profile_saves_metadata_and_token_env_to_config() {
        let (mut a, _dir, path) = app_with_config(&["alpha"]);
        a.handle_event(press(KeyCode::Char('S'))); // open modal
        a.handle_event(press(KeyCode::Char('a'))); // add form
        type_form(&mut a, "lab"); // name
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "https://nb.lab"); // url
        a.handle_event(press(KeyCode::Tab)); // → token_env
        type_form(&mut a, "NETBOX_TOKEN"); // env-backed token source
        // Enter saves (no test required).
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert!(cmds.is_empty(), "plain save does not switch");
        // The live list gained the profile.
        assert!(a.profiles.iter().any(|p| p.name == "lab"));
        // The file has the metadata and env-var name, not a token value.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[profiles.lab]"));
        assert!(text.contains("https://nb.lab"));
        assert!(text.contains("token_env = \"NETBOX_TOKEN\""));
        assert!(
            !text.contains("nbt_supersecret"),
            "an env-backed profile should not invent a config token"
        );
        // Back on the list, the saved profile is selected.
        if let Some(Modal::Config(m)) = &a.modal {
            assert!(matches!(m.profiles.mode, ProfilesMode::List { .. }));
        }
    }

    #[test]
    fn add_profile_with_pasted_token_saves_config_token_by_default() {
        let (mut a, _dir, path) = app_with_config(&["alpha"]);
        a.handle_event(press(KeyCode::Char('S'))); // open modal
        a.handle_event(press(KeyCode::Char('a'))); // add form
        type_form(&mut a, "lab"); // name
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "https://nb.lab"); // url
        a.handle_event(press(KeyCode::Tab)); // → token_env
        a.handle_event(press(KeyCode::Tab)); // → token (masked)
        type_form(&mut a, "nbt_supersecret.value");

        let cmds = a.handle_event(press(KeyCode::Enter));

        assert!(cmds.is_empty());
        let lab = a
            .profiles
            .iter()
            .find(|p| p.name == "lab")
            .expect("profile should be added live");
        assert_eq!(
            lab.config.token.as_ref().map(ConfigToken::expose),
            Some("nbt_supersecret.value")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[profiles.lab]"));
        assert!(text.contains("token = \"nbt_supersecret.value\""), "{text}");
        assert!(
            !text.contains("token_store"),
            "the removed keyring `token_store` key must never be written: {text}"
        );
        assert_eq!(a.status_severity, Severity::Success);
        if let Some(Modal::Config(m)) = &a.modal {
            assert!(matches!(m.profiles.mode, ProfilesMode::List { .. }));
        } else {
            panic!("config modal should remain open on the profile list");
        }
    }

    #[test]
    fn edit_profile_prefills_form_and_persists_changes() {
        let (mut a, _dir, path) = app_with_config(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('j'))); // select beta
        a.handle_event(press(KeyCode::Char('e'))); // edit
        // The form is prefilled with beta's url.
        if let Some(Modal::Config(m)) = &a.modal {
            let f = m.form().expect("edit form open");
            assert_eq!(f.name(), "beta");
            // Prefilled from the LIVE profile entry (app_with_profiles' url shape).
            assert_eq!(f.url(), "http://beta.example");
            assert_eq!(f.editing.as_deref(), Some("beta"));
        } else {
            panic!("edit form should be open");
        }
        // Cycle auth_scheme to bearer and toggle verify_tls off, then save.
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
        )));
        a.handle_event(press(KeyCode::Enter));
        // The changes persisted to the file.
        let cfg = crate::config::load(&path).unwrap();
        let beta = &cfg.profiles["beta"];
        assert_eq!(beta.auth_scheme, Some(AuthScheme::Bearer));
        assert_eq!(beta.verify_tls, Some(false));
        // And to the live entry.
        let live = a.profiles.iter().find(|p| p.name == "beta").unwrap();
        assert_eq!(live.config.auth_scheme, Some(AuthScheme::Bearer));
        assert_eq!(live.config.verify_tls, Some(false));
    }

    #[test]
    fn save_writes_the_new_profile_knobs_and_defaults_stay_clean() {
        use crate::config::{ApiSurface, BackendPreference};
        let (mut a, _dir, path) = app_with_config(&["alpha"]);
        a.handle_event(press(KeyCode::Char('S'))); // open modal
        a.handle_event(press(KeyCode::Char('a'))); // add form
        type_form(&mut a, "lab"); // name
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "https://nb.lab"); // url
        // Tab to timeout_secs (url→token_env→token→timeout_secs) and type a value.
        for _ in 0..3 {
            a.handle_event(press(KeyCode::Tab));
        }
        type_form(&mut a, "30"); // timeout_secs
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "250"); // page_size
        // exclude defaults on → flip it OFF with Ctrl+E; route vrf through graphql.
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )));
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        )));
        a.handle_event(press(KeyCode::Enter)); // save

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[profiles.lab.api]"),
            "api table written: {text}"
        );
        assert!(text.contains("vrf = \"graphql\""), "{text}");
        // route_target stayed REST → no key for it.
        assert!(
            !text.contains("route_target"),
            "REST surface stays clean: {text}"
        );

        let cfg = crate::config::load(&path).unwrap();
        let lab = &cfg.profiles["lab"];
        assert_eq!(lab.timeout_secs, Some(30));
        assert_eq!(lab.page_size, Some(250));
        assert_eq!(lab.exclude_config_context, Some(false));
        assert_eq!(
            lab.api_preference(ApiSurface::Vrf),
            BackendPreference::Graphql
        );
        assert_eq!(
            lab.api_preference(ApiSurface::RouteTarget),
            BackendPreference::Rest
        );

        // Now edit it back to defaults: clear the numeric fields, flip exclude on,
        // cycle vrf back to REST — the keys should drop and the [api] table vanish.
        a.handle_event(press(KeyCode::Char('e'))); // edit the (selected) saved profile
        // Tab to timeout_secs and clear it (Ctrl+U), same for page_size.
        for _ in 0..4 {
            a.handle_event(press(KeyCode::Tab));
        }
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ))); // clear timeout_secs
        a.handle_event(press(KeyCode::Tab));
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ))); // clear page_size
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        ))); // exclude back on (default)
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ))); // vrf back to REST
        a.handle_event(press(KeyCode::Enter)); // save

        let text2 = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text2.contains("timeout_secs"),
            "cleared timeout drops the key: {text2}"
        );
        assert!(
            !text2.contains("page_size"),
            "cleared page_size drops the key: {text2}"
        );
        assert!(
            !text2.contains("[profiles.lab.api]"),
            "REST-everywhere drops the api table: {text2}"
        );
        // exclude is written explicitly (like verify_tls), so it's present as true.
        let cfg2 = crate::config::load(&path).unwrap();
        let lab2 = &cfg2.profiles["lab"];
        assert_eq!(lab2.timeout_secs, None);
        assert_eq!(lab2.page_size, None);
        assert_eq!(lab2.exclude_config_context, Some(true));
        assert_eq!(
            lab2.api_preference(ApiSurface::Vrf),
            BackendPreference::Rest
        );
    }

    #[test]
    fn edit_rename_removes_the_old_toml_section_and_repoints_active() {
        // H1: renaming a profile via edit must drop the old [profiles.<old>] from
        // the file (no phantom returns next launch) and, if it was active, repoint
        // active_profile to the new name.
        let (mut a, _dir, path) = app_with_config(&["alpha", "beta"]);
        // alpha is active; edit it and rename to "renamed".
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('e'))); // edit selected (alpha, idx 0)
        // Clear the name field (Ctrl+U) and type the new name.
        a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
        type_form(&mut a, "renamed");
        a.handle_event(press(KeyCode::Enter)); // save (no use)

        let cfg = crate::config::load(&path).unwrap();
        assert!(!cfg.profiles.contains_key("alpha"), "old section removed");
        assert!(cfg.profiles.contains_key("renamed"), "new section written");
        assert!(cfg.profiles.contains_key("beta"), "sibling untouched");
        assert_eq!(
            cfg.active_profile.as_deref(),
            Some("renamed"),
            "active repointed to the new name"
        );
        // The live list also no longer carries the old name.
        assert!(a.profiles.iter().all(|p| p.name != "alpha"));
        assert!(a.profiles.iter().any(|p| p.name == "renamed"));
        assert_eq!(
            a.profile_name, "renamed",
            "the running active label follows the rename"
        );
        assert_eq!(
            a.profiles[a.profile_index].name, "renamed",
            "active index re-anchors to the renamed profile"
        );
    }

    #[test]
    fn save_with_no_config_path_surfaces_an_error_not_a_false_success() {
        // M7: a save with no backing config path can't persist — surface an error
        // (not a misleading "saved").
        let mut a = app_with_profiles(&["alpha", "beta"]);
        a.config_path = None;
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('a'))); // add form
        type_form(&mut a, "gamma");
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "https://nb.gamma");
        a.handle_event(press(KeyCode::Enter));
        // No new live profile, and the status reflects the failure.
        assert!(a.profiles.iter().all(|p| p.name != "gamma"));
        assert_eq!(a.status_severity, Severity::Error);
        assert!(a.status.contains("no config file path"));
    }

    #[test]
    fn select_from_modal_persists_active_and_rides_the_switch_path() {
        let (mut a, _dir, path) = app_with_config(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('j'))); // → beta
        let cmds = a.handle_event(press(KeyCode::Enter)); // select beta
        // The modal closed and a switch was dispatched.
        assert!(a.modal.is_none());
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::SwitchProfile { name, .. }] if name == "beta"
        ));
        // active_profile was persisted (explicit select persists, unlike P-cycle).
        let cfg = crate::config::load(&path).unwrap();
        assert_eq!(cfg.active_profile.as_deref(), Some("beta"));
    }

    #[test]
    fn save_then_switch_drops_a_stale_prior_switch_via_the_id_guard() {
        // A quick P-cycle starts a switch (id 1). Before it returns, an explicit
        // modal save+use starts a newer switch (id 2). The stale id-1 completion is
        // dropped; only the id-2 (save+use) switch settles.
        let (mut a, _dir, _path) = app_with_config(&["alpha", "beta"]);
        // P → switch to beta (id 1, pending).
        a.handle_event(press(KeyCode::Char('P')));
        let stale_id = a.pending_switch.expect("a switch is pending");
        assert_eq!(a.pending_profile.as_deref(), Some("beta"));
        // Open the modal and add+use a new profile (a newer switch, id 2).
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('a')));
        type_form(&mut a, "gamma");
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "https://nb.gamma");
        // Ctrl+G = save+use (Ctrl+U is the field clear-line, not save+use).
        let cmds = a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        let new_id = match cmds.as_slice() {
            [AppCommand::SwitchProfile { id, name, .. }] if name == "gamma" => *id,
            other => panic!("expected a switch to gamma, got {other:?}"),
        };
        assert!(
            new_id > stale_id,
            "the save+use switch supersedes the cycle"
        );
        // The stale id-1 completion is dropped (latest-switch-wins).
        let client = NetBoxClient::new(
            &ProfileConfig {
                url: "http://x".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            id: stale_id,
            name: "beta".into(),
            result: Ok((client, "4.5.0".into())),
        });
        assert_ne!(a.profile_name, "beta", "the stale switch did not settle");
        // The newer (gamma) completion does settle.
        let client2 = NetBoxClient::new(
            &ProfileConfig {
                url: "http://x".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        a.handle_event(AppEvent::ProfileSwitched {
            id: new_id,
            name: "gamma".into(),
            result: Ok((client2, "4.5.0".into())),
        });
        assert_eq!(a.profile_name, "gamma");
    }

    #[test]
    fn delete_removes_from_file_and_live_list() {
        let (mut a, _dir, path) = app_with_config(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('j'))); // → beta (not active)
        a.handle_event(press(KeyCode::Char('d'))); // confirm
        let cmds = a.handle_event(press(KeyCode::Char('y'))); // delete
        assert!(cmds.is_empty());
        // Gone from the live list and the file.
        assert!(!a.profiles.iter().any(|p| p.name == "beta"));
        let cfg = crate::config::load(&path).unwrap();
        assert!(!cfg.profiles.contains_key("beta"));
        assert!(cfg.profiles.contains_key("alpha"));
    }

    #[test]
    fn delete_active_profile_is_blocked_with_a_message() {
        let (mut a, _dir, _path) = app_with_config(&["alpha", "beta"]);
        a.handle_event(press(KeyCode::Char('S'))); // alpha is selected + active
        a.handle_event(press(KeyCode::Char('d')));
        // No confirm opened; a guidance message is set.
        if let Some(Modal::Config(m)) = &a.modal {
            assert!(matches!(m.profiles.mode, ProfilesMode::List { .. }));
            assert!(m.profiles.message.as_deref().unwrap().contains("active"));
        } else {
            panic!("modal should still be open on the list");
        }
        assert!(
            a.profiles.iter().any(|p| p.name == "alpha"),
            "still present"
        );
    }

    #[test]
    fn test_connect_dispatches_a_guarded_command_and_result_lands_in_the_form() {
        let (mut a, _dir, _path) = app_with_config(&["alpha"]);
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('a')));
        type_form(&mut a, "lab");
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "https://nb.lab");
        // Ctrl+T → a test-connect command, spinner up, form in Testing.
        let cmds = a.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )));
        let id = match cmds.as_slice() {
            [AppCommand::TestConnect { id, .. }] => *id,
            other => panic!("expected a TestConnect, got {other:?}"),
        };
        assert!(a.loading(), "a test-connect is a tracked fetch");
        if let Some(Modal::Config(m)) = &a.modal {
            assert_eq!(m.form().unwrap().test, TestState::Testing);
        }
        // The result lands in the form and clears the spinner.
        a.handle_event(AppEvent::ConnectTested {
            id,
            result: Ok("4.5.0".into()),
        });
        assert!(!a.loading());
        if let Some(Modal::Config(m)) = &a.modal {
            assert_eq!(m.form().unwrap().test, TestState::Ok("4.5.0".into()));
        }
    }

    #[test]
    fn stale_test_connect_result_is_dropped() {
        let (mut a, _dir, _path) = app_with_config(&["alpha"]);
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Char('a')));
        type_form(&mut a, "lab");
        a.handle_event(press(KeyCode::Tab));
        type_form(&mut a, "https://nb.lab");
        // Two tests; only the second (newer id) result should land.
        let first = match a
            .handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Char('t'),
                KeyModifiers::CONTROL,
            )))
            .as_slice()
        {
            [AppCommand::TestConnect { id, .. }] => *id,
            _ => panic!(),
        };
        let second = match a
            .handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Char('t'),
                KeyModifiers::CONTROL,
            )))
            .as_slice()
        {
            [AppCommand::TestConnect { id, .. }] => *id,
            _ => panic!(),
        };
        assert!(second > first);
        // The stale (first) result is dropped — the form stays Testing.
        a.handle_event(AppEvent::ConnectTested {
            id: first,
            result: Ok("9.9.9".into()),
        });
        if let Some(Modal::Config(m)) = &a.modal {
            assert_eq!(m.form().unwrap().test, TestState::Testing);
        }
        // The current (second) result lands.
        a.handle_event(AppEvent::ConnectTested {
            id: second,
            result: Err(anyhow::anyhow!("unreachable")),
        });
        if let Some(Modal::Config(m)) = &a.modal {
            assert!(matches!(m.form().unwrap().test, TestState::Failed(_)));
        }
    }

    #[test]
    fn form_is_invalidated_after_a_successful_test_when_edited() {
        // A test OK shouldn't survive a further edit (it would describe stale
        // contents). The pure form invalidates the test on the next keystroke.
        let mut f = ProfileForm::add();
        f.test = TestState::Ok("4.5.0".into());
        // Reuse the modal's key path: typing into the focused field invalidates.
        let mut m = ConfigModal::default();
        m.profiles.mode = ProfilesMode::Form(f);
        m.handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &[],
            "",
        );
        assert_eq!(m.form().unwrap().test, TestState::Idle);
    }

    // --- Phase C: Config modal Settings section -----------------------------

    fn ctrl_press(c: char) -> AppEvent {
        AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// Open the Config modal on the Settings section (S, then Tab).
    fn open_settings(a: &mut App) {
        a.handle_event(press(KeyCode::Char('S')));
        a.handle_event(press(KeyCode::Tab)); // Profiles list → Settings
        assert!(matches!(
            &a.modal,
            Some(Modal::Config(m)) if m.section == ConfigSection::Settings
        ));
    }

    #[test]
    fn settings_theme_cycle_hot_applies_to_the_live_theme() {
        let mut a = app_with_profiles(&["a"]);
        assert_eq!(a.theme.name(), "default");
        open_settings(&mut a);
        // Right enters the Appearance fields (theme); Right again cycles + hot-applies.
        a.handle_event(press(KeyCode::Right)); // enter fields
        a.handle_event(press(KeyCode::Right)); // cycle theme
        assert_eq!(a.theme.name(), Theme::list()[1]);
        assert_eq!(a.status, format!("theme: {}", a.theme.name()));
        assert_eq!(a.status_ttl, Some(TRANSIENT_STATUS_TICKS));
    }

    #[test]
    fn no_color_blocks_the_settings_theme_change() {
        // The NO_COLOR guard (shared with the `t` cycle / `:theme`) also gates the
        // Settings theme change: it can't re-enable color, and the live theme stays
        // no-color.
        let mut a = app_with_profiles(&["a"]);
        a.set_no_color();
        assert!(a.theme.is_no_color());
        open_settings(&mut a);
        a.handle_event(press(KeyCode::Right)); // enter Appearance fields
        a.handle_event(press(KeyCode::Right)); // try to cycle the theme
        assert!(
            a.theme.is_no_color(),
            "still no-color after a settings cycle"
        );
        assert_eq!(a.status, "NO_COLOR is set — theme change disabled");
    }

    #[test]
    fn settings_save_persists_each_ui_field_and_hot_applies() {
        let (mut a, _dir, path) = app_with_config(&["a"]);
        a.set_ui_settings(None, String::new(), None, None);
        open_settings(&mut a);
        // Appearance → theme: enter fields, cycle once (hot-applies + persists).
        a.handle_event(press(KeyCode::Right)); // enter Appearance fields
        a.handle_event(press(KeyCode::Right)); // cycle theme
        let themed = a.theme.name().to_string();
        // Behavior → refresh_secs + open command.
        a.handle_event(press(KeyCode::Esc)); // back to categories
        a.handle_event(press(KeyCode::Down)); // → Behavior
        a.handle_event(press(KeyCode::Right)); // enter fields (refresh_secs)
        type_form(&mut a, "30");
        a.handle_event(press(KeyCode::Down)); // → open command
        type_form(&mut a, "firefox --new-tab");
        // Save (Enter).
        let cmds = a.handle_event(press(KeyCode::Enter));

        // The modal closed and the live values were adopted.
        assert!(a.modal.is_none(), "save closes the modal");
        assert_eq!(a.refresh_secs, Some(30));
        assert_eq!(a.open_browser_command, "firefox --new-tab");
        // Re-arm is emitted because the interval changed (None → 30).
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::ArmRefresh(Some(30))]
        ));

        // The file persisted each [ui] field (format-preserving load round-trips).
        let cfg = crate::config::load(&path).unwrap();
        assert_eq!(cfg.ui.theme, themed);
        assert_eq!(cfg.ui.refresh_secs, Some(30));
        assert_eq!(cfg.ui.open_browser_command, "firefox --new-tab");
    }

    #[test]
    fn settings_save_persists_log_level_and_file() {
        let (mut a, _dir, path) = app_with_config(&["a"]);
        a.set_ui_settings(None, String::new(), None, None);
        open_settings(&mut a);
        // Logging is the fifth category (Appearance, Behavior, Connection, Cache,
        // Logging).
        a.handle_event(press(KeyCode::Down)); // → Behavior
        a.handle_event(press(KeyCode::Down)); // → Connection
        a.handle_event(press(KeyCode::Down)); // → Cache
        a.handle_event(press(KeyCode::Down)); // → Logging
        a.handle_event(press(KeyCode::Right)); // enter fields (log_level)
        type_form(&mut a, "nbox=debug");
        a.handle_event(press(KeyCode::Down)); // → log_file
        type_form(&mut a, "/tmp/nbox.log");
        a.handle_event(press(KeyCode::Enter)); // save

        assert!(a.modal.is_none());
        assert_eq!(a.log_level.as_deref(), Some("nbox=debug"));
        assert_eq!(a.log_file.as_deref(), Some("/tmp/nbox.log"));
        let cfg = crate::config::load(&path).unwrap();
        assert_eq!(cfg.log_level.as_deref(), Some("nbox=debug"));
        assert_eq!(cfg.log_file.as_deref(), Some("/tmp/nbox.log"));
    }

    #[test]
    fn settings_save_persists_and_hot_applies_cache() {
        let (mut a, _dir, path) = app_with_config(&["a"]);
        // Start from a known cache policy (on, 30s).
        a.set_cache(crate::cache::Cache::from_settings(
            "p".into(),
            &crate::config::CacheSettings {
                enabled: true,
                ttl_secs: 30,
            },
        ));
        assert!(a.cache.enabled());
        open_settings(&mut a);
        // Cache is the fourth category (Appearance, Behavior, Connection, Cache,
        // Logging).
        a.handle_event(press(KeyCode::Down)); // → Behavior
        a.handle_event(press(KeyCode::Down)); // → Connection
        a.handle_event(press(KeyCode::Down)); // → Cache
        a.handle_event(press(KeyCode::Right)); // enter fields (cache on/off)
        a.handle_event(press(KeyCode::Char(' '))); // toggle the cache off
        a.handle_event(press(KeyCode::Enter)); // save

        assert!(a.modal.is_none());
        // Hot-applied to the running session…
        assert!(!a.cache.enabled(), "cache disabled live");
        // …and persisted (with the TTL written alongside).
        let cfg = crate::config::load(&path).unwrap();
        assert!(!cfg.cache.enabled);
        assert_eq!(cfg.cache.ttl_secs, 30);
    }

    #[test]
    fn settings_save_connection_knob_persists_to_profile_and_reconnects() {
        let (mut a, _dir, path) = app_with_config(&["a"]);
        open_settings(&mut a);
        // Connection is the third category; page_size is its first field.
        a.handle_event(press(KeyCode::Down)); // → Behavior
        a.handle_event(press(KeyCode::Down)); // → Connection
        a.handle_event(press(KeyCode::Right)); // enter fields (page_size)
        type_form(&mut a, "250");
        let cmds = a.handle_event(press(KeyCode::Enter)); // save

        assert!(a.modal.is_none(), "save closes the modal");
        // Persisted to the ACTIVE profile (not [ui]); a format-preserving round-trip.
        let cfg = crate::config::load(&path).unwrap();
        assert_eq!(cfg.profiles.get("a").unwrap().page_size, Some(250));
        // Reflected on the live profile entry, and hot-applied via a reconnect so
        // the running client picks up the new page_size.
        assert_eq!(a.profiles[a.profile_index].config.page_size, Some(250));
        assert!(
            cmds.iter()
                .any(|c| matches!(c, AppCommand::SwitchProfile { .. })),
            "a connection-knob change reconnects to hot-apply it"
        );
    }

    #[test]
    fn settings_save_api_backend_persists_to_profile_and_reconnects() {
        use crate::config::{ApiSurface, BackendPreference};
        let (mut a, _dir, path) = app_with_config(&["a"]);
        open_settings(&mut a);
        // Connection category; api vrf is the 4th field (page_size, timeout_secs,
        // exclude_config_context, api vrf, api route_target).
        a.handle_event(press(KeyCode::Down)); // → Behavior
        a.handle_event(press(KeyCode::Down)); // → Connection
        a.handle_event(press(KeyCode::Right)); // enter fields (page_size)
        a.handle_event(press(KeyCode::Down)); // → timeout_secs
        a.handle_event(press(KeyCode::Down)); // → exclude_config_context
        a.handle_event(press(KeyCode::Down)); // → api vrf
        a.handle_event(press(KeyCode::Char(' '))); // cycle rest → graphql
        let cmds = a.handle_event(press(KeyCode::Enter)); // save

        assert!(a.modal.is_none(), "save closes the modal");
        // The [api] vrf backend persisted to the active profile and is reflected live.
        let cfg = crate::config::load(&path).unwrap();
        assert_eq!(
            cfg.profiles
                .get("a")
                .unwrap()
                .api_preference(ApiSurface::Vrf),
            BackendPreference::Graphql
        );
        assert_eq!(
            a.profiles[a.profile_index]
                .config
                .api_preference(ApiSurface::Vrf),
            BackendPreference::Graphql
        );
        assert!(
            cmds.iter()
                .any(|c| matches!(c, AppCommand::SwitchProfile { .. })),
            "an api-backend change reconnects to hot-apply it"
        );
    }

    #[test]
    fn settings_save_emits_rearm_only_when_the_interval_changed() {
        let (mut a, _dir, _path) = app_with_config(&["a"]);
        a.set_ui_settings(Some(15), String::new(), None, None);
        open_settings(&mut a);
        // Change only the browser command (not the interval).
        a.handle_event(press(KeyCode::Down)); // → Behavior
        a.handle_event(press(KeyCode::Right)); // enter fields (refresh_secs)
        a.handle_event(press(KeyCode::Down)); // → open command
        type_form(&mut a, "xdg-open");
        let cmds = a.handle_event(press(KeyCode::Enter));
        assert_eq!(a.refresh_secs, Some(15), "interval unchanged");
        assert_eq!(a.open_browser_command, "xdg-open");
        assert!(cmds.is_empty(), "no re-arm when the interval is the same");
    }

    #[test]
    fn settings_ctrl_s_also_saves() {
        let (mut a, _dir, _path) = app_with_config(&["a"]);
        a.set_ui_settings(None, String::new(), None, None);
        open_settings(&mut a);
        a.handle_event(press(KeyCode::Down)); // → Behavior
        a.handle_event(press(KeyCode::Right)); // enter fields (refresh_secs)
        type_form(&mut a, "5");
        let cmds = a.handle_event(ctrl_press('s'));
        assert!(a.modal.is_none());
        assert_eq!(a.refresh_secs, Some(5));
        assert!(matches!(cmds.as_slice(), [AppCommand::ArmRefresh(Some(5))]));
    }

    #[test]
    fn open_browser_command_rides_the_live_value_on_o() {
        // `o` carries the live open_browser_command so a just-changed setting
        // applies to the next open without a restart.
        let mut a = app_with_profiles(&["a"]);
        a.open_browser_command = "firefox".into();
        set_results(&mut a, vec![result(1, "edge01")]);
        let cmds = a.handle_event(press(KeyCode::Char('o')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::OpenBrowser { url, command }]
                if url == "http://nb/dcim/devices/1/" && command == "firefox"
        ));
        // With no command set, the OS default is used (empty command string).
        let mut b = app_with_profiles(&["a"]);
        set_results(&mut b, vec![result(1, "edge01")]);
        let cmds = b.handle_event(press(KeyCode::Char('o')));
        assert!(matches!(
            cmds.as_slice(),
            [AppCommand::OpenBrowser { command, .. }] if command.is_empty()
        ));
    }
}

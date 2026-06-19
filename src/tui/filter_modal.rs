//! The `f` search-filter modal: a small, discoverable editor for the active
//! [`SearchFilters`]. A sibling of the Config modal (not a section) — pure key
//! handling that yields a [`FilterOutcome`]; the app performs the search.
//!
//! The four mutually-exclusive scope filters (site / region / site-group /
//! location) collapse into one **scope** row: a type cycle plus a value, so
//! "one scope at a time" is a UI invariant rather than a post-hoc error.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::netbox::search::SearchFilters;
use crate::tui::cheese::TextInput;

/// The focusable rows of the filter form, in display order.
pub mod row {
    pub const STATUS: usize = 0;
    pub const SCOPE_TYPE: usize = 1;
    pub const SCOPE_VALUE: usize = 2;
    pub const TENANT: usize = 3;
    pub const ROLE: usize = 4;
    pub const TAG: usize = 5;
    pub const VRF: usize = 6;
    /// Number of rows.
    pub const COUNT: usize = 7;
}

/// The scope types, in cycle order. Index 0 is the default selection.
pub const SCOPE_TYPES: &[&str] = &["site", "region", "site-group", "location"];

/// What the app should do after the filter modal handles a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOutcome {
    /// Nothing yet (the modal stays open).
    None,
    /// Close without applying.
    Close,
    /// Apply these filters (and close): replace the active set and re-run.
    Apply(Box<SearchFilters>),
}

/// The filter modal's editable state. PURE — no I/O.
pub struct FilterModal {
    /// Focused row (one of [`row`]).
    pub focus: usize,
    pub status: TextInput,
    /// Selected scope type (index into [`SCOPE_TYPES`]).
    pub scope_type: usize,
    pub scope_value: TextInput,
    pub tenant: TextInput,
    pub role: TextInput,
    pub tag: TextInput,
    pub vrf: TextInput,
}

impl FilterModal {
    /// Seed the form from the active filters: the set scope (if any) selects the
    /// scope type + value; everything else fills its text field.
    pub fn new(f: &SearchFilters) -> Self {
        let (scope_type, scope_val) = if let Some(v) = &f.site {
            (0, v.clone())
        } else if let Some(v) = &f.region {
            (1, v.clone())
        } else if let Some(v) = &f.site_group {
            (2, v.clone())
        } else if let Some(v) = &f.location {
            (3, v.clone())
        } else {
            (0, String::new())
        };
        let seed = |placeholder: &str, val: &Option<String>| {
            let mut t = TextInput::new(placeholder);
            if let Some(v) = val {
                t.set_value(v);
            }
            t
        };
        let mut scope_value = TextInput::new("scope value (empty = no scope)");
        if !scope_val.is_empty() {
            scope_value.set_value(&scope_val);
        }
        Self {
            focus: row::STATUS,
            status: seed("active | planned | offline …", &f.status),
            scope_type,
            scope_value,
            tenant: seed("tenant slug", &f.tenant),
            role: seed("role slug", &f.role),
            tag: seed("tag slug", &f.tag),
            vrf: seed("id | rd | name", &f.vrf),
        }
    }

    /// The selected scope type's label.
    pub fn scope_type_label(&self) -> &'static str {
        SCOPE_TYPES[self.scope_type.min(SCOPE_TYPES.len() - 1)]
    }

    /// The `TextInput` backing a row, if it has one (the scope-type row is a cycle).
    pub fn input_mut(&mut self, focus: usize) -> Option<&mut TextInput> {
        match focus {
            row::STATUS => Some(&mut self.status),
            row::SCOPE_VALUE => Some(&mut self.scope_value),
            row::TENANT => Some(&mut self.tenant),
            row::ROLE => Some(&mut self.role),
            row::TAG => Some(&mut self.tag),
            row::VRF => Some(&mut self.vrf),
            _ => None,
        }
    }

    /// Build the [`SearchFilters`] from the form. The chosen scope is set only when
    /// its value is non-empty; the other three scopes stay clear (mutual exclusion).
    pub fn to_filters(&self) -> SearchFilters {
        let val = |t: &TextInput| {
            let s = t.value().trim();
            (!s.is_empty()).then(|| s.to_string())
        };
        let mut f = SearchFilters {
            status: val(&self.status),
            tenant: val(&self.tenant),
            role: val(&self.role),
            tag: val(&self.tag),
            vrf: val(&self.vrf),
            ..SearchFilters::default()
        };
        if let Some(scope) = val(&self.scope_value) {
            match self.scope_type {
                0 => f.site = Some(scope),
                1 => f.region = Some(scope),
                2 => f.site_group = Some(scope),
                3 => f.location = Some(scope),
                _ => {}
            }
        }
        f
    }

    fn focus_next(&mut self) {
        self.focus = (self.focus + 1) % row::COUNT;
    }

    fn focus_prev(&mut self) {
        self.focus = (self.focus + row::COUNT - 1) % row::COUNT;
    }

    fn cycle_scope(&mut self, forward: bool) {
        let n = SCOPE_TYPES.len();
        self.scope_type = if forward {
            (self.scope_type + 1) % n
        } else {
            (self.scope_type + n - 1) % n
        };
    }

    /// Feed a key; `↑/↓`/`Tab` move rows, the scope row cycles with `←/→/Space`,
    /// `Enter`/`Ctrl+S` apply, `Esc` closes, everything else edits the focused text.
    pub fn handle_key(&mut self, key: KeyEvent) -> FilterOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => FilterOutcome::Close,
            KeyCode::Enter => FilterOutcome::Apply(Box::new(self.to_filters())),
            KeyCode::Char('s') if ctrl => FilterOutcome::Apply(Box::new(self.to_filters())),
            KeyCode::Up => {
                self.focus_prev();
                FilterOutcome::None
            }
            KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                if key.code == KeyCode::BackTab {
                    self.focus_prev();
                } else {
                    self.focus_next();
                }
                FilterOutcome::None
            }
            KeyCode::Left if self.focus == row::SCOPE_TYPE => {
                self.cycle_scope(false);
                FilterOutcome::None
            }
            KeyCode::Right | KeyCode::Char(' ') if self.focus == row::SCOPE_TYPE => {
                self.cycle_scope(true);
                FilterOutcome::None
            }
            _ => {
                if let Some(input) = self.input_mut(self.focus) {
                    input.handle_key(key);
                }
                FilterOutcome::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(m: &mut FilterModal, s: &str) {
        for c in s.chars() {
            m.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn seeds_from_active_filters_including_scope() {
        let f = SearchFilters {
            status: Some("active".into()),
            region: Some("us-east".into()),
            vrf: Some("mgmt".into()),
            ..SearchFilters::default()
        };
        let m = FilterModal::new(&f);
        assert_eq!(m.status.value(), "active");
        assert_eq!(m.scope_type_label(), "region");
        assert_eq!(m.scope_value.value(), "us-east");
        assert_eq!(m.vrf.value(), "mgmt");
    }

    #[test]
    fn building_filters_sets_only_the_selected_scope() {
        let mut m = FilterModal::new(&SearchFilters::default());
        // status row.
        type_str(&mut m, "active");
        // → scope type, cycle to region, → scope value, type it.
        m.handle_key(key(KeyCode::Down)); // scope type
        m.handle_key(key(KeyCode::Right)); // site → region
        m.handle_key(key(KeyCode::Down)); // scope value
        type_str(&mut m, "us-east");
        let f = m.to_filters();
        assert_eq!(f.status.as_deref(), Some("active"));
        assert_eq!(f.region.as_deref(), Some("us-east"));
        assert_eq!(f.site, None, "only the chosen scope is set");
        assert_eq!(f.location, None);
    }

    #[test]
    fn empty_scope_value_sets_no_scope() {
        let mut m = FilterModal::new(&SearchFilters::default());
        m.handle_key(key(KeyCode::Down)); // scope type
        m.handle_key(key(KeyCode::Right)); // region (no value typed)
        let f = m.to_filters();
        assert_eq!(f.region, None, "no value ⇒ no scope filter");
    }

    #[test]
    fn enter_applies_esc_closes() {
        let mut m = FilterModal::new(&SearchFilters::default());
        type_str(&mut m, "planned");
        match m.handle_key(key(KeyCode::Enter)) {
            FilterOutcome::Apply(f) => assert_eq!(f.status.as_deref(), Some("planned")),
            other => panic!("expected Apply, got {other:?}"),
        }
        let mut m2 = FilterModal::new(&SearchFilters::default());
        assert_eq!(m2.handle_key(key(KeyCode::Esc)), FilterOutcome::Close);
    }

    #[test]
    fn typing_routes_to_the_focused_text_row_not_the_scope_cycle() {
        let mut m = FilterModal::new(&SearchFilters::default());
        // On the status row, Space is text (not a scope cycle).
        type_str(&mut m, "a b");
        assert_eq!(m.status.value(), "a b");
        // Move to the scope-type row: Space cycles instead of typing.
        m.handle_key(key(KeyCode::Down));
        let before = m.scope_type;
        m.handle_key(key(KeyCode::Char(' ')));
        assert_ne!(m.scope_type, before, "Space cycles the scope type");
    }
}

//! Terminal UI.
//!
//! A two-pane home screen — a results/recents list on the left, a live preview
//! of the highlighted object on the right — with `Tab`-switched focus routing
//! the movement keys to whichever pane owns scrolling or selection. `Enter`
//! opens a full detail screen (device sub-resource tabs, scrollable body); `/`
//! searches, `:` runs the command palette. The pure app state and input
//! handling live in `state` (event→commands, no I/O); `app` runs the event loop
//! and spawns the network work; `ui` renders each frame; `theme` carries the
//! color themes.

pub mod app;
pub mod cheese;
pub mod config_modal;
pub mod events;
pub mod fuzzy;
pub mod onboarding;
pub mod palette;
pub mod state;
pub mod term;
pub mod theme;
pub mod ui;

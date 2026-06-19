//! NetBox API client and data models.
//!
//! REST is the primary integration path. This module hosts the HTTP client,
//! pagination, endpoint paths, authentication, and (across Phase 2) the model
//! structs. See `DESIGN.md` and `ROADMAP.md`.

pub mod auth;
pub mod capabilities;
pub mod client;
pub mod endpoints;
pub mod graphql;
pub mod models;
pub mod pagination;
pub mod query;
pub mod search;
pub mod status;

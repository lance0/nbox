//! NetBox object models — wire representations of REST responses.
//!
//! These mirror NetBox's "complete"/"brief" serializers and are intentionally
//! permissive: related objects arrive as nested brief representations, many
//! fields are nullable, and unknown fields are ignored. UI-facing view models
//! live in `crate::domain`, kept separate from these wire types.

pub mod common;
pub mod dcim;
pub mod ipam;
pub mod tenancy;

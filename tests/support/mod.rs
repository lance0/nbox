//! Shared integration-test support.
//!
//! Integration tests are separate crates, so each test file opts in with
//! `mod support;`. Keep reusable builders and wiremock helpers here instead of
//! cloning representative NetBox payloads across test files.

#![allow(dead_code)]

pub mod binary;
pub mod fixtures;
pub mod json_contract;
pub mod netbox;
pub mod serve;

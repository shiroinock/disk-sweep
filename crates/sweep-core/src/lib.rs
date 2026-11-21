//! Core of disk-sweep: parallel scanning, rule-based classification, and plan/apply.
//! Platform specifics live behind [`platform::Platform`].

pub mod index;
pub mod model;
pub mod overview;
pub mod plan;
pub mod platform;
pub mod projects;
pub mod rules;
pub mod scan;

pub use model::*;

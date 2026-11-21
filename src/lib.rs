//! `logshape` reads logs in one streaming pass and reports their shape:
//! which kinds of lines they contain, how often, and when.
//!
//! The pieces, in the order a line travels through them:
//!
//! - [`input`]: files, stdin and gzip, split into length-capped lines
//! - [`timestamp`] and [`level`]: per-line detection
//! - [`json`]: field extraction for JSON lines
//! - [`mask`]: turning a line into a template
//! - [`histogram`]: time buckets and error bursts
//! - [`analyze`]: the aggregation that ties them together
//! - [`report`]: terminal and JSON rendering

pub mod analyze;
pub mod histogram;
pub mod input;
pub mod json;
pub mod level;
pub mod mask;
pub mod report;
pub mod timestamp;

//! `logshape` reads logs in one streaming pass and reports their shape:
//! which kinds of lines they contain, how often, and when.

pub mod json;
pub mod level;
pub mod mask;
pub mod timestamp;

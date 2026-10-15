//! Client library: argument parsing and the connection/session logic.
//!
//! Exposed as a library so integration tests can drive sessions in-process.

pub mod args;
pub mod bitrate;
pub mod session;

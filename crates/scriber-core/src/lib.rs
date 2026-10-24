//! Scriber's logic, with no Tauri, no async runtime and no direct I/O.
//!
//! Every effect goes through a trait in [`ports`]: the real implementations
//! live in the app crate (`apps/desktop/src-tauri`) and the fakes in this
//! crate's tests. The bridge contract (command names and JSON shapes) is in
//! `docs/design.md`; the types in [`model`] are its Rust side.
//!
//! The example feature is [`scan::scan`]: walk a folder through the
//! [`ports::Host`] port and summarise its files by extension.

pub mod model;
pub mod ports;
pub mod scan;

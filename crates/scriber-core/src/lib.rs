//! Scriber's logic, with no Tauri, no async runtime and no direct I/O.
//!
//! Every effect goes through a trait in [`ports`]: the real implementations
//! live in the app crate (`apps/desktop/src-tauri`) and the fakes in this
//! crate's tests. The bridge contract (command names and JSON shapes) is in
//! `docs/design.md`; the types in [`model`] are its Rust side.
//!
//! The feature is dictation: [`dictation::Dictation`] is the session's state
//! machine, [`audio`] turns captured sound into 16 kHz mono and WAV files,
//! [`transcript`] filters and joins what the transcriber heard,
//! [`vocabulary`] builds the transcriber's prompt from the user's glossary,
//! and [`store`] lays out the data folder.

pub mod audio;
pub mod dictation;
pub mod model;
pub mod ports;
pub mod store;
pub mod transcript;
pub mod vocabulary;

//! Scriber's logic, with no Tauri, no async runtime and no direct I/O.
//!
//! Every effect goes through a trait in [`ports`]: the real implementations
//! live in the engine crate (`crates/scriber-engine`; the clipboard's in the
//! desktop app, `apps/desktop/src-tauri`) and the fakes in this crate's
//! tests. The bridge contract (command names and JSON shapes) is in
//! `docs/design.md`; the types in [`model`] are its Rust side.
//!
//! The feature is dictation: [`dictation::Dictation`] is the session's state
//! machine, [`audio`] turns captured sound into 16 kHz mono and WAV files,
//! [`transcript`] filters and joins what the transcriber heard,
//! [`vocabulary`] builds the transcriber's prompt from the user's glossary,
//! [`store`] lays out the data folder, [`level`] meters the microphone for
//! the window's waveform, [`download`] fetches and verifies a missing speech
//! model, and [`sync`] holds the model that loads once for every caller.

pub mod audio;
pub mod dictation;
pub mod download;
pub mod level;
pub mod model;
pub mod ports;
pub mod store;
pub mod sync;
pub mod transcript;
pub mod vocabulary;

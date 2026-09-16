//! The Tauri boundary: DTOs, event payloads, errors, and the command functions.
//!
//! Commands are thin. They validate arguments, call a service, and project the result. Provider
//! parsing and business rules live below them, in `adapters/` and `services/`.

pub mod commands;
pub mod errors;
pub mod events;
pub mod types;

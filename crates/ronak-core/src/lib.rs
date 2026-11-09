//! # ronak-core
//!
//! The platform-independent heart of Ronak. Nothing in this crate knows which
//! operating system or desktop it runs on: no D-Bus, no Win32, no files, no
//! network. Platform adapters (see `ronak-gnome`) translate the real desktop
//! into the types defined here, and translate [`protocol::Action`]s back into
//! real window moves.
//!
//! ```text
//!  adapter ──PlatformEvent──▶ state ──▶ intent ──▶ layout ──▶ actions ──▶ adapter
//!                               │                                ▲
//!                               └──── privacy (what may leave) ──┘
//! ```
//!
//! Module map:
//! - [`model`]: the open context schema (`schema/context.schema.json`) as Rust types.
//! - [`protocol`]: messages exchanged with platform adapters.
//! - [`apps`]: maps application ids to broad categories (editor, browser, ...).
//! - [`state`]: the live desktop model, updated event by event.
//! - [`privacy`]: exclusion rules and detail-level projection.
//! - [`intent`]: guesses the current task and each window's role in it.
//! - [`layout`]: deterministic solver turning roles into rectangles.
//! - [`actions`]: reversible actions and the undo stack.
//! - [`suggest`]: builds a suggestion from a task, a layout and policy.
//! - [`config`]: user-facing configuration with safe defaults.
//! - [`adapter`]: the channel-based handle every platform adapter returns.

pub mod actions;
pub mod adapter;
pub mod apps;
pub mod config;
pub mod intent;
pub mod layout;
pub mod model;
pub mod privacy;
pub mod protocol;
pub mod state;
pub mod suggest;

pub use model::*;

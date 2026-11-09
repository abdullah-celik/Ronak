//! # ronak-caps
//!
//! Capabilities are things apps can report or do **without their windows**.
//! Each module talks to one source through a standard interface:
//!
//! - [`media`]: any media player, through MPRIS on D-Bus.
//! - [`calendar`]: GNOME's calendar store (Evolution Data Server).
//! - [`mcp`]: any Model Context Protocol server you configure, with Ronak as
//!   the client this time.
//! - [`cpu`]: gentler CPU priority for apps outside the current task, through
//!   the systemd user manager.
//!
//! Notifications are not here: on GNOME they are only visible inside the
//! shell, so the extension reports them as adapter events.

pub mod calendar;
pub mod cpu;
pub mod ical;
pub mod mcp;
pub mod media;

#[derive(Debug, thiserror::Error)]
pub enum CapsError {
    #[error("D-Bus: {0}")]
    Bus(#[from] zbus::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CapsError>;

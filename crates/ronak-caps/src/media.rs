//! Media players through MPRIS, the standard every Linux player (Spotify,
//! Firefox, Chromium, VLC, Rhythmbox, mpv with a plugin) implements. Each
//! player owns a bus name starting with `org.mpris.MediaPlayer2.` and serves
//! the same object, so one implementation covers all of them.

use std::collections::HashMap;

use ronak_core::model::{Access, Capability, CapabilityKind, MediaState, PlaybackStatus};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, proxy};

use crate::Result;

const PREFIX: &str = "org.mpris.MediaPlayer2.";

#[proxy(interface = "org.mpris.MediaPlayer2", default_path = "/org/mpris/MediaPlayer2")]
trait MediaPlayer2 {
    #[zbus(property)]
    fn identity(&self) -> zbus::Result<String>;
}

#[proxy(interface = "org.mpris.MediaPlayer2.Player", default_path = "/org/mpris/MediaPlayer2")]
pub trait Player {
    fn play_pause(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    PlayPause,
    Next,
    Previous,
}

fn text(meta: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let v = meta.get(key)?;
    if let Ok(s) = String::try_from(v.try_clone().ok()?) {
        return Some(s).filter(|s| !s.is_empty());
    }
    let list: Vec<String> = Vec::<String>::try_from(v.try_clone().ok()?).ok()?;
    Some(list.join(", ")).filter(|s| !s.is_empty())
}

/// Every running player, as a capability.
pub async fn players(conn: &Connection, access: Access) -> Result<Vec<Capability>> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let mut names: Vec<String> =
        dbus.list_names().await.map_err(zbus::Error::from)?.into_iter().map(|n| n.to_string()).collect();
    names.retain(|n| n.starts_with(PREFIX));
    names.sort();
    let mut out = vec![];
    for name in names {
        let Ok(player) = PlayerProxy::builder(conn)
            .destination(name.as_str())?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
        else {
            continue;
        };
        let status = match player.playback_status().await.as_deref() {
            Ok("Playing") => PlaybackStatus::Playing,
            Ok("Paused") => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        };
        let meta = player.metadata().await.unwrap_or_default();
        let identity = match MediaPlayer2Proxy::builder(conn)
            .destination(name.as_str())?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
        {
            Ok(p) => p.identity().await.unwrap_or_default(),
            Err(_) => String::new(),
        };
        let short = name.trim_start_matches(PREFIX).split('.').next().unwrap_or("player").to_string();
        out.push(Capability {
            id: format!("media:{short}"),
            kind: CapabilityKind::Media,
            provider: name.clone(),
            name: if identity.is_empty() { short } else { identity },
            access,
            actions: if access == Access::Act {
                vec!["play_pause".into(), "next".into(), "previous".into()]
            } else {
                vec![]
            },
            media: Some(MediaState { status, title: text(&meta, "xesam:title"), artist: text(&meta, "xesam:artist") }),
            calendar: None,
            notifications: None,
            mcp: None,
        });
    }
    Ok(out)
}

/// Sends a command to the player behind `provider` (its bus name).
pub async fn control(conn: &Connection, provider: &str, command: MediaCommand) -> Result<()> {
    if !provider.starts_with(PREFIX) {
        return Err(crate::CapsError::Other(format!("{provider} is not a media player")));
    }
    let p = PlayerProxy::builder(conn).destination(provider)?.build().await?;
    match command {
        MediaCommand::PlayPause => p.play_pause().await?,
        MediaCommand::Next => p.next().await?,
        MediaCommand::Previous => p.previous().await?,
    }
    Ok(())
}

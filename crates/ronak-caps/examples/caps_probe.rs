//! Prints the media players and today's calendar as Ronak sees them.
//! `--fake-player` also serves a stand-in MPRIS player for testing.
use std::collections::HashMap;

use ronak_caps::{calendar::CalendarReader, media};
use ronak_core::model::Access;
use zbus::zvariant::{OwnedValue, Value};

struct Fake;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Fake {
    #[zbus(property)]
    fn identity(&self) -> String {
        "Test Player".into()
    }
}

struct FakePlayer {
    playing: bool,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    fn play_pause(&mut self) {
        self.playing = !self.playing;
    }
    fn next(&self) {}
    fn previous(&self) {}
    #[zbus(property)]
    fn playback_status(&self) -> String {
        if self.playing { "Playing".into() } else { "Paused".into() }
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        m.insert("xesam:title".into(), Value::from("Weightless").try_into().unwrap());
        m.insert("xesam:artist".into(), Value::from(vec!["Marconi Union".to_string()]).try_into().unwrap());
        m
    }
}

#[tokio::main]
async fn main() {
    let conn = zbus::Connection::session().await.expect("session bus");
    let _server = if std::env::args().any(|a| a == "--fake-player") {
        Some(
            zbus::connection::Builder::session()
                .unwrap()
                .name("org.mpris.MediaPlayer2.ronaktest")
                .unwrap()
                .serve_at("/org/mpris/MediaPlayer2", Fake)
                .unwrap()
                .serve_at("/org/mpris/MediaPlayer2", FakePlayer { playing: true })
                .unwrap()
                .build()
                .await
                .expect("fake player"),
        )
    } else {
        None
    };
    let players = media::players(&conn, Access::Act).await.expect("players");
    println!("players: {}", serde_json::to_string(&players).unwrap());
    if let Some(p) = players.first() {
        media::control(&conn, &p.provider, media::MediaCommand::PlayPause).await.expect("control");
        let after = media::players(&conn, Access::Act).await.unwrap();
        println!("after play_pause: {:?}", after[0].media.as_ref().unwrap().status);
    }
    let mut cal = CalendarReader::new(conn.clone());
    match cal.state(chrono::Utc::now()).await {
        Ok(s) => println!("calendar: {}", serde_json::to_string(&s).unwrap()),
        Err(e) => println!("calendar error: {e}"),
    }
}

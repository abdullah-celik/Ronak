//! Prints what Ronak sees inside a running app.
//! Usage: cargo run -p ronak-atspi --example read_app -- <pid> [--no-content]
use ronak_atspi::{AtspiSensor, ReadOptions};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().and_then(|a| a.parse().ok()).expect("usage: read_app <pid> [--no-content]");
    let read_content = !args.any(|a| a == "--no-content");
    let sensor = AtspiSensor::connect().await.expect("accessibility bus");
    for (app, pid, name) in sensor.apps().await.expect("apps") {
        eprintln!("app {:?} pid={pid:?} at {}", name, app.name().map(|n| n.as_str()).unwrap_or("?"));
    }
    let opts = ReadOptions { read_content, ..ReadOptions::default() };
    let reading = sensor.read_pid(pid, None, Some("w1"), &opts).await.expect("read");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "exposes": reading.exposes, "truncated": reading.truncated, "nodes_visited": reading.nodes_visited,
            "elements": reading.elements,
        }))
        .unwrap()
    );
}

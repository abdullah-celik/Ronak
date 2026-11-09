//! Gentler CPU priority for apps outside your current task.
//!
//! GNOME starts every app in its own systemd *scope*, a kernel control group
//! (cgroup) such as `app-gnome-firefox-4242.scope`. The systemd user manager
//! can change a scope's `CPUWeight` at runtime: 100 is normal, 20 means the
//! app gets a fifth of the share when the CPU is busy, and nothing changes
//! when the CPU is idle. It is fully reversible and never stops an app.

use zbus::Connection;
use zbus::zvariant::Value;

use crate::Result;

pub const NORMAL_WEIGHT: u64 = 100;

/// The systemd unit an app's process runs in, if it is an app unit Ronak may
/// touch. Reads `/proc/<pid>/cgroup` ("0::/user.slice/.../app-gnome-x-1.scope").
pub fn app_unit_of_pid(pid: u32) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    app_unit_from_cgroup(&text)
}

pub fn app_unit_from_cgroup(text: &str) -> Option<String> {
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    let unit = path.rsplit('/').next()?;
    let app_unit = unit.starts_with("app-") && (unit.ends_with(".scope") || unit.ends_with(".service"));
    // Never the shell, Ronak itself, or anything outside the user's app slice.
    let protected = ["gnome-shell", "ronak", "org.gnome.Shell"].iter().any(|p| unit.contains(p));
    (app_unit && path.contains("/app.slice/") && !protected).then(|| unit.to_string())
}

/// Sets the CPU weight of a unit through the systemd user manager on the
/// session bus (runtime only: it resets when the app or session ends).
pub async fn set_cpu_weight(conn: &Connection, unit: &str, weight: u64) -> Result<()> {
    let props: Vec<(&str, Value)> = vec![("CPUWeight", Value::U64(weight.clamp(1, 10_000)))];
    conn.call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "SetUnitProperties",
        &(unit, true, props),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_app_units_are_touched() {
        let firefox = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-firefox-4242.scope\n";
        assert_eq!(app_unit_from_cgroup(firefox).as_deref(), Some("app-gnome-firefox-4242.scope"));
        let shell = "0::/user.slice/user-1000.slice/user@1000.service/session.slice/org.gnome.Shell@wayland.service\n";
        assert_eq!(app_unit_from_cgroup(shell), None);
        let ronak = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-ronakd.service\n";
        assert_eq!(app_unit_from_cgroup(ronak), None);
        assert_eq!(app_unit_from_cgroup("0::/system.slice/cron.service\n"), None);
        assert_eq!(app_unit_from_cgroup("garbage"), None);
    }
}

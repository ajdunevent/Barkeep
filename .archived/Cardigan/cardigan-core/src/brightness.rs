//! Forces full screen brightness while a card's barcode is on display --
//! handy in bright daylight, where a dim or auto-dimmed phone screen can
//! be unreadable to a store's barcode scanner.
//!
// -----------------------------------------------------------------------
// NOTE ON CONFIDENCE: same caveat as barcode.rs/camera.rs -- no live
// D-Bus session or real backlight in the environment this was written
// in, so this is unverified against real hardware except where noted.
//
// Confirmed NOT to work on a FuriLabs FLX1S: this file's original,
// gnome-settings-daemon-only implementation. That daemon's private
// brightness interface just isn't what this device's software stack
// provides -- the same "assumed a standard GNOME-session service that
// isn't actually there" pattern camera.rs and the sysfs fallback
// (previously added here) both also hit.
//
// This version is built around a specific working approach for that
// device: go through systemd-logind's Session.SetBrightness method over
// the *system* bus, rather than gnome-settings-daemon over the session
// bus. logind is what actually grants a session temporary write access
// to a backlight/LED device's sysfs files in the first place (that's
// the mechanism a udev rule + group membership is an alternative to,
// not a replacement for) -- calling through it sidesteps needing that
// permission grant to already be set up correctly.
//
// Which session to call SetBrightness on is resolved explicitly, via
// find_seated_session_path -- not the simpler `/session/self` magic
// path, which was confirmed, directly on-device, to fail specifically
// inside a Flatpak sandbox (see that function's own doc comment for the
// full, real evidence). Three tiers overall, each falling through to
// the next on failure:
//   1. logind SetBrightness (system bus) -- for *setting* brightness.
//      Needs a detected backlight/LED device (see detect_device below)
//      to know which class/name to pass it.
//   2. Direct sysfs write to the detected device's `brightness` file --
//      only reachable if permission is already granted some other way
//      (root, or a udev rule granting a group like `video` write
//      access). Also always used for *reading* current brightness,
//      regardless of which tier ends up used for writing -- reading a
//      sysfs file doesn't need any special permission.
//   3. org.gnome.SettingsDaemon.Power.Screen over the session bus --
//      this file's original, only mechanism. Kept as a last resort for
//      a real GNOME session (a laptop, most likely) where it might
//      still be the right thing, per the same "probe per device rather
//      than assume" reasoning as the rest of this file.
// -----------------------------------------------------------------------

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use zbus::blocking::{Connection, Proxy};

/// How long to disable a brightness-toggle button for after every toggle
/// (on *or* off) before letting it be pressed again. This is a safety
/// constraint, not UI polish: toggling brightness rapidly was confirmed
/// to crash Phosh outright, almost certainly from hammering logind/
/// backlight calls faster than the compositor's own brightness handling
/// can keep up with. Shared here as a plain value -- not the
/// gtk::ToggleButton-touching disable/re-enable mechanism itself, which
/// each consumer (the main app's own brightness_button, and the lock-
/// screen plugin's own backlight toggle) still implements as its own
/// few-line wrapper, since this crate deliberately has no GTK
/// dependency (see this crate's own lib.rs doc comment for why) -- so
/// that both stay in sync on the one number that's actually
/// safety-critical to get right, rather than each hard-coding its own,
/// potentially-drifting copy of it. Value confirmed against the main
/// app's own already-validated-on-real-hardware cooldown.
pub const TOGGLE_COOLDOWN_MS: u64 = 1500;

/// A detected brightness-controllable device: which sysfs class it's
/// under (`backlight` or `leds`), its name within that class, and the
/// full directory path (so callers don't have to rebuild it).
struct BacklightDevice {
    class: &'static str,
    name: String,
    dir: PathBuf,
}

/// Checks /sys/class/backlight first (the conventional location for a
/// display backlight), then /sys/class/leds/lcd-backlight (some
/// devices, apparently including the FLX1S, expose it as an LED class
/// device instead). First backlight entry found wins, same
/// first-found-is-good-enough simplification as before -- phones
/// normally have exactly one display backlight.
fn detect_device() -> Option<BacklightDevice> {
    if let Some(entry) = fs::read_dir("/sys/class/backlight")
        .ok()
        .and_then(|mut d| d.find_map(|e| e.ok()))
    {
        return Some(BacklightDevice {
            class: "backlight",
            name: entry.file_name().to_string_lossy().to_string(),
            dir: entry.path(),
        });
    }

    let leds_dir = PathBuf::from("/sys/class/leds/lcd-backlight");
    if leds_dir.join("brightness").exists() {
        return Some(BacklightDevice {
            class: "leds",
            name: "lcd-backlight".to_string(),
            dir: leds_dir,
        });
    }

    None
}

fn read_u32(path: &std::path::Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Cached result of find_seated_session_path, below -- see that
/// function's own doc comment for why this exists: without it, every
/// single brightness toggle would make a second, real logind D-Bus
/// call in addition to the existing SetBrightness one, not just once at
/// startup. `None` (not yet resolved, or the last attempt failed) is
/// deliberately never cached here -- only a real, successful path is,
/// so a transient failure (e.g. the app starting before the real
/// session is fully established) can still be retried on a later call
/// rather than being permanently stuck.
static SEATED_SESSION_PATH: Mutex<Option<String>> = Mutex::new(None);

/// Looks up the real, explicit object path of the seat-assigned session
/// (seat0 on this phone) via logind's own ListSessions method, rather
/// than the `/org/freedesktop/login1/session/self` magic path used
/// previously. That magic path relies on logind matching the *calling
/// process's own* PID/cgroup against its session table to resolve
/// "self" -- confirmed, directly, on the real device, to fail inside a
/// Flatpak sandbox specifically ("Unknown object
/// '/org/freedesktop/login1/session/self'"), almost certainly because
/// the sandbox's own separate PID namespace means that matching can't
/// succeed, even though the underlying D-Bus permission itself is
/// genuinely fine (confirmed separately: a raw ListSessions call over
/// the same connection succeeds and returns real data). Filtering
/// ListSessions's own results by seat sidesteps PID-matching entirely,
/// rather than trying a different PID-based lookup that would likely
/// hit the same root problem.
///
/// Cached in SEATED_SESSION_PATH above after the first successful
/// lookup -- a real, direct regression this closes: an earlier version
/// of this fix called ListSessions fresh on every single invocation,
/// meaning every brightness toggle made two real logind D-Bus calls
/// instead of one. This file's own, pre-existing comment already
/// documents that "toggling brightness rapidly was confirmed to crash
/// Phosh outright, almost certainly from hammering logind/backlight
/// calls faster than the compositor's own brightness handling can keep
/// up with" -- doubling the real call count on every toggle, not just
/// toggling faster, was a real, direct way to make that same, existing
/// risk worse even with the existing cooldown between toggles still
/// intact (the cooldown limits how often a toggle can happen, not how
/// many D-Bus calls each one makes). The resolved path itself doesn't
/// change during the app's own runtime, so caching after the first
/// successful lookup restores the original, single-call-per-toggle
/// rate while keeping the correct, explicit-path fix for the sandboxed
/// "self" failure above.
///
/// Real, on-device output (gdbus call --system --dest=org.freedesktop.
/// login1 --object-path=/org/freedesktop/login1 --method=org.
/// freedesktop.login1.Manager.ListSessions) returned three sessions on
/// this phone, only one with a non-empty seat:
///   ('1', 32011, 'furios', '', '/org/freedesktop/login1/session/_31')
///   ('9', 32011, 'furios', '', '/org/freedesktop/login1/session/_39')
///   ('8', 32011, 'furios', 'seat0', '/org/freedesktop/login1/session/_38')
/// -- the other two are presumably background/non-graphical sessions;
/// seat0 is the one actually tied to the phone's real display, which is
/// the one brightness control genuinely needs.
///
/// NOTE ON CONFIDENCE: the zbus deserialization type used for the
/// returned path field (a plain String rather than zbus's own dedicated
/// ObjectPath type) is a reasonable, common zbus pattern for this exact
/// case, but not independently confirmed against a real, compiled
/// build -- matches this crate's own established practice (see this
/// file's own top-of-file note) of flagging what's confirmed against
/// real behavior (the actual session data above, confirmed on-device)
/// versus what's a reasonable, standard assumption about the Rust-level
/// API shape needed to get it (not yet confirmed to compile).
fn find_seated_session_path(conn: &Connection) -> Option<String> {
    if let Some(cached) = SEATED_SESSION_PATH.lock().unwrap().as_ref() {
        return Some(cached.clone());
    }

    let proxy = Proxy::new(
        conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .ok()?;
    let sessions: Vec<(String, u32, String, String, String)> =
        proxy.call("ListSessions", &()).ok()?;
    let path = sessions
        .into_iter()
        .find(|(_, _, _, seat, _)| !seat.is_empty())
        .map(|(_, _, _, _, path)| path)?;

    *SEATED_SESSION_PATH.lock().unwrap() = Some(path.clone());
    Some(path)
}

/// systemd-logind's own brightness control, over the *system* bus (not
/// session) -- this is what actually grants the calling session write
/// access to the device's sysfs files, which is why this is tried
/// before a raw sysfs write rather than after.
fn logind_set_brightness(class: &str, name: &str, raw: u32) -> Option<()> {
    let conn = Connection::system().ok()?;
    let session_path = find_seated_session_path(&conn)?;
    let proxy = Proxy::new(&conn, "org.freedesktop.login1", session_path, "org.freedesktop.login1.Session")
        .ok()?;
    proxy
        .call::<_, _, ()>("SetBrightness", &(class, name, raw))
        .ok()
}

fn sysfs_set_brightness(dev: &BacklightDevice, raw: u32) -> Option<()> {
    fs::write(dev.dir.join("brightness"), raw.to_string()).ok()
}

const DBUS_BUS_NAME: &str = "org.gnome.SettingsDaemon.Power";
const DBUS_OBJECT_PATH: &str = "/org/gnome/SettingsDaemon/Power";
const DBUS_INTERFACE: &str = "org.gnome.SettingsDaemon.Power.Screen";

fn gnome_screen_proxy(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(conn, DBUS_BUS_NAME, DBUS_OBJECT_PATH, DBUS_INTERFACE)
}

fn dbus_get_brightness() -> Option<i32> {
    let conn = Connection::session().ok()?;
    let proxy = gnome_screen_proxy(&conn).ok()?;
    proxy.get_property::<i32>("Brightness").ok()
}

fn dbus_set_brightness(percent: i32) -> Option<()> {
    let conn = Connection::session().ok()?;
    let proxy = gnome_screen_proxy(&conn).ok()?;
    proxy.set_property("Brightness", percent).ok()?;
    Some(())
}

/// Current screen brightness as a 0-100 percentage, or `None` if nothing
/// could reach it. Used to remember what to restore to after a forced
/// full-brightness session ends.
pub fn get_brightness() -> Option<i32> {
    if let Some(dev) = detect_device() {
        // Falls through to the D-Bus fallback below on a failed read
        // (rather than returning None immediately via `?`), matching
        // set_brightness's own fallback structure for the analogous
        // case (a missing max_brightness there) -- a device being
        // detected at all doesn't guarantee its own brightness/
        // max_brightness files are actually readable right now.
        if let (Some(current), Some(max)) = (
            read_u32(&dev.dir.join("brightness")),
            read_u32(&dev.dir.join("max_brightness")),
        ) {
            if max > 0 {
                return Some(((current * 100) / max).clamp(0, 100) as i32);
            }
        }
    }
    dbus_get_brightness()
}

/// Sets screen brightness to a 0-100 percentage. Returns `None` if
/// nothing could set it -- callers should treat that as "couldn't force
/// brightness here" and fail quietly rather than block card viewing on
/// it.
pub fn set_brightness(percent: i32) -> Option<()> {
    let percent = percent.clamp(0, 100);

    if let Some(dev) = detect_device() {
        let Some(max) = read_u32(&dev.dir.join("max_brightness")) else {
            return dbus_set_brightness(percent);
        };
        let raw = ((percent as u32) * max) / 100;

        if logind_set_brightness(dev.class, &dev.name, raw).is_some() {
            return Some(());
        }
        if sysfs_set_brightness(&dev, raw).is_some() {
            return Some(());
        }
    }

    dbus_set_brightness(percent)
}

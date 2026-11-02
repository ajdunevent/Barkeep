use adw::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gtk::glib;
use std::os::fd::IntoRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cardigan_core::barcode;
use cardigan_core::card::BarcodeType;

// -----------------------------------------------------------------------
// NOTE ON CONFIDENCE: originally written with no camera and no GStreamer
// runtime available to test against, so none of this had actually
// captured a frame. Since then, the Rust-level API surface itself --
// gst::parse::launch(...), gst_app::AppSinkCallbacks::builder()
// .new_sample(...), set_property_from_str(...), and the rest of the
// pipeline/appsink plumbing below -- has been checked directly against
// the pinned gstreamer-rs 0.23 docs and, for the appsink callback shape
// specifically, cross-checked against the official gstreamer-rs
// project's own utility code and a separate real-world production
// codebase (Servo's media backend) using the identical pattern. That
// resolves the "does this even compile and call the right methods"
// uncertainty this note used to carry.
//
// The XDG camera portal path (try_portal_camera_fd, below) is newer and
// carries its own, separate confidence note: the ashpd API shape itself
// (Camera::new().await?, .is_present().await?, .request_access(...),
// .open_pipe_wire_remote(...)) is confirmed directly against ashpd's own
// current README, docs.rs page, and crates.io listing (version 0.13.12)
// -- not guessed. What's NOT independently confirmed here: the exact
// return type of open_pipe_wire_remote() (assumed std::os::fd::OwnedFd,
// the standard modern Rust type for this, and the only shape that makes
// the rest of this code's own ownership-transfer-to-pipewiresrc logic
// correct) and pipewiresrc's own fd property's exact ownership
// semantics (assumed to take ownership of a raw fd set via a
// gst-launch-style pipeline string, the standard GStreamer convention
// for element-owned resources set this way). Both are reasonable,
// standard assumptions, not confirmed against a real, compiled build
// the way the rest of this file's own API surface has been.
//
// What's still genuinely unverified, and can't be resolved the same
// way: everything about actual runtime behavior on real hardware --
// whether droidcamsrc actually negotiates and captures a frame on a
// real FLX1S, whether the NV21/640x480/30fps assumptions in
// build_pipeline_string below are actually correct for that camera,
// whether detect_camera_source's own priority order picks the right
// element on a given device, and generally whether this pipeline
// produces a usable image at all. Treat the pipeline's own hardware-
// facing assumptions as a solid starting shape, not a guaranteed-
// working one -- the API calls themselves are no longer the risk here,
// the hardware is.
//
// Camera access on Linux phones is genuinely fragmented across
// hardware/OS combinations -- there's no one GStreamer source element
// that works everywhere:
//   - Halium/UBports-lineage devices (confirmed for the FuriLabs FLX1/
//     FLX1S specifically, by reading FuriLabs' own camera app source --
//     it depends on `qt5-cameraplugin-aal` and `gstreamer1.0-droid`)
//     bridge the Android camera HAL via libhybris, and the matching
//     GStreamer element is `droidcamsrc` (from the gst-droid plugin,
//     package gstreamer1.0-droid on Debian-family systems). It's also
//     known to be picky about caps -- real-world reports show
//     negotiation failures ("failed to negotiate vidsrc") when caps
//     aren't given explicitly, hence the explicit caps below rather
//     than a bare `droidcamsrc ! videoconvert ! ...`.
//   - More conventional postmarketOS/Mobian-style devices (PinePhone,
//     Librem5) generally go through libcamera (`libcamerasrc`) or
//     PipeWire's camera portal (`pipewiresrc`) instead -- see the
//     Megapixels project for how deep this rabbit hole goes there.
//   - A plain UVC-ish v4l2 device (`v4l2src`) is the lowest-common-
//     denominator fallback.
// try_portal_camera_fd(), below, is tried first, ahead of any of this --
// see its own doc comment for why. detect_camera_source() below is the
// fallback for whenever that isn't available at all, and picks
// whichever of the four elements above is actually registered as a
// GStreamer plugin on the running system, in a best-effort priority
// order -- not a guaranteed-correct one, since I
// can't verify this against real hardware for any of these devices.
//
// A second, unrelated risk: the gtk4paintablesink element requires the
// `gstreamer1.0-gtk4` plugin package (Debian/Ubuntu-family naming) to be
// installed system-wide -- it's not something Cargo can pull in.
//
// THREADING NOTE: pipeline construction and set_state(Playing) both run
// on a background thread, not the GTK main thread (see open_scan_dialog
// below) -- some of these source elements are known to block
// synchronously for a while during their own startup (binder/HAL
// round-trips for droidcamsrc, PipeWire registry negotiation for
// pipewiresrc), and doing that on the main thread would freeze the
// whole app's UI for however long that takes, not just the scan dialog.
// Separately, AppSink::new_sample runs on GStreamer's own streaming
// thread once the pipeline is playing, and gstreamer-app requires that
// callback to be `Send`. GTK/libadwaita objects (and anything capturing
// them, like `on_detected`) are emphatically not thread-safe, so none of
// that can be touched from either background context. The pattern used
// throughout this file: background work only writes into a plain
// `Arc<Mutex<...>>` (thread-safe by construction), and a `glib` timeout
// source on the *main* thread polls that slot and is the only place
// that ever touches GTK or calls `on_detected`.
// -----------------------------------------------------------------------

/// Cheap heuristic for whether to grey out the "Scan with Camera" menu
/// item. Doesn't guarantee GStreamer can actually open the device (that
/// also depends on permissions and how the device routes camera access),
/// just that something camera-shaped exists.
pub fn camera_available() -> bool {
    std::fs::read_dir("/dev")
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with("video"))
        })
        .unwrap_or(false)
}

/// Candidate GStreamer camera source elements, in priority order. Must
/// call gst::init() before this is checked (ElementFactory::find() looks
/// up the plugin registry, which init() populates).
const CAMERA_SOURCE_CANDIDATES: &[&str] =
    &["droidcamsrc", "libcamerasrc", "pipewiresrc", "v4l2src"];

/// Picks the first candidate source whose GStreamer plugin is actually
/// registered on this system, rather than assuming any one of them.
/// `None` if none of them are available at all.
fn detect_camera_source() -> Option<&'static str> {
    CAMERA_SOURCE_CANDIDATES
        .iter()
        .find(|name| gst::ElementFactory::find(name).is_some())
        .copied()
}

/// Tries the XDG camera portal (org.freedesktop.portal.Camera, via
/// ashpd) first -- the "proper," most privacy-preserving way to get
/// camera access, and, on this app's own actual target hardware (a
/// FuriLabs FLX1S), the only way confirmed to give genuinely good,
/// real-time performance inside a Flatpak sandbox at all. Raw
/// pipewiresrc -- letting it discover the PipeWire daemon on its own,
/// rather than through the portal -- was directly, empirically confirmed
/// severely slow on the same, real hardware (roughly 2 frames per
/// minute, degrading further from there), even with a working,
/// GPU-accelerated GTK renderer already confirmed and forced (ruling out
/// rendering as the cause) -- whereas a real, portal-using Flatpak app
/// on the same device (GNOME's own "Decoder" QR-code scanner) was
/// directly confirmed fast. `None` on any failure (not running inside a
/// sandbox with a portal backend installed, no camera portal support at
/// all, the user declined the consent prompt, etc.) -- callers should
/// fall back to the existing, direct GStreamer-element approach
/// (detect_camera_source) in that case, matching this file's own
/// established "try the best option, fall through on failure" pattern
/// (see brightness.rs's own, similar multi-tier fallback for the same
/// philosophy applied to a different feature).
///
/// Run via async_io::block_on rather than a persistent async runtime --
/// this function is called from open_scan_dialog's own, existing
/// background thread (see the THREADING NOTE at the top of this file),
/// so blocking that one thread for the duration of this single, one-shot
/// portal request is fine -- it's already expected to block for a while
/// during camera startup regardless.
fn try_portal_camera_fd() -> Option<std::os::fd::OwnedFd> {
    async_io::block_on(async {
        let camera = ashpd::desktop::camera::Camera::new().await.ok()?;
        if !camera.is_present().await.unwrap_or(false) {
            return None;
        }
        camera.request_access(Default::default()).await.ok()?;
        camera.open_pipe_wire_remote(Default::default()).await.ok()
    })
}

fn orientation_to_videoflip_method(orientation: &str) -> &'static str {
    match orientation {
        "normal" => "none",
        "left-up" => "clockwise",
        "right-up" => "counterclockwise",
        "bottom-up" => "rotate-180",
        _ => "none",
    }
}

/// One-shot AccelerometerOrientation read over iio-sensor-proxy's system
/// bus service (net.hadess.SensorProxy) -- used only to pick a sane
/// initial videoflip method before the pipeline starts. Live tracking
/// while the scan dialog is open (see spawn_orientation_tracker) is what
/// actually keeps the preview correct as the phone is rotated -- a
/// confirmed real requirement, not a hypothetical: an earlier version of
/// this only ever queried orientation once, at dialog-open, and so never
/// noticed the phone being rotated afterward, which is exactly what was
/// reported. "none" on any failure (service not running, claim failed,
/// etc).
fn detect_preview_rotation() -> &'static str {
    (|| -> Option<&'static str> {
        let conn = zbus::blocking::Connection::system().ok()?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "net.hadess.SensorProxy",
            "/net/hadess/SensorProxy",
            "net.hadess.SensorProxy",
        )
        .ok()?;
        proxy.call::<_, _, ()>("ClaimAccelerometer", &()).ok()?;
        let orientation = proxy.get_property::<String>("AccelerometerOrientation").ok()?;
        let _: Result<(), _> = proxy.call("ReleaseAccelerometer", &());
        Some(orientation_to_videoflip_method(&orientation))
    })()
    .unwrap_or("none")
}

/// Claims the accelerometer for the lifetime of the scan dialog and
/// polls AccelerometerOrientation on a background thread (D-Bus round
/// trips off the GTK main thread, same reasoning as the pipeline-startup
/// THREADING NOTE at the top of this file), updating `videoflip`'s
/// `method` property live -- via a main-thread timer, since GTK/GStreamer
/// property writes should happen from a consistent thread rather than
/// racing a background one -- whenever the orientation actually changes.
/// `should_stop` is checked once per poll interval and is what lets the
/// background thread notice the dialog closed and release the
/// accelerometer instead of holding it claimed indefinitely.
fn spawn_orientation_tracker(videoflip: gst::Element, dialog_weak: glib::WeakRef<adw::Dialog>) {
    let latest: Arc<Mutex<&'static str>> = Arc::new(Mutex::new("none"));
    let should_stop = Arc::new(AtomicBool::new(false));

    {
        let latest = latest.clone();
        let should_stop = should_stop.clone();
        std::thread::spawn(move || {
            let tracked: Option<(zbus::blocking::Connection, zbus::blocking::Proxy)> = (|| {
                let conn = zbus::blocking::Connection::system().ok()?;
                let proxy = zbus::blocking::Proxy::new(
                    &conn,
                    "net.hadess.SensorProxy",
                    "/net/hadess/SensorProxy",
                    "net.hadess.SensorProxy",
                )
                .ok()?;
                proxy.call::<_, _, ()>("ClaimAccelerometer", &()).ok()?;
                Some((conn, proxy))
            })();

            let Some((_conn, proxy)) = tracked else {
                return;
            };

            while !should_stop.load(Ordering::SeqCst) {
                if let Ok(orientation) = proxy.get_property::<String>("AccelerometerOrientation") {
                    *latest.lock().unwrap() = orientation_to_videoflip_method(&orientation);
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
            }

            let _: Result<(), _> = proxy.call("ReleaseAccelerometer", &());
        });
    }

    let applied: std::cell::Cell<&'static str> = std::cell::Cell::new("none");
    glib::source::timeout_add_local(std::time::Duration::from_millis(150), move || {
        if dialog_weak.upgrade().is_none() {
            should_stop.store(true, Ordering::SeqCst);
            return glib::ControlFlow::Break;
        }
        let current = *latest.lock().unwrap();
        if current != applied.get() {
            applied.set(current);
            // set_property_from_str: gstreamer-rs's convenience wrapper
            // for setting an enum/flags-typed property (videoflip's
            // "method" here) from its string nick, the same value shape
            // gst-launch pipeline syntax uses -- matches how "method"
            // is already set in build_pipeline_string's own pipeline
            // string. Confirmed as a real method on any IsA<glib::Object>
            // (which gst::Element satisfies) -- see this file's own
            // top-of-file confidence note for what's been verified.
            videoflip.set_property_from_str("method", current);
        }
        glib::ControlFlow::Continue
    });
}

/// Which strategy produced the camera source stage of the pipeline --
/// see try_portal_camera_fd's own doc comment for why the portal path
/// is tried first, and detect_camera_source's own doc comment for the
/// existing, direct fallback. A distinct type from a plain &str
/// specifically so a portal fd can never be confused with a bare
/// element name at the type level.
enum CameraSource {
    /// A PipeWire remote fd obtained through the XDG camera portal.
    /// Ownership of the fd transfers to pipewiresrc once embedded in the
    /// pipeline string below -- see this file's own top-of-file
    /// confidence note for what's confirmed vs assumed about that
    /// transfer.
    Portal(std::os::fd::OwnedFd),
    /// A GStreamer element name picked by detect_camera_source's own
    /// plugin-registry check (droidcamsrc, libcamerasrc, pipewiresrc,
    /// or v4l2src).
    Direct(&'static str),
}

/// Builds the full pipeline description for a given source. droidcamsrc
/// needs its own branch: `mode=2` selects video-capture mode (as opposed
/// to still-image mode), and it needs explicit caps right on its own
/// output -- real-world reports of this element show it failing to
/// negotiate a format at all otherwise. The NV21/width/height/framerate
/// values below are a reasonable starting guess for a phone back camera,
/// not confirmed against FLX1S hardware -- likely the single most
/// probable thing to need adjusting on-device, alongside mode itself
/// (some gst-droid versions use different mode numbers for front vs
/// back camera or video vs image capture).
///
/// videoflip is only on the preview leg, not the scan leg -- the
/// barcode decoder already tries all 4 rotations blind (see
/// barcode::decode_luma), so it doesn't need or benefit from this;
/// applying it there too would just be extra work for no gain. Named
/// (`name=rotate`) so spawn_orientation_tracker can look it up and
/// update its `method` property live after the pipeline is playing --
/// the value baked in here via detect_preview_rotation() is only a
/// reasonable starting point for however the phone happens to be held
/// when the dialog opens, not the final word.
fn build_pipeline_string(source: CameraSource) -> String {
    let source_stage = match source {
        // into_raw_fd deliberately does not close the fd on drop --
        // ownership is meant to transfer to pipewiresrc here, not be
        // released back to Rust's own allocator. See this file's own
        // top-of-file confidence note: this is the standard GStreamer
        // convention for an fd-typed property set this way, not
        // independently confirmed against a real, compiled build.
        CameraSource::Portal(fd) => {
            format!("pipewiresrc fd={}", fd.into_raw_fd())
        }
        CameraSource::Direct("droidcamsrc") => {
            "droidcamsrc mode=2 ! video/x-raw,format=NV21,width=640,height=480,framerate=30/1"
                .to_string()
        }
        CameraSource::Direct(name) => name.to_string(),
    };
    let rotation = detect_preview_rotation();

    format!(
        "{source_stage} ! videoconvert ! tee name=t \
         t. ! queue ! videoflip name=rotate method={rotation} ! gtk4paintablesink name=preview_sink \
         t. ! queue ! videoconvert ! video/x-raw,format=GRAY8 ! \
         appsink name=scan_sink emit-signals=true max-buffers=1 drop=true"
    )
}


enum PipelineOutcome {
    Ready(gst::Pipeline),
    Error(String),
}

/// Opens a modal scanning dialog with a live camera preview. Calls
/// `on_detected(value, format)` the moment a barcode/QR is found in a
/// frame, then closes itself. `on_error` is called instead if GStreamer
/// can't be started at all (not installed, plugin missing, camera busy).
pub fn open_scan_dialog(
    window: &adw::ApplicationWindow,
    on_detected: impl Fn(String, BarcodeType) + 'static,
    on_error: impl Fn(String) + 'static,
) {
    let status_label = gtk::Label::new(Some("Starting camera\u{2026}"));
    status_label.add_css_class("dim-label");
    let spinner = gtk::Spinner::builder().spinning(true).build();

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_valign(gtk::Align::Center);
    vbox.append(&spinner);
    vbox.append(&status_label);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Scan Code", "")));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&vbox));

    let dialog = adw::Dialog::builder()
        .title("Scan Code")
        .content_width(420)
        .content_height(480)
        .child(&toolbar)
        .build();
    dialog.present(Some(window));

    // Pipeline construction and set_state(Playing) both happen here, off
    // the GTK main thread -- see the THREADING NOTE at the top of this
    // file for why. The dialog above is already showing a loading state
    // by the time this spawns, so a slow camera backend delays only the
    // preview appearing, not the whole app.
    let outcome: Arc<Mutex<Option<PipelineOutcome>>> = Arc::new(Mutex::new(None));
    {
        let outcome = outcome.clone();
        std::thread::spawn(move || {
            if let Err(e) = gst::init() {
                *outcome.lock().unwrap() = Some(PipelineOutcome::Error(format!(
                    "GStreamer failed to initialize: {e}"
                )));
                return;
            }

            // Portal tried first -- see try_portal_camera_fd's own doc
            // comment for why. pipewiresrc itself still needs to be an
            // installed plugin for this path to make sense at all (it's
            // what actually consumes the portal-provided fd) -- gated on
            // that directly rather than letting a missing plugin surface
            // later as an opaque gst::parse::launch failure.
            let source = if gst::ElementFactory::find("pipewiresrc").is_some() {
                try_portal_camera_fd().map(CameraSource::Portal)
            } else {
                None
            };
            let source = match source {
                Some(source) => source,
                None => match detect_camera_source() {
                    Some(name) => CameraSource::Direct(name),
                    None => {
                        *outcome.lock().unwrap() = Some(PipelineOutcome::Error(
                            "No supported camera source found on this system (checked the \
                             XDG camera portal, droidcamsrc, libcamerasrc, pipewiresrc, \
                             v4l2src -- none of those are available)."
                                .to_string(),
                        ));
                        return;
                    }
                },
            };

            // Captured before build_pipeline_string consumes `source`
            // below -- CameraSource::Portal owns a real fd (not Copy),
            // so `source` itself can't still be referenced afterward for
            // the error messages below; this plain label can.
            let source_label = match &source {
                CameraSource::Portal(_) => "XDG camera portal",
                CameraSource::Direct(name) => name,
            };
            let pipeline_str = build_pipeline_string(source);

            let element = match gst::parse::launch(&pipeline_str) {
                Ok(p) => p,
                Err(e) => {
                    *outcome.lock().unwrap() = Some(PipelineOutcome::Error(format!(
                        "Couldn't build camera pipeline ({source_label}): {e}"
                    )));
                    return;
                }
            };
            let pipeline = match element.downcast::<gst::Pipeline>() {
                Ok(p) => p,
                Err(_) => {
                    *outcome.lock().unwrap() = Some(PipelineOutcome::Error(
                        "Internal error: parsed pipeline wasn't a Pipeline".to_string(),
                    ));
                    return;
                }
            };

            // The call most likely to block for a while, depending on
            // the source picked above -- see the THREADING NOTE. Fine
            // to block this background thread; the point of running it
            // here is that it's *not* the GTK main thread.
            if let Err(e) = pipeline.set_state(gst::State::Playing) {
                *outcome.lock().unwrap() = Some(PipelineOutcome::Error(format!(
                    "Couldn't start the camera ({source_label}): {e}"
                )));
                return;
            }

            *outcome.lock().unwrap() = Some(PipelineOutcome::Ready(pipeline));
        });
    }

    // Polls every 50ms for the background thread's result -- short
    // enough to feel prompt once the camera's actually ready, long
    // enough not to spin a CPU core busy-waiting for however long
    // camera startup takes.
    //
    // on_detected/on_error are wrapped in Option here because
    // timeout_add_local requires a repeatable FnMut, but this closure
    // only actually needs to use either of them once (it always returns
    // Break right after) -- moving them out of the closure's own
    // captured environment directly (e.g. straight into
    // finish_scan_dialog) isn't allowed for FnMut, since the type has to
    // stay valid to call again even though in practice it never is.
    // Option::take() sidesteps that: it's a mutation through &mut self
    // (which FnMut permits), not a move out of the environment.
    let dialog_weak = dialog.downgrade();
    let mut on_detected = Some(on_detected);
    let mut on_error = Some(on_error);
    glib::source::timeout_add_local(std::time::Duration::from_millis(50), move || {
        let Some(found) = outcome.lock().unwrap().take() else {
            return glib::ControlFlow::Continue;
        };

        match found {
            PipelineOutcome::Error(message) => {
                if let Some(dialog) = dialog_weak.upgrade() {
                    dialog.close();
                }
                if let Some(on_error) = on_error.take() {
                    on_error(message);
                }
            }
            PipelineOutcome::Ready(pipeline) => {
                let Some(dialog) = dialog_weak.upgrade() else {
                    // Spawned rather than called directly -- see
                    // dialog.connect_closed's own, similar fix further
                    // below for the full reasoning. Lower risk here
                    // specifically (this only runs once the pipeline has
                    // already, genuinely reached Playing), but moved for
                    // consistency and defense in depth regardless.
                    std::thread::spawn(move || {
                        let _ = pipeline.set_state(gst::State::Null);
                    });
                    return glib::ControlFlow::Break;
                };
                if let (Some(on_detected), Some(on_error)) = (on_detected.take(), on_error.take())
                {
                    finish_scan_dialog(&dialog, pipeline, on_detected, on_error);
                }
            }
        }

        glib::ControlFlow::Break
    });
}

/// Replaces the dialog's "Starting camera..." placeholder with the real
/// live preview + barcode scanner, once open_scan_dialog's background
/// thread has confirmed the pipeline is actually already playing (this
/// function does not call set_state itself -- that already happened).
fn finish_scan_dialog(
    dialog: &adw::Dialog,
    pipeline: gst::Pipeline,
    on_detected: impl Fn(String, BarcodeType) + 'static,
    on_error: impl Fn(String) + 'static,
) {
    let preview_sink_element = match pipeline.by_name("preview_sink") {
        Some(e) => e,
        None => {
            on_error("Internal error: pipeline has no 'preview_sink' element".to_string());
            dialog.close();
            return;
        }
    };
    let paintable = preview_sink_element.property::<gtk::gdk::Paintable>("paintable");

    // Keeps the preview upright as the phone is physically rotated
    // while the dialog is open -- see spawn_orientation_tracker's own
    // doc comment. Missing "rotate" element would mean
    // build_pipeline_string's pipeline description changed without
    // this call site being updated to match; not fatal on its own (the
    // preview would just stay at whichever rotation it started with),
    // so this only skips live tracking rather than failing the whole
    // dialog.
    if let Some(videoflip) = pipeline.by_name("rotate") {
        spawn_orientation_tracker(videoflip, dialog.downgrade());
    }

    let picture = gtk::Picture::builder()
        .paintable(&paintable)
        .content_fit(gtk::ContentFit::Contain)
        .height_request(320)
        .build();

    let status_label = gtk::Label::new(Some("Point the camera at a barcode or QR code"));
    status_label.add_css_class("dim-label");

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.append(&picture);
    vbox.append(&status_label);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Scan Code", "")));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&vbox));
    dialog.set_child(Some(&toolbar));

    // Stop the pipeline whenever the dialog goes away, however it happens
    // (found a code, or the person just backed out). Genuinely thread-safe
    // to hold/clone a Pipeline across threads (unlike GTK objects) -- but
    // thread-safe isn't the same as fast: if this fires while the
    // earlier, background-thread set_state(Playing) call from
    // open_scan_dialog is still genuinely in progress (a real,
    // confirmed possibility -- droidcamsrc in particular can take a
    // long time to start), GStreamer's own state-change machinery has
    // to serialize the two, and a synchronous set_state(Null) call
    // right here, on the main thread, would block the entire UI --
    // and, since the main thread can never then return to let the
    // process exit, the whole app -- for however long that takes. Real,
    // directly reported symptom this fixes: the app freezing and the
    // process outliving the window after the dialog was closed while
    // the camera was still starting up. Spawned onto its own thread
    // instead so the main thread is never, genuinely blocked by it,
    // matching this file's own established pattern for GStreamer calls
    // that might take a while (see the THREADING NOTE at the top of
    // this file).
    {
        let pipeline = pipeline.clone();
        dialog.connect_closed(move |_| {
            let pipeline = pipeline.clone();
            std::thread::spawn(move || {
                let _ = pipeline.set_state(gst::State::Null);
            });
        });
    }

    let appsink = match pipeline
        .by_name("scan_sink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
    {
        Some(a) => a,
        None => {
            on_error("Internal error: pipeline has no 'scan_sink' AppSink".to_string());
            dialog.close();
            return;
        }
    };

    // Thread-safe hand-off slot: written from the GStreamer streaming
    // thread inside `new_sample`, read from the main thread's idle poll.
    let result_slot: Arc<Mutex<Option<(String, BarcodeType)>>> = Arc::new(Mutex::new(None));
    let already_found = Arc::new(AtomicBool::new(false));

    {
        let result_slot = result_slot.clone();
        let already_found = already_found.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    // Everything captured above (result_slot,
                    // already_found) is Send/Sync -- deliberately nothing
                    // GTK-related is touched in this closure.
                    if already_found.load(Ordering::SeqCst) {
                        return Ok(gst::FlowSuccess::Ok);
                    }
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                    let info = gstreamer_video::VideoInfo::from_caps(caps)
                        .map_err(|_| gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                    if let Some((value, format)) =
                        barcode::decode_luma(map.as_slice().to_vec(), info.width(), info.height())
                    {
                        already_found.store(true, Ordering::SeqCst);
                        // Deliberately NOT calling pipeline.set_state()
                        // here -- see the THREADING NOTE at the top of
                        // this file. This callback runs ON one of the
                        // pipeline's own streaming threads; calling
                        // set_state(Null) synchronously from inside it
                        // makes the state-change machinery try to join
                        // that same thread from within itself, which
                        // deadlocks (this was the actual cause of the
                        // reported total app lockup + runaway memory:
                        // once deadlocked, the pipeline can never
                        // actually stop, so buffers keep queuing up
                        // with nothing draining them). Only writing to
                        // result_slot here and letting the main-thread
                        // poll below do the state change instead -- a
                        // different thread than the one whose state is
                        // being changed -- avoids that entirely.
                        *result_slot.lock().unwrap() = Some((value, format));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    // Main-thread poll: this closure is NOT required to be Send (it's
    // `idle_add_local`, tied to the calling thread's MainContext, which
    // is the GTK main thread here), so it's free to capture `on_detected`
    // and the dialog directly.
    {
        let result_slot = result_slot.clone();
        let dialog_weak = dialog.downgrade();
        let pipeline = pipeline.clone();
        glib::source::idle_add_local(move || {
            let found = result_slot.lock().unwrap().take();
            match found {
                Some((value, format)) => {
                    // Stopping the pipeline here, not in the appsink
                    // callback above, is the actual fix -- see the
                    // comment there for why. Spawned rather than called
                    // directly -- see dialog.connect_closed's own,
                    // similar fix further above for the full reasoning.
                    // Lower risk here specifically (this only runs once
                    // the pipeline has already, genuinely reached
                    // Playing), but moved for consistency and defense
                    // in depth regardless.
                    {
                        let pipeline = pipeline.clone();
                        std::thread::spawn(move || {
                            let _ = pipeline.set_state(gst::State::Null);
                        });
                    }
                    on_detected(value, format);
                    if let Some(dialog) = dialog_weak.upgrade() {
                        dialog.close();
                    }
                    glib::ControlFlow::Break
                }
                None => glib::ControlFlow::Continue,
            }
        });
    }
}

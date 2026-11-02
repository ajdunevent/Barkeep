use adw::prelude::*;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gtk::{gio, glib};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use barkeep_core::barcode;
use barkeep_core::card::BarcodeType;

// Camera access on Linux phones is fragmented across hardware/OS
// combinations, no one GStreamer source works everywhere:
//   - Halium/UBports-lineage devices, including the FLX1/FLX1S (per
//     FuriLabs' own camera app source, which depends on
//     `qt5-cameraplugin-aal` and `gstreamer1.0-droid`), bridge the
//     Android camera HAL via libhybris; the matching element is
//     `droidcamsrc`. It's picky about caps, failing to negotiate
//     ("failed to negotiate vidsrc") without them given explicitly.
//   - postmarketOS/Mobian-style devices (PinePhone, Librem5) generally
//     go through libcamera (`libcamerasrc`) or PipeWire's camera
//     portal (`pipewiresrc`) instead.
//   - A plain UVC-ish v4l2 device (`v4l2src`) is the fallback.
// detect_camera_source() picks whichever element is actually
// registered on the system, in this priority order.
//
// The gtk4paintablesink element requires the `gstreamer1.0-gtk4`
// plugin package installed system-wide; Cargo can't pull it in.
//
// THREADING NOTE: pipeline construction and set_state(Playing) both
// run on a background thread, not the GTK main thread, since some
// source elements block synchronously for a while during startup
// (binder/HAL round-trips for droidcamsrc, PipeWire registry
// negotiation for pipewiresrc) and doing that on the main thread would
// freeze the whole app's UI. AppSink::new_sample runs on GStreamer's
// own streaming thread once playing, and gstreamer-app requires that
// callback to be `Send`; GTK/libadwaita objects aren't thread-safe, so
// neither background context can touch them. Background work only
// writes into a plain `Arc<Mutex<...>>`, and a `glib` timeout source
// on the main thread polls that slot and is the only place that
// touches GTK or calls `on_detected`.

/// Cheap heuristic for whether to grey out the "Scan with Camera" menu
/// item. Doesn't guarantee GStreamer can actually open the device,
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

fn orientation_to_videoflip_method(orientation: &str) -> &'static str {
    match orientation {
        "normal" => "none",
        "left-up" => "clockwise",
        "right-up" => "counterclockwise",
        "bottom-up" => "rotate-180",
        _ => "none",
    }
}

const SENSOR_PROXY_NAME: &str = "net.hadess.SensorProxy";
const SENSOR_PROXY_PATH: &str = "/net/hadess/SensorProxy";

fn sensor_proxy_call(
    conn: &gio::DBusConnection,
    interface: &str,
    method: &str,
    parameters: Option<&glib::Variant>,
) -> Option<glib::Variant> {
    conn.call_sync(
        Some(SENSOR_PROXY_NAME),
        SENSOR_PROXY_PATH,
        interface,
        method,
        parameters,
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
    )
    .ok()
}

/// Reads the property directly each time. gio::DBusProxy only caches
/// properties and refreshes them from the main loop, which the polling
/// thread in spawn_orientation_tracker doesn't have.
fn read_accelerometer_orientation(conn: &gio::DBusConnection) -> Option<String> {
    let reply = sensor_proxy_call(
        conn,
        "org.freedesktop.DBus.Properties",
        "Get",
        Some(&(SENSOR_PROXY_NAME, "AccelerometerOrientation").to_variant()),
    )?;
    let (value,) = reply.get::<(glib::Variant,)>()?;
    value.str().map(str::to_owned)
}

/// One-shot AccelerometerOrientation read over iio-sensor-proxy, used
/// only to pick a starting videoflip method before the pipeline
/// starts. Live tracking (spawn_orientation_tracker) is what keeps the
/// preview correct as the phone rotates: an earlier version only
/// queried orientation once at dialog-open and never noticed rotation
/// afterward. "none" on any failure.
fn detect_preview_rotation() -> &'static str {
    (|| -> Option<&'static str> {
        let conn = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).ok()?;
        sensor_proxy_call(&conn, SENSOR_PROXY_NAME, "ClaimAccelerometer", None)?;
        let orientation = read_accelerometer_orientation(&conn);
        sensor_proxy_call(&conn, SENSOR_PROXY_NAME, "ReleaseAccelerometer", None);
        Some(orientation_to_videoflip_method(&orientation?))
    })()
    .unwrap_or("none")
}

/// Claims the accelerometer for the lifetime of the scan dialog and
/// polls AccelerometerOrientation on a background thread, updating
/// `videoflip`'s `method` property live via a main-thread timer.
/// `should_stop` lets the background thread notice the dialog closed
/// and release the accelerometer.
fn spawn_orientation_tracker(videoflip: gst::Element, dialog_weak: glib::WeakRef<adw::Dialog>) {
    let latest: Arc<Mutex<&'static str>> = Arc::new(Mutex::new("none"));
    let should_stop = Arc::new(AtomicBool::new(false));

    {
        let latest = latest.clone();
        let should_stop = should_stop.clone();
        std::thread::spawn(move || {
            let Some(conn) = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).ok() else {
                return;
            };
            if sensor_proxy_call(&conn, SENSOR_PROXY_NAME, "ClaimAccelerometer", None).is_none() {
                return;
            }

            while !should_stop.load(Ordering::SeqCst) {
                if let Some(orientation) = read_accelerometer_orientation(&conn) {
                    *latest.lock().unwrap() = orientation_to_videoflip_method(&orientation);
                }
                std::thread::sleep(std::time::Duration::from_millis(400));
            }

            sensor_proxy_call(&conn, SENSOR_PROXY_NAME, "ReleaseAccelerometer", None);
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
            // Sets videoflip's "method" from its string nick, matching
            // how build_pipeline_string sets it initially.
            videoflip.set_property_from_str("method", current);
        }
        glib::ControlFlow::Continue
    });
}

/// Builds the full pipeline description for a given source element.
/// droidcamsrc needs its own branch: `mode=2` selects video-capture
/// mode, and it needs explicit caps on its output or it fails to
/// negotiate a format. NV21/640x480/30fps works for the FLX1S's back
/// camera; a different device may need different values, and some
/// gst-droid versions use different mode numbers for front vs back
/// camera or video vs image capture.
///
/// videoflip is only on the preview leg, not the scan leg: the
/// barcode decoder already tries all 4 rotations blind (see
/// barcode::decode_luma). Named (`name=rotate`) so
/// spawn_orientation_tracker can look it up and update `method` live
/// after the pipeline is playing; the value baked in via
/// detect_preview_rotation() is only a starting point.
fn build_pipeline_string(source: &str) -> String {
    let source_stage = if source == "droidcamsrc" {
        "droidcamsrc mode=2 ! video/x-raw,format=NV21,width=640,height=480,framerate=30/1"
            .to_string()
    } else {
        source.to_string()
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

    // Pipeline construction and set_state(Playing) both happen off the
    // GTK main thread. The dialog above already shows a loading state
    // by the time this spawns, so a slow camera backend only delays
    // the preview, not the whole app.
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

            let Some(source) = detect_camera_source() else {
                *outcome.lock().unwrap() = Some(PipelineOutcome::Error(
                    "No supported camera source found on this system (checked droidcamsrc, \
                     libcamerasrc, pipewiresrc, v4l2src -- none of those GStreamer plugins \
                     appear to be installed)."
                        .to_string(),
                ));
                return;
            };

            let pipeline_str = build_pipeline_string(source);

            let element = match gst::parse::launch(&pipeline_str) {
                Ok(p) => p,
                Err(e) => {
                    *outcome.lock().unwrap() = Some(PipelineOutcome::Error(format!(
                        "Couldn't build camera pipeline ({source}): {e}"
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
            // the source. Fine to block this background thread.
            if let Err(e) = pipeline.set_state(gst::State::Playing) {
                *outcome.lock().unwrap() = Some(PipelineOutcome::Error(format!(
                    "Couldn't start the camera ({source}): {e}"
                )));
                return;
            }

            *outcome.lock().unwrap() = Some(PipelineOutcome::Ready(pipeline));
        });
    }

    // Polls every 50ms for the background thread's result.
    //
    // on_detected/on_error are wrapped in Option since
    // timeout_add_local requires a repeatable FnMut, but this closure
    // only needs to use either once. Option::take() is a mutation
    // through &mut self, which FnMut permits, rather than a move out
    // of the environment.
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
                    // Spawned onto its own thread rather than called
                    // directly; see dialog.connect_closed below for
                    // why (same fix, lower risk here since the
                    // pipeline has already reached Playing).
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

/// Replaces the dialog's "Starting camera..." placeholder with the
/// real live preview and barcode scanner, once open_scan_dialog's
/// background thread confirms the pipeline is already playing (this
/// function doesn't call set_state itself).
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

    // Keeps the preview upright as the phone rotates while the dialog
    // is open (see spawn_orientation_tracker). Missing "rotate" means
    // build_pipeline_string changed without this call site being
    // updated; not fatal, just skips live tracking.
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

    // Stop the pipeline whenever the dialog goes away. Pipeline is
    // thread-safe to hold/clone across threads (unlike GTK objects),
    // but a synchronous set_state(Null) here, on the main thread,
    // would block the whole app if it races the background-thread
    // set_state(Playing) from open_scan_dialog (droidcamsrc in
    // particular can take a long time to start): GStreamer's
    // state-change machinery has to serialize the two, and the main
    // thread can't return to let the process exit while blocked. This
    // fixes a real symptom: the app freezing and outliving the window
    // after the dialog was closed while the camera was still starting.
    // Spawned onto its own thread so the main thread is never blocked.
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
                    // result_slot, already_found are Send/Sync;
                    // deliberately nothing GTK-related is touched in
                    // this closure.
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
                        // here. This callback runs on one of the
                        // pipeline's own streaming threads; calling
                        // set_state(Null) synchronously from inside it
                        // makes the state-change machinery try to join
                        // that same thread from within itself, which
                        // deadlocks. This was the actual cause of a
                        // reported total app lockup and runaway memory:
                        // once deadlocked, the pipeline can never stop,
                        // so buffers keep queuing with nothing draining
                        // them. Only writing to result_slot here and
                        // letting the main-thread poll below do the
                        // state change, from a different thread than
                        // the one whose state is changing, avoids that.
                        *result_slot.lock().unwrap() = Some((value, format));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
    }

    // Main-thread poll: this closure is NOT required to be Send (it's
    // `idle_add_local`, tied to the calling thread's MainContext, the
    // GTK main thread here), so it's free to capture `on_detected` and
    // the dialog directly.
    {
        let result_slot = result_slot.clone();
        let dialog_weak = dialog.downgrade();
        let pipeline = pipeline.clone();
        glib::source::idle_add_local(move || {
            let found = result_slot.lock().unwrap().take();
            match found {
                Some((value, format)) => {
                    // Stopping the pipeline here, not in the appsink
                    // callback above, is the fix; see the comment
                    // there. Spawned rather than called directly, same
                    // reasoning as dialog.connect_closed above.
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

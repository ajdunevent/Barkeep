//! Shared, GTK-free logic for Barkeep.
//!
//! Split out of the main app into its own crate specifically so the
//! Phosh lock-screen plugin (a separate cdylib, running inside Phosh's
//! own process) can depend on the same card/group data model, JSON
//! loading, barcode rendering, and brightness control the main app
//! uses -- without either of them risking the two independently
//! drifting out of sync, which would be a real problem for anything
//! that reads/writes the same on-disk cards.json, or for the
//! brightness rate-limit specifically (see brightness::TOGGLE_COOLDOWN_MS's
//! own doc comment for why that one's safety-critical, not just
//! convenience).
//!
//! Deliberately has no gtk4/libadwaita/gstreamer dependency of any
//! kind: libphosh-rs (the plugin's own dependency for talking to
//! Phosh) pins an older gtk-rs version (^0.18) than the main app uses,
//! and gtk::Widget-family types from two different gtk-rs versions
//! aren't interchangeable even though they wrap the same underlying
//! GObject. Keeping this crate GTK-free avoids forcing both consumers
//! onto the same gtk-rs version. The one exception this couldn't fully
//! avoid: brightness::TOGGLE_COOLDOWN_MS is shared as a plain constant,
//! not the gtk::ToggleButton-touching mechanism around it -- each
//! consumer still writes its own few-line wrapper for that part.

pub mod barcode;
pub mod brightness;
pub mod card;

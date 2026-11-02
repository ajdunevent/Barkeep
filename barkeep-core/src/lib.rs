//! Shared, GTK-free logic for Barkeep.
//!
//! No GTK so lockscreen plugin, if attempted again, doesn't have a
//! gtk-rs version mismatch (libphosh-rs pins an older one than the
//! main app uses).

pub mod barcode;
pub mod card;

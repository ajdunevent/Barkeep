// Raised from the 128 default -- open_change_groups_dialog (window.rs)
// grew large and complex enough that a release build was hitting
// "reached the recursion limit while instantiating
// <Button as ButtonExt>::connect_clicked::<...>" at its pill's own
// click handler. This is a well-documented class of rustc limitation
// (a generic function's type parameter hitting an internal
// monomorphization depth limit during release-build optimization
// passes), not an actual infinite recursion anywhere in this code --
// the standard, low-risk fix is exactly this attribute, not a code
// change. If this function keeps growing, splitting it into smaller
// pieces is the more durable fix; this buys headroom in the meantime.
#![recursion_limit = "256"]

mod camera;
mod window;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

pub const APP_ID: &str = "net.dunevent.Barkeep";

fn main() -> glib::ExitCode {
    // Bundled icons not shipped in the system Adwaita theme (currently
    // just external-link-symbolic) -- compiled from resources/ by
    // build.rs. resources_register_include! is gio's own macro for
    // exactly this (include the build-script-compiled blob + register
    // it in one step) -- switched to it after a hand-rolled
    // include_bytes!(concat!(env!("OUT_DIR"), ...)) failed to compile
    // (env! reads its variable at the *invoking* crate's compile time,
    // and apparently didn't see OUT_DIR the way this macro does).
    // Makes the icons available at the /net/dunevent/Barkeep/...
    // paths declared in resources/barkeep.gresource.xml; adding that
    // as an icon search path below is what lets plain icon-name
    // lookups (e.g. gtk::Button::from_icon_name("external-link-symbolic"))
    // find it the same way they'd find a system-theme icon.
    gio::resources_register_include!("barkeep.gresource")
        .expect("failed to register bundled barkeep.gresource");

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::IconTheme::for_display(&display)
                .add_resource_path("/net/dunevent/Barkeep/icons");
        }
        let win = window::build_window(app);
        win.present();
    });
    app.run()
}

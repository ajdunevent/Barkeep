// Raised from 128. open_change_groups_dialog (window.rs) grew complex
// enough to hit rustc's monomorphization recursion limit in release
// builds at its pill's click handler. Not an actual infinite recursion.
// If this keeps happening, split that function up instead of raising
// this further.
#![recursion_limit = "256"]

mod camera;
mod window;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

pub const APP_ID: &str = "net.dunevent.Barkeep";

fn main() -> glib::ExitCode {
    // Bundled icons not shipped in the system Adwaita theme, compiled
    // from resources/ by build.rs. A hand-rolled
    // include_bytes!(concat!(env!("OUT_DIR"), ...)) failed to compile:
    // env! reads at the invoking crate's compile time, not the build
    // script's.
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

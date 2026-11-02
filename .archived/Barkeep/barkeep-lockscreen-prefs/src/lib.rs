//! Entry point for Phosh's own GIOModule loading mechanism -- prefs
//! variant. Structurally identical to barkeep-lockscreen's own lib.rs
//! (see that file's own doc comment for the full reasoning behind the
//! three required symbol names), just targeting
//! PHOSH_PLUGIN_EXTENSION_POINT_LOCKSCREEN_WIDGET_PREFS instead of the
//! plain widget extension point, and registering widget::PrefsDialog
//! rather than the lock-screen widget itself.
//!
//! This is a genuinely separate build artifact from barkeep-lockscreen
//! -- confirmed directly from the real ticket-box.desktop.in.in, whose
//! own [Prefs] section points at a distinct .so
//! (libphosh-plugin-prefs-@name@.so) from its own [Plugin] section
//! (libphosh-plugin-@name@.so). Not a mistake to have two crates here;
//! that's the real, working shape.
//!
//! Query symbol name corrected after a real, on-device diagnosis: see
//! barkeep-lockscreen's own lib.rs doc comment for the full reasoning
//! -- the real rule is "matches this crate's own actual, compiled .so
//! filename", confirmed against a real gio-querymodules cache dump, not
//! the C examples' own logical-plugin-name convention this file
//! originally (incorrectly) followed.

mod widget;

use gio::glib;
use glib::translate::IntoGlibPtr;

/// Same identifier as barkeep-lockscreen's own PLUGIN_NAME -- matches
/// the real ticket-box example, where both the main plugin and its own
/// prefs plugin use the same base name ("ticket-box") for their own,
/// separately-compiled -DPLUGIN_NAME/-DPLUGIN_PREFS_NAME C defines.
const PLUGIN_PREFS_NAME: &str = "barkeep";

/// Confirmed directly from plugins/meson.build's own phosh-plugin.h.in
/// substitution -- same source as barkeep-lockscreen's own
/// EXTENSION_POINT_LOCKSCREEN_WIDGET constant, this is the sibling
/// "_PREFS" extension point string from that same six-constant header.
const EXTENSION_POINT_LOCKSCREEN_WIDGET_PREFS: &str = "phosh-lockscreen-widget-prefs";

/// # Safety
/// Called by libgio via dlsym immediately after this .so is loaded,
/// exactly once, with a valid, non-null `module` pointer it owns.
/// Mirrors g_io_module_load(GIOModule *module) from
/// phosh-plugin-prefs-ticket-box.c's own real source.
#[no_mangle]
pub unsafe extern "C" fn g_io_module_load(module: *mut gio::ffi::GIOModule) {
    let type_module: glib::TypeModule = glib::translate::from_glib_none(module as *mut glib::gobject_ffi::GTypeModule);
    gio::prelude::TypeModuleExt::use_(&type_module);

    // register_dynamic_type (not the plain static_type() an earlier
    // version of this file called directly) -- see widget.rs's own
    // #[object_subclass_dynamic] comment for why a GIOModule-loaded
    // plugin type needs this specifically.
    gio::IOExtensionPoint::implement(
        EXTENSION_POINT_LOCKSCREEN_WIDGET_PREFS,
        widget::PrefsDialog::register_dynamic_type(&type_module),
        PLUGIN_PREFS_NAME,
        10,
    );
}

/// # Safety
/// Same contract as g_io_module_load above.
#[no_mangle]
pub unsafe extern "C" fn g_io_module_unload(_module: *mut gio::ffi::GIOModule) {}

/// # Safety
/// Called by gio-querymodules via dlsym to build its extension-point
/// cache without needing to actually run g_io_module_load.
///
/// Name corrected to match this crate's own real, compiled .so
/// filename (libbarkeep_lockscreen_prefs.so, from this crate's own
/// [package] name "barkeep-lockscreen-prefs") -- see
/// barkeep-lockscreen's own g_io_barkeep_lockscreen_query for the
/// full reasoning behind this rule, confirmed directly against a real
/// gio-querymodules cache dump on the actual target device. This was
/// the direct, confirmed cause of phosh-mobile-settings never finding
/// this plugin's own prefs extension at all: the old, wrong name meant
/// gio-querymodules silently omitted this .so from its own cache
/// entirely, and phosh-mobile-settings's own prefs lookup relies
/// strictly on that cache.
///
/// Return value built via glib::collections::StrV -- see that same
/// function's own doc comment in barkeep-lockscreen/src/lib.rs for
/// the full reasoning (a dedicated type for producing a correctly-
/// shaped GStrv, rather than a generic ToGlibPtr call whose exact
/// memory shape for this case wasn't independently verified).
#[no_mangle]
pub unsafe extern "C" fn g_io_barkeep_lockscreen_prefs_query() -> *mut *mut std::os::raw::c_char {
    let extension_points: glib::collections::StrV =
        [EXTENSION_POINT_LOCKSCREEN_WIDGET_PREFS].into_iter().map(glib::GString::from).collect();
    extension_points.into_glib_ptr()
}

//! Entry point for Phosh's own GIOModule loading mechanism.
//!
//! This whole file is a direct Rust translation of the real, working C
//! entry point at plugins/ticket-box/phosh-plugin-ticket-box.c (and
//! confirmed identical in shape against plugins/launcher-box/
//! phosh-plugin-launcher-box.c) -- not a guess at what GIOModule
//! expects, since that C file is what's actually shipping and working
//! as a Phosh lock-screen plugin today.
//!
//! The three functions below are called by libgio itself via dlsym, by
//! these *exact* symbol names -- not through any Rust-level trait or
//! call site of our own, so `#[no_mangle] pub extern "C" fn` on all
//! three is required, not a style choice. See GIOModule's own C
//! interface (`struct _GIOModule { ... void (*load)(GIOModule*); void
//! (*unload)(GIOModule*); ... }` in glib's own giomodule.c) for why
//! these three names specifically:
//!   - g_io_module_load / g_io_module_unload: the load/unload contract
//!     every GIOModule must implement.
//!   - g_io_<name>_query: a *name-dependent* symbol GIO's caching layer
//!     (gio-querymodules) looks for so it doesn't have to actually load
//!     every module on disk just to discover what extension points each
//!     one implements. <name> must match this crate's own real,
//!     compiled .so filename (minus the lib prefix/.so suffix, hyphens
//!     to underscores) -- confirmed directly against a real
//!     gio-querymodules cache dump, not this plugin's own logical name
//!     (PLUGIN_NAME below is a separate thing entirely -- the third
//!     argument to IOExtensionPoint::implement, not this). Getting this
//!     wrong doesn't break g_io_module_load itself, but means
//!     gio-querymodules silently omits this plugin from its own cache
//!     entirely -- confirmed as the real, on-device cause of the prefs
//!     dialog never being found by phosh-mobile-settings.

mod row;
mod widget;

use gio::glib;
use glib::translate::IntoGlibPtr;

/// This plugin's own module name, used both as the extension's priority
/// name (the third argument to IOExtensionPoint::implement, mirroring
/// PLUGIN_NAME in the C examples -- there just a compile-time
/// -DPLUGIN_NAME="..." C define, here a plain Rust const since Rust
/// doesn't need the preprocessor detour) and as the suffix of the
/// g_io_<name>_query symbol name below. Must stay in sync with
/// whatever this crate's own compiled .so is actually named in the
/// plugin's own .plugin/.desktop.in.in metadata file -- see that
/// file's own comment for the exact expected filename shape
/// (libphosh-plugin-<name>.so, confirmed directly from ticket-box's
/// own ticket-box.desktop.in.in).
const PLUGIN_NAME: &str = "barkeep";

/// Confirmed directly from plugins/meson.build's own phosh-plugin.h.in
/// substitution -- this is the *entire* content of phosh-plugin.h for
/// this extension point: a single string constant, nothing else. No
/// FFI, no struct, no function declarations to bind against for this
/// part specifically.
const EXTENSION_POINT_LOCKSCREEN_WIDGET: &str = "phosh-lockscreen-widget";

/// # Safety
/// Called by libgio via dlsym immediately after this .so is loaded,
/// exactly once, with a valid, non-null `module` pointer it owns -- we
/// never construct or free this pointer ourselves. Mirrors
/// g_io_module_load(GIOModule *module) from ticket-box's own C source.
#[no_mangle]
pub unsafe extern "C" fn g_io_module_load(module: *mut gio::ffi::GIOModule) {
    // TEMPORARY DIAGNOSTIC -- remove once resolved. Directly tests a
    // real, specific hypothesis for a recurring, real, on-device crash
    // ("specified class size... is smaller than the parent type's...",
    // immediately followed by a real Rust panic: "GTK has not been
    // initialized. Call `gtk::init` first.", both confirmed via real
    // journalctl output) -- rather than guessing at another fix.
    //
    // gtk4-rs's own real source (gtk4/rt.rs) confirms GTK tracks two
    // genuinely separate things: whether GTK was initialized *anywhere*
    // in this process (is_initialized()) and whether it was initialized
    // on *this specific thread* (is_initialized_main_thread()) --
    // calling gtk::init() again from a second thread panics with
    // "Attempted to initialize GTK from two different threads", a
    // closely related check to the one this plugin's own panic hit.
    // Phosh itself has certainly already called gtk::init() somewhere
    // (it's a GTK4 shell) -- so if this plugin is being loaded from a
    // different thread than Phosh's own main one, GTK would genuinely,
    // correctly consider itself uninitialized right here, producing
    // exactly the panic seen. This log line directly answers that,
    // rather than inferring it.
    eprintln!(
        "BARKEEP THREAD DIAGNOSTIC: g_io_module_load running on {:?}; gtk::is_initialized() = {}; gtk::is_initialized_main_thread() = {}",
        std::thread::current().id(),
        gtk::is_initialized(),
        gtk::is_initialized_main_thread(),
    );

    // from_glib_none: the standard glib-rs idiom for wrapping a
    // borrowed raw pointer as a safe GObject wrapper without taking
    // ownership of it -- module belongs to libgio, not to us, so this
    // must not be a `from_glib_full`-style ownership transfer.
    let type_module: glib::TypeModule = glib::translate::from_glib_none(module as *mut glib::gobject_ffi::GTypeModule);
    // Mirrors g_type_module_use(G_TYPE_MODULE(module)) -- tells glib
    // this module is actively in use so it won't be unloaded out from
    // under a live widget instance. Disambiguated to TypeModuleExt
    // specifically (GTypeModule also implements TypePlugin, which
    // defines its own, separately-named use_() -- the compiler itself
    // flagged this as ambiguous rather than picking one silently, and
    // TypeModuleExt is the one matching g_type_module_use specifically,
    // not TypePlugin's own, different use_).
    gio::prelude::TypeModuleExt::use_(&type_module);

    // Mirrors g_io_extension_point_implement(...) -- registers our own
    // widget type (see widget.rs) as an implementation of the
    // lock-screen extension point, at the same priority (10) the real
    // ticket-box/launcher-box examples both use.
    //
    // register_dynamic_type (not the plain static_type() an earlier
    // version of this file called directly) registers the type against
    // this same type_module first -- see widget.rs's own
    // #[object_subclass_dynamic] comment for the full reasoning this
    // is required for a GIOModule-loaded plugin type specifically, not
    // optional polish.
    gio::IOExtensionPoint::implement(
        EXTENSION_POINT_LOCKSCREEN_WIDGET,
        widget::BarkeepLockscreenWidget::register_dynamic_type(&type_module),
        PLUGIN_NAME,
        10,
    );
}

/// # Safety
/// Same contract as g_io_module_load above -- called by libgio, never
/// by us. Nothing to clean up here: we own no long-lived global state
/// outside individual widget instances, which GTK itself already
/// disposes of normally when the lock screen is torn down.
#[no_mangle]
pub unsafe extern "C" fn g_io_module_unload(_module: *mut gio::ffi::GIOModule) {}

/// # Safety
/// Called by gio-querymodules (via dlsym) to build its extension-point
/// cache without needing to actually run g_io_module_load -- must be
/// safe to call standalone, before any other entry point, which is
/// exactly what it does here (no shared state touched).
///
/// Name corrected after a real, on-device diagnosis: a working
/// giomodule.cache dump for several real plugins (ticket-box,
/// emergency-info, upcoming-events, etc.) showed each one's own query
/// symbol matches its own actual, compiled .so filename exactly (minus
/// the lib prefix and .so suffix, hyphens to underscores) -- e.g.
/// libphosh-plugin-prefs-ticket-box.so's own query symbol is
/// g_io_phosh_plugin_prefs_ticket_box_query. That's a coincidence of
/// the C examples' own meson build deliberately naming their compiled
/// .so to match a hand-chosen query symbol -- but our own .so filename
/// isn't hand-chosen at all: Cargo derives it automatically from this
/// crate's own [package] name (barkeep-lockscreen ->
/// libbarkeep_lockscreen.so), which never matched
/// g_io_phosh_plugin_barkeep_query (an earlier, incorrect version of
/// this function, following the C examples' own logical-name
/// convention rather than the real, filename-matching rule). That
/// mismatch meant gio-querymodules never found this symbol at all,
/// silently omitting this plugin from its own cache -- confirmed
/// directly: a real cache dump for this plugin's own prefs
/// counterpart, built the same wrong way, was completely absent from
/// its own directory's otherwise-successful cache. This crate's own
/// name is the one already fixed in stone (referenced throughout
/// Cargo.toml, the .deb assets list, etc.), so the query symbol is
/// what needed to change to match it, not the other way around.
#[no_mangle]
pub unsafe extern "C" fn g_io_barkeep_lockscreen_query() -> *mut *mut std::os::raw::c_char {
    let extension_points: glib::collections::StrV =
        [EXTENSION_POINT_LOCKSCREEN_WIDGET].into_iter().map(glib::GString::from).collect();
    extension_points.into_glib_ptr()
}

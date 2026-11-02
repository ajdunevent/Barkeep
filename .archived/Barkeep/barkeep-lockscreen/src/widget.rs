//! The lock-screen widget itself: a plain GtkBox subclass, matching
//! ticket-box.h's own G_DECLARE_FINAL_TYPE(..., GtkBox) exactly --
//! "any valid GTK widget" per the Phosh blog post, not something that
//! needs to inherit from a Phosh-specific base class.
//!
//! Reads cards.json fresh on every construction (mirroring
//! load_tickets's own comment in the real ticket-box.c -- "since the
//! lockscreen is rebuilt on lock we don't worry about changes in
//! directory content"), via barkeep_core::card::CardStore's own
//! load_read_only -- deliberately not open()/open_default(), which
//! attempt backup-and-repair on a corrupt file; per our own earlier
//! discussion, only the main app should ever attempt that, since two
//! independent processes both trying to "fix" the same shared file is
//! a good way to create a new bug, not fewer.
//!
//! A single, non-scrolling action bar sits above the card list, holding
//! both the search entry and the brightness toggle in one bar rather
//! than two separate ones -- deliberate, given this widget is height-
//! constrained (roughly 55% of screen height on Phosh's own lock
//! screen), where two persistent bars would eat meaningfully into an
//! already-scarce budget for no real gain over one.

use barkeep_core::card::{Card, CardStore};
use barkeep_core::brightness;
use gio::glib;
use adw::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::row::build_row;

/// This plugin's own GSettings schema id -- matches
/// sm.puri.phosh.plugins.barkeep.gschema.xml exactly, and
/// PLUGIN_NAME/EXTENSION_POINT_LOCKSCREEN_WIDGET in lib.rs (same
/// "barkeep" identifier throughout).
const SCHEMA_ID: &str = "sm.puri.phosh.plugins.barkeep";

/// How long a single brightness-force lasts before automatically
/// restoring, per our own earlier design discussion -- shorter than the
/// 30s first floated, since on this widget brightness only ever turns
/// off via this timeout (no manual off at all), so erring short and
/// letting a still-scanning user just tap again to reset it (see this
/// file's own brightness-button handler below) was judged better than
/// erring long.
const BRIGHTNESS_TIMEOUT: Duration = Duration::from_secs(10);

/// What load_and_populate actually filters by, read from GSettings when
/// available. A plain struct rather than passing four loose arguments
/// around -- see load_filter_prefs below for where these values
/// actually come from, and Default's own values for what's used when
/// the schema isn't installed at all (matching the schema's own
/// defaults exactly, so behavior doesn't silently differ between "not
/// installed yet" and "installed but never configured").
#[derive(Default)]
struct FilterPrefs {
    group_ids: Vec<Uuid>,
    favorites_only: bool,
    show_archived: bool,
    valid_only: bool,
}

impl FilterPrefs {
    fn matches(&self, card: &Card, today: &str) -> bool {
        if !self.show_archived && card.archived {
            return false;
        }
        if self.favorites_only && !card.starred {
            return false;
        }
        if self.valid_only && !today.is_empty() && !card.is_valid(today) {
            return false;
        }
        if !self.group_ids.is_empty() && !card.group_ids.iter().any(|id| self.group_ids.contains(id)) {
            return false;
        }
        true
    }
}

glib::wrapper! {
    pub struct BarkeepLockscreenWidget(ObjectSubclass<imp::BarkeepLockscreenWidget>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl BarkeepLockscreenWidget {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Registers this widget as a dynamic type within `type_module`,
    /// and returns the resulting glib::Type -- called from lib.rs's own
    /// g_io_module_load, in place of the plain
    /// BarkeepLockscreenWidget::static_type() an earlier version of
    /// this file used (see this file's own #[object_subclass_dynamic]
    /// comment above for why that was wrong for a plugin type).
    ///
    /// on_implementation_load and type_() are both provided by
    /// #[object_subclass_dynamic] itself (confirmed directly from
    /// glib-rs's own real docs.rs example: `let simple_module_object_type
    /// = imp::SimpleModuleObject::type_();` used after
    /// `TypeModuleExt::use_(&simple_type_module)` -- the same
    /// "register against the module, then read back the real type"
    /// two-step this function performs) -- not something we define
    /// ourselves, and not present at all without that attribute.
    ///
    /// Guarded with is_valid() -- confirmed directly from that same
    /// docs.rs example, which checks is_valid() before registering
    /// (expected false) and after (expected true) as its own way of
    /// confirming registration actually happened, not skipped here.
    /// Added after a real, on-device "Two different plugins tried to
    /// register 'BarkeepLockscreenWidget'" failure: the canonical
    /// glib-rs pattern calls on_implementation_load from within a
    /// proper TypeModuleImpl::load(), which -- unlike the real C
    /// examples' own G_DEFINE_DYNAMIC_TYPE, which has a built-in
    /// already-registered check -- our own g_io_module_load (a raw C
    /// entry point, not a TypeModuleImpl) has no guard against being
    /// invoked more than once for the same type without an intervening
    /// unload. This makes re-registration a deliberate no-op instead of
    /// relying on that never happening.
    pub fn register_dynamic_type(type_module: &glib::TypeModule) -> glib::types::Type {
        if !imp::BarkeepLockscreenWidget::type_().is_valid() {
            imp::BarkeepLockscreenWidget::on_implementation_load(type_module);
        }
        imp::BarkeepLockscreenWidget::type_()
    }
}

impl Default for BarkeepLockscreenWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// Opens this plugin's own GSettings safely -- None if the schema isn't
/// installed, rather than the hard process abort gio::Settings::new
/// itself would cause in that case (confirmed directly from gio-rs's
/// own docs: "it is an error for the schema to not exist... If schemas
/// need to be dynamically loaded... SettingsSchemaSource::lookup() can
/// be used to test for their existence before loading them" -- this is
/// exactly that officially-documented pattern, not an invented safety
/// check). A missing schema most likely means packaging/installation
/// went wrong somewhere, not that the plugin is being asked to do
/// something invalid -- crashing Phosh's own process over it would be a
/// far worse outcome than just falling back to this file's own
/// FilterPrefs::default() (every non-archived card, no other filter),
/// which is what every caller of this function already does on None.
fn open_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(SCHEMA_ID, true)?;
    Some(gio::Settings::new_full(&schema, gio::SettingsBackend::NONE, None))
}

/// Reads this plugin's own four filter keys from GSettings (see
/// sm.puri.phosh.plugins.barkeep.gschema.xml for their own types and
/// defaults) -- FilterPrefs::default() if the schema isn't installed
/// (see open_settings's own doc comment for why that's the right
/// fallback, not a crash). group-ids round-trips through Uuid::parse_str
/// per string (Card::group_ids is Vec<Uuid>, GSettings has no native
/// UUID type) -- any stored string that fails to parse is silently
/// dropped rather than treated as an error, the same defensive-fallback
/// spirit as Card::group_ids's own doc comment for a group id that no
/// longer matches a real Group (this plugin has no way to "fix" a
/// malformed stored value any more than it has a way to fix a corrupt
/// cards.json, and a single bad string here shouldn't make the whole
/// group filter fail).
fn load_filter_prefs() -> FilterPrefs {
    let Some(settings) = open_settings() else {
        return FilterPrefs::default();
    };
    FilterPrefs {
        group_ids: settings
            .strv("group-ids")
            .iter()
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect(),
        favorites_only: settings.boolean("favorites-only"),
        show_archived: settings.boolean("show-archived"),
        valid_only: settings.boolean("valid-only"),
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BarkeepLockscreenWidget {
        pub(super) list_box: gtk::ListBox,
        pub(super) stack: gtk::Stack,
        pub(super) search_entry: gtk::SearchEntry,
        pub(super) brightness_button: gtk::Button,
        // None when brightness isn't currently forced by this widget.
        // Some(deadline) while it is -- the single shared piece of
        // state both a fresh tap (sets it) and a reset tap (bumps it
        // forward) touch, checked by the repeating tick timer this
        // starts. Kept as widget-level state deliberately, not
        // per-row/per-button state, since brightness itself is one
        // global screen property, not something that could meaningfully
        // have more than one independent instance at once.
        pub(super) brightness_deadline: Rc<Cell<Option<Instant>>>,
        // Whatever the screen's own brightness was right before this
        // widget forced it to 100, so restoring it on timeout puts it
        // back rather than just leaving the screen maxed. None if it
        // was never successfully read (get_brightness failed) --
        // mirrors window.rs's own brightness_button handler exactly:
        // if we don't know what to restore to, we simply don't call
        // set_brightness on restore at all, rather than inventing a
        // fallback value.
        pub(super) saved_brightness: Rc<Cell<Option<i32>>>,
    }

    // #[object_subclass_dynamic]: registers this type as a *dynamic*
    // type (module-aware, can be loaded/unloaded/reloaded), not the
    // plain, static-only registration a normal, permanently-linked
    // application's own custom widgets use. Confirmed directly from
    // glib-rs's own real docs.rs page for object_subclass: "whereas an
    // object subclass can be registered only once as a static type, it
    // can be registered several times as a dynamic type" -- exactly
    // the distinction a GIOModule-loaded plugin type genuinely needs,
    // matching how every real C example (ticket-box, emergency-info,
    // upcoming-events) registers its own type via G_DEFINE_DYNAMIC_TYPE,
    // never the plain G_DEFINE_TYPE a permanent, static type would use.
    // An earlier version of this file omitted this attribute entirely,
    // registering this type as static -- confirmed as the likely real
    // cause of a genuine "specified class size... is smaller than the
    // parent type's" failure seen on real hardware, not a guess: the
    // pre-installed, C-based lock-screen plugins on that same device
    // (also GtkBox subclasses, also GTK4) don't exhibit that failure,
    // which specifically rules out a generic GTK4/Phosh-loading
    // explanation and points at this plugin's own type-registration
    // mechanism specifically.
    #[glib::object_subclass]
    #[object_subclass_dynamic]
    impl ObjectSubclass for BarkeepLockscreenWidget {
        const NAME: &'static str = "BarkeepLockscreenWidget";
        type Type = super::BarkeepLockscreenWidget;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for BarkeepLockscreenWidget {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);

            let action_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            action_bar.set_margin_start(6);
            action_bar.set_margin_end(6);
            action_bar.set_margin_top(6);
            action_bar.set_margin_bottom(6);

            self.search_entry.set_hexpand(true);
            self.search_entry.set_placeholder_text(Some("Search cards"));
            action_bar.append(&self.search_entry);

            self.brightness_button.set_icon_name("weather-clear-symbolic");
            self.brightness_button.set_tooltip_text(Some("Full Brightness"));
            action_bar.append(&self.brightness_button);

            obj.append(&action_bar);

            self.list_box.set_selection_mode(gtk::SelectionMode::None);
            let scrolled = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&self.list_box)
                .build();

            let empty_status = adw::StatusPage::builder()
                .icon_name("view-list-symbolic")
                .title("No Cards")
                .description("No cards match the current filter.")
                .build();

            let error_status = adw::StatusPage::builder()
                .icon_name("dialog-warning-symbolic")
                .title("Couldn't Load Cards")
                .build();

            self.stack.add_named(&scrolled, Some("list"));
            self.stack.add_named(&empty_status, Some("empty"));
            self.stack.add_named(&error_status, Some("error"));
            obj.append(&self.stack);

            {
                let obj_weak = obj.downgrade();
                self.search_entry.connect_search_changed(move |_| {
                    if let Some(obj) = obj_weak.upgrade() {
                        obj.imp().load_and_populate();
                    }
                });
            }

            self.setup_brightness_button();

            self.load_and_populate();
        }
    }

    impl WidgetImpl for BarkeepLockscreenWidget {}
    impl BoxImpl for BarkeepLockscreenWidget {}

    impl BarkeepLockscreenWidget {
        /// Only name and group names -- deliberately not notes, unlike
        /// the main app's own CardStore::search (confirmed directly
        /// against that function's own query-matching logic, not
        /// assumed). This plugin's own row content never shows notes
        /// at all (an earlier, explicit design decision), so matching
        /// against them here would surface a card for a reason the
        /// user has no way to see in this widget -- confusing, not
        /// helpful, unlike in the main app where notes are visible and
        /// a search match against them is self-explanatory.
        fn matches_search(&self, card: &Card, groups: &[barkeep_core::card::Group], query: &str) -> bool {
            if query.is_empty() {
                return true;
            }
            let q = query.to_lowercase();
            card.name.to_lowercase().contains(&q)
                || card.group_ids.iter().any(|gid| {
                    groups
                        .iter()
                        .find(|g| g.id == *gid)
                        .map(|g| g.name.to_lowercase().contains(&q))
                        .unwrap_or(false)
                })
        }

        fn load_and_populate(&self) {
            let Ok(path) = CardStore::default_path() else {
                self.stack.set_visible_child_name("error");
                return;
            };
            let Some(store) = CardStore::load_read_only(&path) else {
                self.stack.set_visible_child_name("error");
                return;
            };

            let prefs = load_filter_prefs();
            // "" rather than an actual today string when unavailable --
            // FilterPrefs::matches's own valid_only check already treats
            // an empty today the same way CardStore::search's own
            // valid_only filter does (skip the check entirely), for the
            // exact same reason: an empty today would make every
            // valid_from-having card wrongly compare as "not yet valid",
            // since any non-empty date string sorts lexicographically
            // greater than an empty one. Shape matches window.rs's own
            // already-working export_action handler exactly (DateTime::
            // format returns a Result, not an Option).
            let today = glib::DateTime::now_local()
                .map(|d| d.format("%Y-%m-%d").unwrap_or_else(|_| "".into()).to_string())
                .unwrap_or_default();

            let query = self.search_entry.text().to_string();

            let mut cards: Vec<_> = store
                .cards
                .iter()
                .filter(|c| prefs.matches(c, &today))
                .filter(|c| self.matches_search(c, &store.groups, &query))
                .collect();

            // Starred first, alphabetical within each group -- matches
            // CardStore::search's own sort exactly (confirmed directly
            // against card.rs's own sort_by_key call, not assumed),
            // since our own earlier design decision was to carry the
            // main app's sort order over unchanged, not invent a
            // separate one for the plugin.
            cards.sort_by_key(|c| (!c.starred, c.name.to_lowercase()));

            if cards.is_empty() {
                self.stack.set_visible_child_name("empty");
                return;
            }

            while let Some(child) = self.list_box.first_child() {
                self.list_box.remove(&child);
            }
            for card in &cards {
                self.list_box.append(&build_row(card, &self.list_box));
            }
            self.stack.set_visible_child_name("list");
        }

        /// Wires up the brightness button's full click/timeout/cooldown
        /// lifecycle. A plain gtk::Button, not gtk::ToggleButton, unlike
        /// window.rs's own brightness_button -- deliberately: this
        /// widget's own design has every tap either start or reset the
        /// 10s timer, never a direct, immediate "turn off" the way the
        /// main app's own simple on/off toggle has, and a
        /// ToggleButton's own built-in active/inactive flip on each
        /// click would fight that rather than match it.
        fn setup_brightness_button(&self) {
            let deadline = self.brightness_deadline.clone();
            let saved = self.saved_brightness.clone();
            let btn = self.brightness_button.clone();

            btn.connect_clicked(move |btn| {
                if deadline.get().is_some() {
                    // Already forced on -- a reset tap, not a fresh
                    // one. No set_brightness call here at all: the
                    // screen's already at 100, so there's nothing to
                    // actually change, which is also exactly why this
                    // path needs no cooldown of its own (see this
                    // file's own BRIGHTNESS_TIMEOUT doc comment and the
                    // cooldown reasoning in the timeout branch below --
                    // the cooldown only guards actual brightness-
                    // changing D-Bus calls, and this tap makes none).
                    deadline.set(Some(Instant::now() + BRIGHTNESS_TIMEOUT));
                    return;
                }

                saved.set(brightness::get_brightness());
                if brightness::set_brightness(100).is_none() {
                    // Couldn't reach the brightness D-Bus service --
                    // don't start a timer for an effect that didn't
                    // actually happen (mirrors window.rs's own
                    // brightness_button handler: don't leave state
                    // claiming success when the underlying call
                    // failed).
                    return;
                }
                deadline.set(Some(Instant::now() + BRIGHTNESS_TIMEOUT));
                btn.set_icon_name("display-brightness-symbolic");

                let deadline_for_tick = deadline.clone();
                let saved_for_tick = saved.clone();
                let btn_for_tick = btn.clone();
                glib::source::timeout_add_local(Duration::from_millis(250), move || {
                    let Some(dl) = deadline_for_tick.get() else {
                        // Cleared from somewhere else already (shouldn't
                        // happen while this tick timer's own the only
                        // thing that ever clears it, but defensive
                        // rather than assumed).
                        return glib::ControlFlow::Break;
                    };

                    if Instant::now() < dl {
                        return glib::ControlFlow::Continue;
                    }

                    // Timeout reached -- the only way this widget's own
                    // brightness force ever turns off, per our own
                    // earlier design decision (no manual off at all).
                    if let Some(prev) = saved_for_tick.take() {
                        brightness::set_brightness(prev);
                    }
                    deadline_for_tick.set(None);
                    btn_for_tick.set_icon_name("weather-clear-symbolic");

                    // Rate limit: same reasoning and same 1500ms value
                    // as window.rs's own brightness_button handler
                    // (TOGGLE_COOLDOWN_MS in barkeep-core's own
                    // brightness.rs) -- toggling brightness rapidly was
                    // confirmed to crash Phosh outright. Only needed
                    // here, at the actual off transition, not on every
                    // tap -- see this closure's own caller for why a
                    // reset tap makes no set_brightness call at all and
                    // so needs no cooldown of its own.
                    btn_for_tick.set_sensitive(false);
                    let btn_for_reenable = btn_for_tick.clone();
                    glib::source::timeout_add_local_once(
                        Duration::from_millis(brightness::TOGGLE_COOLDOWN_MS),
                        move || {
                            btn_for_reenable.set_sensitive(true);
                        },
                    );

                    glib::ControlFlow::Break
                });
            });
        }
    }
}


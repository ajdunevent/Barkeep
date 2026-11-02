//! The lock-screen plugin's preferences dialog: an AdwPreferencesDialog
//! subclass, matching ticket-box-prefs.c's own base class
//! (ADW_TYPE_PREFERENCES_DIALOG) exactly, but built programmatically
//! rather than from a GResource .ui template -- Cardigan's own main app
//! (window.rs) builds every widget programmatically throughout, with no
//! .ui-template convention anywhere in the existing codebase, so this
//! follows that established pattern rather than introducing a new one
//! (a second build.rs, a .gresources.xml, .ui compilation) just for
//! this one small dialog.

use cardigan_core::card::CardStore;
use gio::glib;
use adw::prelude::*;
use adw::subclass::prelude::*;
use uuid::Uuid;

const SCHEMA_ID: &str = "sm.puri.phosh.plugins.cardigan";

/// Same reasoning and shape as cardigan-lockscreen's own open_settings
/// -- duplicated rather than shared via cardigan-core, since that crate
/// is deliberately gio/glib-free too (not just gtk4/libadwaita-free):
/// depending on gio directly from cardigan-core would still couple the
/// main app to whatever gio/glib version this plugin pins, reintroducing
/// the same coupling risk cardigan-core's own GTK-free boundary exists
/// to avoid. See gio-rs's own docs for why this lookup-first pattern is
/// used instead of calling gio::Settings::new directly (which aborts
/// the process outright if the schema isn't installed).
fn open_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(SCHEMA_ID, true)?;
    Some(gio::Settings::new_full(&schema, gio::SettingsBackend::NONE, None))
}

glib::wrapper! {
    pub struct PrefsDialog(ObjectSubclass<imp::PrefsDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PrefsDialog {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Same reasoning and shape as CardiganLockscreenWidget's own
    /// register_dynamic_type in cardigan-lockscreen -- see that
    /// function's own doc comment for the full explanation, including
    /// the is_valid() guard against re-registration. This is a
    /// second, independent GIOModule with its own, separate type to
    /// register (see this crate's own lib.rs doc comment on why this
    /// is genuinely a second build artifact, not a mistake to have
    /// two), so it needs this same fix applied on its own, not
    /// inherited from the other crate automatically.
    pub fn register_dynamic_type(type_module: &glib::TypeModule) -> glib::types::Type {
        if !imp::PrefsDialog::type_().is_valid() {
            imp::PrefsDialog::on_implementation_load(type_module);
        }
        imp::PrefsDialog::type_()
    }
}

impl Default for PrefsDialog {
    fn default() -> Self {
        Self::new()
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PrefsDialog;

    // See cardigan-lockscreen's own widget.rs for the full reasoning --
    // this crate's own PrefsDialog needs this exact same fix
    // independently, since it's a genuinely separate GIOModule/type
    // registration, not something the other crate's own fix covers.
    #[glib::object_subclass]
    #[object_subclass_dynamic]
    impl ObjectSubclass for PrefsDialog {
        const NAME: &'static str = "CardiganLockscreenPrefsDialog";
        type Type = super::PrefsDialog;
        type ParentType = adw::PreferencesDialog;
    }

    impl ObjectImpl for PrefsDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let page = adw::PreferencesPage::new();

            let Some(settings) = open_settings() else {
                // No schema installed -- almost certainly a packaging
                // problem, not something this dialog itself can fix.
                // Same reasoning as cardigan-lockscreen's own
                // open_settings: better to show something explanatory
                // than to have gio::Settings::new abort the whole
                // process over it.
                let group = adw::PreferencesGroup::new();
                group.add(&adw::ActionRow::builder().title("Couldn't load settings").build());
                page.add(&group);
                obj.add(&page);
                return;
            };

            let filters_group = adw::PreferencesGroup::builder().title("Filters").build();

            let favorites_row = adw::SwitchRow::builder().title("Favorites Only").build();
            settings.bind("favorites-only", &favorites_row, "active").build();
            filters_group.add(&favorites_row);

            let archived_row = adw::SwitchRow::builder().title("Show Archived").build();
            settings.bind("show-archived", &archived_row, "active").build();
            filters_group.add(&archived_row);

            let valid_row = adw::SwitchRow::builder().title("Valid Only").build();
            settings.bind("valid-only", &valid_row, "active").build();
            filters_group.add(&valid_row);

            page.add(&filters_group);

            let groups_group = adw::PreferencesGroup::builder()
                .title("Groups")
                .description("Only show cards from the selected groups. Select none to show cards from every group.")
                .build();

            // Reads cards.json the same way the lock-screen widget
            // itself does (load_read_only, not open()/open_default()
            // -- see that function's own doc comment in card.rs for
            // why this dialog shouldn't attempt repair either), purely
            // to list the groups that currently exist. A missing/
            // unreadable file just means no groups to offer here, not
            // an error worth its own UI state -- there's nothing this
            // dialog needs to do differently, since an empty groups
            // list already means "no group filter" (see
            // sm.puri.phosh.plugins.cardigan.gschema.xml's own
            // group-ids doc comment).
            if let Ok(path) = CardStore::default_path() {
                if let Some(store) = CardStore::load_read_only(&path) {
                    let current: Vec<Uuid> = settings
                        .strv("group-ids")
                        .iter()
                        .filter_map(|s| Uuid::parse_str(s).ok())
                        .collect();

                    for group in &store.groups {
                        let row = adw::SwitchRow::builder()
                            .title(glib::markup_escape_text(&group.name).to_string())
                            .active(current.contains(&group.id))
                            .build();

                        let settings = settings.clone();
                        let group_id = group.id;
                        row.connect_active_notify(move |row| {
                            // Re-reads group-ids fresh on every toggle
                            // rather than trusting a captured snapshot
                            // from construction time -- this dialog can
                            // have many of these rows open at once, and
                            // each one's own toggle must not clobber
                            // whatever any of the others already wrote.
                            let mut ids: Vec<Uuid> = settings
                                .strv("group-ids")
                                .iter()
                                .filter_map(|s| Uuid::parse_str(s).ok())
                                .collect();
                            if row.is_active() {
                                if !ids.contains(&group_id) {
                                    ids.push(group_id);
                                }
                            } else {
                                ids.retain(|id| *id != group_id);
                            }
                            let strs: Vec<String> = ids.iter().map(Uuid::to_string).collect();
                            let _ = settings.set_strv("group-ids", strs);
                        });

                        groups_group.add(&row);
                    }
                }
            }

            page.add(&groups_group);
            obj.add(&page);
        }
    }

    impl WidgetImpl for PrefsDialog {}
    impl AdwDialogImpl for PrefsDialog {}
    impl PreferencesDialogImpl for PrefsDialog {}
}

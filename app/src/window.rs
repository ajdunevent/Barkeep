use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use barkeep_core::barcode;
use crate::camera;
use barkeep_core::card::{BarcodeType, Card, CardStore, Group, ImportOutcome, OnExpiration, UiState};
use uuid::Uuid;

/// A breakpoint and polling approach caused a grow/snap-back glitch.
const CARD_TILE_WIDTH: i32 = 160;
const CARD_TILE_HEIGHT: i32 = 100;
/// Space under the last row of tiles, clear of the floating add button
/// and the multiselect toolbar.
const GRID_BOTTOM_CLEARANCE: i32 = 88;

/// Shown when CardStore::open_default fails outright (permissions on
/// the XDG data directory, no data directory available, invalid UTF-8
/// in an existing cards.json): a genuine I/O failure, not a parse
/// failure (which CardStore::open already recovers from, backing up
/// the damaged file first). Minimal and self-contained rather than
/// reusing the main widget-construction machinery below, since that
/// assumes a working store. Quits the application on the only button
/// here rather than leaving an inert error window open.
fn build_startup_error_window(app: &adw::Application, error: &str) -> adw::ApplicationWindow {
    let status = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("Couldn't Open Your Cards")
        .description(format!(
            "Barkeep couldn't read or create its own data file:\n\n{error}\n\nThis usually means a permissions problem with your data directory. Your existing cards, if any, have not been touched."
        ))
        .build();

    let quit_button = gtk::Button::builder()
        .label("Quit")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    status.set_child(Some(&quit_button));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(360)
        .default_height(480)
        .content(&toolbar)
        .build();

    {
        let app = app.clone();
        quit_button.connect_clicked(move |_| {
            app.quit();
        });
    }

    window
}

/// Every widget build_multiselect_toolbar constructs that's still
/// needed later in build_window for click-handler wiring (which stays
/// in build_window since it depends on more state than this
/// construction step). Named fields rather than a positional tuple so
/// the call site can't mix up which widget is which.
struct MultiselectToolbarWidgets {
    star_button: gtk::ToggleButton,
    archive_button: gtk::ToggleButton,
    change_groups_button: gtk::Button,
    delete_button: gtk::Button,
    clear_all_button: gtk::Button,
    done_button: gtk::Button,
    action_bar_revealer: gtk::Revealer,
}

/// Builds multiselect's bottom toolbar widgets and packs them into
/// layout. Event-handler wiring for these buttons stays in
/// build_window since it depends on that function's other state.
fn build_multiselect_toolbar() -> MultiselectToolbarWidgets {
    // Works like Archive: reflects whether all visible-selected cards
    // are already starred (set from rebuild_grid below); unstars only
    // when everything's already starred, stars otherwise.
    let star_button = gtk::ToggleButton::builder()
        .icon_name("starred-symbolic")
        .tooltip_text("Star")
        .css_classes(["raised"])
        .build();
    let archive_button = gtk::ToggleButton::builder()
        .icon_name("shoe-box-symbolic")
        .tooltip_text("Archive")
        .css_classes(["raised"])
        .build();
    let change_groups_button = gtk::Button::builder().label("Groups").build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Delete")
        .css_classes(["destructive-action"])
        .build();
    let clear_all_button = gtk::Button::builder()
        .icon_name("brush-symbolic")
        .tooltip_text("Clear All")
        .css_classes(["raised"])
        .build();
    // Multiselect no longer auto-exits after a bulk action, so this
    // button's meaning shifted from "abort" to "finished, exit and
    // deselect everything"; renamed from Cancel to Done to match.
    let done_button = gtk::Button::builder().label("Done").build();
    let action_bar = gtk::ActionBar::new();
    action_bar.pack_start(&star_button);
    action_bar.pack_start(&archive_button);
    action_bar.pack_start(&change_groups_button);
    action_bar.pack_start(&delete_button);
    // pack_end's first call lands at the true end edge, each
    // subsequent call pushes inward toward center (the opposite of
    // what an earlier version of this comment claimed). Done packed
    // first to sit at the edge; Clear All packed second to sit just
    // inside it.
    action_bar.pack_end(&done_button);
    action_bar.pack_end(&clear_all_button);
    // Same Revealer pattern as search_revealer (200ms), but SlideUp
    // since this bar is anchored to the bottom of the window.
    let action_bar_revealer = gtk::Revealer::builder()
        .child(&action_bar)
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(200)
        .reveal_child(false)
        .build();

    MultiselectToolbarWidgets {
        star_button,
        archive_button,
        change_groups_button,
        delete_button,
        clear_all_button,
        done_button,
        action_bar_revealer,
    }
}

/// Both fields are gtk::MenuButton; named fields rather than a
/// positional tuple since the compiler can't distinguish two
/// same-typed tuple positions if the call site mixes them up.
struct HeaderMenuButtons {
    add_button: gtk::MenuButton,
    menu_button: gtk::MenuButton,
}

/// The two menu buttons, the floating "+" (scan/enter/import) and the
/// header's hamburger menu (view toggles, export, about), built together since
/// both are just a gio::Menu structure referencing action names as
/// strings (e.g. "win.scan-camera"). The real actions are created and
/// wired up separately, later in build_window's "Wiring" section.
fn build_header_menu_buttons() -> HeaderMenuButtons {
    // "+" opens a menu: Scan with Camera / Scan from Image / Enter Manually,
    // rather than jumping straight to manual entry.
    let add_menu_model = gio::Menu::new();
    add_menu_model.append(Some("Scan with Camera"), Some("win.scan-camera"));
    add_menu_model.append(Some("Scan from Image…"), Some("win.scan-image"));
    add_menu_model.append(Some("Enter Manually"), Some("win.enter-manually"));
    // Own section: importing a whole file is a different kind of
    // action from the three add-one-card options above it.
    let add_import_section = gio::Menu::new();
    add_import_section.append(Some("Import from File…"), Some("win.import"));
    add_menu_model.append_section(None, &add_import_section);
    let add_button = gtk::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .menu_model(&add_menu_model)
        .direction(gtk::ArrowType::Up)
        .tooltip_text("Add card")
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .margin_end(18)
        .margin_bottom(18)
        .css_classes(["circular", "suggested-action", "add-float"])
        .build();

    let menu_model = gio::Menu::new();
    let view_section = gio::Menu::new();
    view_section.append(Some("Hide Presently Invalid Cards"), Some("win.show-valid-only"));
    view_section.append(Some("Hide Archived Cards"), Some("win.show-archived"));
    view_section.append(Some("Manage Groups"), Some("win.manage-groups"));
    menu_model.append_section(None, &view_section);
    // Own section, between Manage Groups and About, now that Import
    // has moved to the Add menu.
    let export_section = gio::Menu::new();
    export_section.append(Some("Export Collection…"), Some("win.export"));
    menu_model.append_section(None, &export_section);
    let about_section = gio::Menu::new();
    about_section.append(Some("About Barkeep"), Some("win.about"));
    menu_model.append_section(None, &about_section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu_model)
        .build();

    HeaderMenuButtons { add_button, menu_button }
}

/// Assembles the sidebar's headerbar (search toggle, menu button,
/// title) and wraps it with sidebar_box into the
/// NavigationPage build_window's split view uses. All four parameters
/// are taken by reference: sidebar_box specifically is still mutated
/// later in build_window (group_tabs_row is inserted into it), so
/// taking it by value here would break that later call.
fn build_sidebar_headerbar(
    menu_button: &gtk::MenuButton,
    search_toggle_button: &gtk::ToggleButton,
    sidebar_box: &gtk::Box,
    banner: &adw::Banner,
) -> adw::NavigationPage {
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.pack_start(search_toggle_button);
    sidebar_header.pack_end(menu_button);
    let sidebar_title = adw::WindowTitle::new("Barkeep", "");
    sidebar_header.set_title_widget(Some(&sidebar_title));

    let sidebar_toolbar = adw::ToolbarView::new();
    // Flat leaves this wrapper transparent, causing white/black bands
    // above the headerbar. A CSS override on the internal wrapper
    // directly was tried first and didn't work (no direct handle to
    // that GtkBox exists).
    sidebar_toolbar.set_top_bar_style(adw::ToolbarStyle::Raised);
    sidebar_toolbar.add_top_bar(&sidebar_header);
    // Second top bar, below the headerbar (see SharedBanner). Added
    // right after the headerbar since AdwToolbarView stacks top bars
    // in the order they're added.
    sidebar_toolbar.add_top_bar(banner);
    // No width clamp anymore (there used to be one, to avoid a visible
    // snap when the second pane appeared). A grid wants full window
    // width when collapsed, to lay out multiple columns on a phone.
    // split_view's min/max-sidebar-width below still pins this pane to
    // a narrow range in the non-collapsed split view.
    sidebar_toolbar.set_content(Some(sidebar_box));

    adw::NavigationPage::builder()
        .title("Barkeep")
        .child(&sidebar_toolbar)
        .build()
}

/// Every widget/state from the detail pane's construction still needed
/// later in build_window. detail_status and content_page looked
/// "probably local" by reference count but are referenced again much
/// later (refresh's closure, split_view's logic). share_actions and
/// share_menu_model are the only two genuinely local to this construction.
struct DetailPaneWidgets {
    detail_status: adw::StatusPage,
    content_toolbar: adw::ToolbarView,
    content_header: adw::HeaderBar,
    bottom_bar: gtk::ActionBar,
    edit_button: gtk::Button,
    star_button: gtk::ToggleButton,
    archive_button: gtk::ToggleButton,
    copy_code_action: gio::SimpleAction,
    copy_image_action: gio::SimpleAction,
    share_menu_button: gtk::MenuButton,
    info_button: gtk::MenuButton,
    link_button: gtk::Button,
    edit_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    star_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    archive_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    copy_code_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    copy_image_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    link_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    content_page: adw::NavigationPage,
}

/// Builds the detail pane: the barcode/card-info side of the split
/// view, its bottom action bar, and every per-card button. Purely
/// self-contained, no input parameters needed. Per-card wiring
/// (swapping each button's click handler to the selected card) happens
/// later, in show_card_detail; this only constructs the widgets and
/// the *_handler slots that wiring fills in.
fn build_detail_pane(banner: &adw::Banner) -> DetailPaneWidgets {
    let detail_status = adw::StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title("No card selected")
        .description("Pick a card from the list, or add a new one.")
        .build();

    let content_toolbar = adw::ToolbarView::new();
    // RaisedBorder removes the shadow between the header and content
    // below, while staying opaque like Raised so it doesn't reopen the
    // white/black-bands issue.
    content_toolbar.set_top_bar_style(adw::ToolbarStyle::RaisedBorder);
    let content_header = adw::HeaderBar::new();
    content_toolbar.add_top_bar(&content_header);
    // Second top bar, below the headerbar (see SharedBanner). Added
    // right after the headerbar since AdwToolbarView stacks top bars
    // in the order they're added.
    content_toolbar.add_top_bar(banner);
    // Targeted by update_header_style (id-selector CSS, colors the
    // whole header to match the selected card). Set once here since
    // the widget is permanent; only the CSS rule's content changes
    // per-card.
    content_header.set_widget_name("barkeep-detail-header");
    // Separate from content_header's name above: libadwaita's
    // --headerbar-bg-color/--headerbar-backdrop-color variables are
    // read by the toolbar view's internal wrapper, not by
    // content_header itself. #barkeep-detail-header's background-color
    // rule only ever painted the HeaderBar's own surface, never the
    // separate Raised-provided background layer behind it, which kept
    // showing the theme's default header color wherever content_header
    // didn't cover it exactly. Named here so update_header_style can
    // override that variable, scoped to this subtree.
    content_toolbar.set_widget_name("barkeep-content-toolbar");

    // Per-card action buttons live in a bottom ActionBar instead of
    // the headerbar; with edit/delete/star/archive/copy
    // plus info/link, the header was too crowded on a phone-width
    // window.
    let bottom_bar = gtk::ActionBar::new();
    content_toolbar.add_bottom_bar(&bottom_bar);

    // Created once and reused; hidden until something's selected,
    // show_card_detail swaps their click handler rather than creating
    // new buttons each time.
    let edit_button = gtk::Button::from_icon_name("document-edit-symbolic");
    edit_button.set_tooltip_text(Some("Edit"));
    edit_button.set_visible(false);

    // Wired per-card in show_card_detail via star_handler: the handler
    // needs to know which card's star it's toggling, and set_active()
    // has to run without that handler firing (a ToggleButton's
    // "toggled" signal fires on programmatic set_active() too) when
    // just syncing the button to a newly selected card's state.
    let star_button = gtk::ToggleButton::builder()
        .icon_name("starred-symbolic")
        .tooltip_text("Star")
        .visible(false)
        .build();

    // Tooltip communicates archived/not-archived state rather than a
    // swapping icon pair. shoe-box-symbolic is bundled in resources/
    // rather than depending on a system icon theme (mail-archive-
    // symbolic didn't render on desktop; folder-symbolic and
    // folder-work-symbolic had their own rendering issues).
    let archive_button = gtk::ToggleButton::builder()
        .icon_name("shoe-box-symbolic")
        .tooltip_text("Archive")
        .visible(false)
        .build();

    // Copy code / copy barcode image combined into one share menu
    // instead of two ActionBar buttons. gio::SimpleAction since
    // gio::Menu items wire to actions, not click signals. Each action
    // is disconnected/reconnected per-card the same way a button's
    // click handler would be, via copy_code_handler/copy_image_handler
    // below.
    let share_actions = gio::SimpleActionGroup::new();
    let copy_code_action = gio::SimpleAction::new("copy-code", None);
    let copy_image_action = gio::SimpleAction::new("copy-image", None);
    share_actions.add_action(&copy_code_action);
    share_actions.add_action(&copy_image_action);

    let share_menu_model = gio::Menu::new();
    share_menu_model.append(Some("Copy barcode code"), Some("share.copy-code"));
    share_menu_model.append(Some("Copy barcode image"), Some("share.copy-image"));

    let share_menu_button = gtk::MenuButton::builder()
        .icon_name("folder-publicshare-symbolic")
        .tooltip_text("Share")
        .menu_model(&share_menu_model)
        .visible(false)
        .css_classes(["flat"])
        .build();
    // "share" matches what the menu model references ("share.copy-code"
    // etc), scoped to this widget rather than the window's action map
    // since these actions only make sense for the selected card.
    share_menu_button.insert_action_group("share", Some(&share_actions));

    // Notes live behind this info icon; a MenuButton's built-in
    // popover handles narrow-screen placement itself. Hidden by
    // default, shown by show_card_detail for cards that have notes.
    // help-about-symbolic ships with Adwaita itself, unlike
    // dialog-information-symbolic, which renders closer to a lightbulb/
    // exclamation mark depending on icon theme.
    let info_button = gtk::MenuButton::builder()
        .icon_name("help-about-symbolic")
        .tooltip_text("Notes")
        .build();
    info_button.set_visible(false);

    // The card's external link, if it has one. Reconnected per-card
    // via link_handler below.
    let link_button = gtk::Button::from_icon_name("external-link-symbolic");
    link_button.set_tooltip_text(Some("Open link"));
    link_button.set_visible(false);
    link_button.add_css_class("flat");

    bottom_bar.pack_start(&star_button);
    bottom_bar.pack_start(&archive_button);
    bottom_bar.pack_start(&info_button);
    bottom_bar.pack_start(&edit_button);
    // pack_end's first call ends up closest to the edge, so link is
    // packed first (rightmost) and share_menu_button second (just left
    // of it), for left-to-right share/link reading.
    bottom_bar.pack_end(&link_button);
    bottom_bar.pack_end(&share_menu_button);
    // delete_button moved into the edit dialog (see open_edit_dialog).

    let edit_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let star_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let archive_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let copy_code_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let copy_image_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    let link_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));

    content_toolbar.set_content(Some(&detail_status));

    let content_page = adw::NavigationPage::builder()
        .title("Details")
        .child(&content_toolbar)
        .build();

    DetailPaneWidgets {
        detail_status,
        content_toolbar,
        content_header,
        bottom_bar,
        edit_button,
        star_button,
        archive_button,
        copy_code_action,
        copy_image_action,
        share_menu_button,
        info_button,
        link_button,
        edit_handler,
        star_handler,
        archive_handler,
        copy_code_handler,
        copy_image_handler,
        link_handler,
        content_page,
    }
}

/// split_view, toast_overlay, the main window, and the card-name
/// label's reusable CSS provider/color cache, all still needed later
/// in build_window (window especially, referenced well over a hundred
/// times). sidebar_page and content_page are taken by reference since
/// content_page is still needed again later too.
struct SplitViewAndWindow {
    split_view: adw::NavigationSplitView,
    toast_overlay: adw::ToastOverlay,
    window: adw::ApplicationWindow,
    name_label_css: gtk::CssProvider,
    name_label_colors: Rc<RefCell<(String, String)>>,
}

fn build_split_view_and_window(
    app: &adw::Application,
    sidebar_page: &adw::NavigationPage,
    content_page: &adw::NavigationPage,
) -> SplitViewAndWindow {
    // Always single-pane/stack-navigation style (tap a card -> full-
    // width detail view with a back button) regardless of window
    // width, rather than switching to side-by-side above some
    // breakpoint. collapsed(true) is set directly rather than left to
    // an adw::Breakpoint, so sidebar_width_unit/min_sidebar_width/
    // max_sidebar_width are gone too.
    let split_view = adw::NavigationSplitView::builder()
        .sidebar(sidebar_page)
        .content(content_page)
        .collapsed(true)
        .build();

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&split_view));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(360)
        .default_height(720)
        .width_request(368)
        .height_request(480)
        .content(&toast_overlay)
        .build();

    // Explicit app.quit() rather than relying on GApplication's
    // implicit "quit once the last window closes": a gdb backtrace and
    // a temporary application.windows() diagnostic showed the main
    // thread healthy and the window count reaching zero within 500ms,
    // yet the process didn't exit even after waiting over a minute.
    // Root cause not identified; this is a direct fix for the observed
    // effect (the app should always quit once its only window closes)
    // rather than a diagnosis of why GApplication's default didn't
    // fire. Guarded on window count actually being zero.
    {
        window.connect_close_request(move |win| {
            if let Some(app) = win.application() {
                glib::source::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    if app.windows().is_empty() {
                        app.quit();
                    }
                });
            }
            glib::Propagation::Proceed
        });
    }

    // The card-name label above the barcode gets its style (font size,
    // background/text color) from this one reusable provider, targeted
    // at a fixed widget ID, rather than a fresh provider each time.
    // Font size is set once when a card is opened rather than kept
    // live during resize, since there's no resize-driven update loop
    // anymore (see CARD_TILE_WIDTH).
    let name_label_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &default_display(),
        &name_label_css,
        // PRIORITY_USER, not PRIORITY_APPLICATION (see register_css).
        // This provider drives update_header_style's gradient CSS
        // (#barkeep-card-body's background), part of the white/black
        // bands issue register_css's padding fix didn't cover, since
        // this provider is separate from register_css.
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );
    let name_label_colors: Rc<RefCell<(String, String)>> =
        Rc::new(RefCell::new(("#3584e4".to_string(), "#ffffff".to_string())));

    SplitViewAndWindow {
        split_view,
        toast_overlay,
        window,
        name_label_css,
        name_label_colors,
    }
}

pub fn build_window(app: &adw::Application) -> adw::ApplicationWindow {
    let (store, startup_warnings) = match CardStore::open_default() {
        Ok((store, warnings)) => (store, warnings),
        Err(e) => return build_startup_error_window(app, &e.to_string()),
    };
    let store = Rc::new(RefCell::new(store));
    // Loaded once here since it's needed by search_toggle_button below
    // and by the two hide-toggle actions later in this function. See
    // UiState's own doc comment in card.rs for why this is separate
    // from CardStore's persistence.
    let ui_state: Rc<RefCell<UiState>> = Rc::new(RefCell::new(UiState::open_default()));

    // --- Sidebar: search + card grid ---------------------------------
    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text("Search cards")
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    // search_entry itself stays visible=true; reveal_child on this
    // wrapper controls whether it's shown, with an animated transition.
    let search_revealer = gtk::Revealer::builder()
        .child(&search_entry)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .reveal_child(false)
        .build();

    let flow_box = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .homogeneous(true)
        .row_spacing(4)
        .column_spacing(4)
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(GRID_BOTTOM_CLEARANCE)
        .valign(gtk::Align::Start)
        .build();

    let grid_scroller = gtk::ScrolledWindow::builder()
        .child(&flow_box)
        .vexpand(true)
        .build();

    // Shown when the store has zero cards, not when a search just has
    // no matches; rebuild_grid checks the store's real card count for
    // this separately.
    let empty_status = adw::StatusPage::builder()
        .title("No Cards Yet")
        .description("Tap the + button to add your first card, or use the menu to import existing ones.")
        .vexpand(true)
        .visible(false)
        .build();

    // See build_header_menu_buttons for construction details.
    let HeaderMenuButtons { add_button, menu_button } = build_header_menu_buttons();
    register_css(
        "menubutton.add-float > button { min-width: 56px; min-height: 56px; \
         box-shadow: 0 1px 3px alpha(black, 0.3), 0 4px 14px alpha(black, 0.25); } \
         menubutton.add-float image { -gtk-icon-size: 22px; }",
    );

    let grid_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    grid_area.append(&grid_scroller);
    grid_area.append(&empty_status);
    let grid_overlay = gtk::Overlay::builder().child(&grid_area).vexpand(true).build();
    grid_overlay.add_overlay(&add_button);

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.append(&search_revealer);
    sidebar_box.append(&grid_overlay);

    // Archive/Groups/Delete on the start side; Clear All and Done on
    // the end side, Clear All just to Done's left since it's an
    // infrastructure action over the selection, not a card-modifying
    // one. See build_multiselect_toolbar for construction details.
    let MultiselectToolbarWidgets {
        star_button: multiselect_star_button,
        archive_button: multiselect_archive_button,
        change_groups_button: multiselect_change_groups_button,
        delete_button: multiselect_delete_button,
        clear_all_button: multiselect_clear_all_button,
        done_button: multiselect_done_button,
        action_bar_revealer: multiselect_action_bar_revealer,
    } = build_multiselect_toolbar();
    multiselect_action_bar_revealer.set_valign(gtk::Align::End);
    grid_overlay.add_overlay(&multiselect_action_bar_revealer);

    let search_toggle_button = gtk::ToggleButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text("Search")
        .active(ui_state.borrow().search_bar_visible)
        .build();
    // Matches the button's initial active state, since connect_toggled
    // below is attached after .build() and wouldn't fire for it.
    search_revealer.set_reveal_child(ui_state.borrow().search_bar_visible);
    {
        let search_entry = search_entry.clone();
        let search_revealer = search_revealer.clone();
        let ui_state = ui_state.clone();
        search_toggle_button.connect_toggled(move |btn| {
            let showing = btn.is_active();
            search_revealer.set_reveal_child(showing);
            ui_state.borrow_mut().search_bar_visible = showing;
            ui_state.borrow().save();
            if showing {
                search_entry.grab_focus();
            } else {
                // Clears the filter rather than leaving it applied but
                // hidden. set_text fires search-changed (which fires on
                // programmatic changes too), which already calls
                // refresh(), so no separate rebuild call is needed.
                search_entry.set_text("");
            }
        });
    }

    // Constructed before build_sidebar_headerbar/build_detail_pane so
    // one instance can be threaded into each as that page's second top
    // bar. use_markup(false) on both, since error text routinely
    // contains raw file paths and io::Error messages that could
    // otherwise be parsed as Pango markup.
    let banner_css_provider = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &default_display(),
        &banner_css_provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );
    let banner = SharedBanner {
        sidebar: adw::Banner::builder().use_markup(false).build(),
        content: adw::Banner::builder().use_markup(false).build(),
        css_provider: banner_css_provider,
    };
    banner.sidebar.set_widget_name("barkeep-banner-sidebar");
    banner.content.set_widget_name("barkeep-banner-content");
    // Wired once, not per-notification: every message uses this same
    // acknowledge-and-dismiss action.
    //
    // Both instances dismissed together from either button. An earlier
    // version only dismissed whichever instance's button was clicked,
    // leaving the other pane's instance still revealed next time that
    // pane became visible.
    let sidebar_for_dismiss = banner.sidebar.clone();
    let content_for_dismiss = banner.content.clone();
    for b in [&banner.sidebar, &banner.content] {
        b.set_button_label(Some("OK"));
        let sidebar_for_dismiss = sidebar_for_dismiss.clone();
        let content_for_dismiss = content_for_dismiss.clone();
        b.connect_button_clicked(move |_| {
            sidebar_for_dismiss.set_revealed(false);
            content_for_dismiss.set_revealed(false);
        });
    }

    // See build_sidebar_headerbar for construction details.
    let sidebar_page = build_sidebar_headerbar(&menu_button, &search_toggle_button, &sidebar_box, &banner.sidebar);

    // --- Detail pane: barcode + card info ---------------------------------
    // See build_detail_pane for construction details.
    let DetailPaneWidgets {
        detail_status,
        content_toolbar,
        content_header,
        bottom_bar,
        edit_button,
        star_button,
        archive_button,
        copy_code_action,
        copy_image_action,
        share_menu_button,
        info_button,
        link_button,
        edit_handler,
        star_handler,
        archive_handler,
        copy_code_handler,
        copy_image_handler,
        link_handler,
        content_page,
    } = build_detail_pane(&banner.content);

    // --- Split view: permanently collapsed, not responsive to width
    // anymore; see build_split_view_and_window for construction details
    // --------------------------------------------------------
    let SplitViewAndWindow {
        split_view,
        toast_overlay,
        window,
        name_label_css,
        name_label_colors,
    } = build_split_view_and_window(app, &sidebar_page, &content_page);

    // --- Wiring --------------------------------------------------------
    // Grid-only half of refresh, split out so the star-toggle handler
    // can re-sort/rebuild the grid without also resetting the detail
    // pane to "no card selected".
    // show_archived means "show" (true = shown), the inverse of
    // UiState's "hide_archived"; inverted here on load, matching how
    // show_archived_action's handler inverts on every toggle change.
    let show_archived: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(!ui_state.borrow().hide_archived));
    // show_valid_only matches UiState's "hide_invalid" directly, not
    // inverted, matching show_valid_only_action's existing logic.
    let show_valid_only: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(ui_state.borrow().hide_invalid));
    // None = "All". Restored from ui_state, safe even if the persisted
    // group id no longer exists, since rebuild_group_tabs' existing
    // "does this group still exist" check resets to None if not.
    let selected_group_filter: Rc<RefCell<Option<Uuid>>> = Rc::new(RefCell::new(ui_state.borrow().selected_group));
    // Tracked by card ID rather than widget reference: rebuild_grid
    // tears down and recreates every tile from scratch on every call,
    // so a selection tracked on the widgets wouldn't survive that.
    let selected_card_ids: Rc<RefCell<HashSet<Uuid>>> = Rc::new(RefCell::new(HashSet::new()));
    // One CssProvider per card, reused across rebuild_grid calls
    // instead of a fresh one each time, which previously accumulated
    // unbounded for the session since GTK has no way to know an old
    // one should be dropped. build_card_tile reuses this cache's
    // provider for a card it's already seen, only registering a new
    // one the first time. Pruned of ids no longer in the store at the
    // top of every rebuild_grid call.
    let tile_css_providers: Rc<RefCell<HashMap<Uuid, gtk::CssProvider>>> = Rc::new(RefCell::new(HashMap::new()));
    // A separate flag from whether selected_card_ids happens to be
    // empty: chaining several bulk actions against the same session
    // means briefly emptying the selection shouldn't exit the whole
    // mode. Only the Done button turns this off.
    let multiselect_mode_active: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));

    // Lets each tile's right-click/long-press handlers trigger a full
    // grid rebuild after toggling a selection, without rebuild_grid's
    // closure needing to capture itself. Box<dyn Fn()> since a bare
    // `dyn Fn()` behind a RefCell can't be reassigned to a different
    // closure after construction (see open_manage_groups_dialog's
    // rebuild_rows for the same pattern).

    let rebuild_grid_handle: Rc<RefCell<Box<dyn Fn()>>> = Rc::new(RefCell::new(Box::new(|| {})));

    let rebuild_grid: Rc<RefCell<dyn Fn()>> = {
        let store = store.clone();
        let flow_box = flow_box.clone();
        let grid_scroller = grid_scroller.clone();
        let empty_status = empty_status.clone();
        let search_entry = search_entry.clone();
        let show_archived = show_archived.clone();
        let show_valid_only = show_valid_only.clone();
        let selected_group_filter = selected_group_filter.clone();
        let selected_card_ids = selected_card_ids.clone();
        let multiselect_mode_active = multiselect_mode_active.clone();
        let ui_state = ui_state.clone();
        let rebuild_grid_handle = rebuild_grid_handle.clone();
        let multiselect_action_bar_revealer = multiselect_action_bar_revealer.clone();
        let add_button = add_button.clone();
        let multiselect_archive_button = multiselect_archive_button.clone();
        let multiselect_star_button = multiselect_star_button.clone();
        let tile_css_providers = tile_css_providers.clone();

        Rc::new(RefCell::new(move || {
            // Saved before tearing down flow_box, restored via
            // idle_add_local_once once rebuilding is done. Without
            // this, every rebuild_grid call (even one triggered by
            // toggling a card's selection) was jumping the list back
            // to the top, since flow_box's old widgets no longer exist
            // once torn down and recreated.
            let saved_scroll_position = grid_scroller.vadjustment().value();
            // Only saved when the group filter actually differs from
            // what's persisted, since rebuild_grid runs on every
            // search keystroke and selection toggle too.
            let current_group_filter = *selected_group_filter.borrow();
            if ui_state.borrow().selected_group != current_group_filter {
                ui_state.borrow_mut().selected_group = current_group_filter;
                ui_state.borrow().save();
            }
            while let Some(child) = flow_box.child_at_index(0) {
                flow_box.remove(&child);
            }
            let query = search_entry.text();
            let store_ref = store.borrow();
            // Prunes any cached provider whose card no longer exists
            // (deleted since the last rebuild), not just currently
            // filtered out. Explicitly unregisters each pruned provider
            // from the display's style context; removing only this
            // struct's reference wouldn't deregister it, since GTK's
            // style-context machinery holds its own reference once a
            // provider's registered.
            {
                let mut providers = tile_css_providers.borrow_mut();
                let live_ids: HashSet<Uuid> = store_ref.cards.iter().map(|c| c.id).collect();
                let stale_ids: Vec<Uuid> = providers.keys().filter(|id| !live_ids.contains(id)).copied().collect();
                if !stale_ids.is_empty() {
                    let display = default_display();
                    for id in stale_ids {
                        if let Some(provider) = providers.remove(&id) {
                            gtk::style_context_remove_provider_for_display(&display, &provider);
                        }
                    }
                }
            }
            let multiselect_active = multiselect_mode_active.get();
            // Tracked alongside the tile-building loop rather than a
            // separate pass: whether every visible selected card is
            // archived/starred (for the Archive/Star buttons' dynamic
            // labels) is available from the same iteration already
            // looking at each card's state.
            let mut any_visible_selected = false;
            let mut all_visible_selected_archived = true;
            let mut all_visible_selected_starred = true;
            for card in store_ref.search(
                &query,
                show_archived.get(),
                show_valid_only.get(),
                &today_iso(),
                *selected_group_filter.borrow(),
            ) {
                let selection_badge = if !multiselect_active {
                    TileSelectionBadge::None
                } else if selected_card_ids.borrow().contains(&card.id) {
                    TileSelectionBadge::Checked
                } else {
                    TileSelectionBadge::Unchecked
                };
                if selection_badge == TileSelectionBadge::Checked {
                    any_visible_selected = true;
                    if !card.archived {
                        all_visible_selected_archived = false;
                    }
                    if !card.starred {
                        all_visible_selected_starred = false;
                    }
                }
                let tile = build_card_tile(card, selection_badge, &tile_css_providers);

                // Right-click and long-press both enter/toggle
                // multiselect for this tile, both calling
                // toggle_card_selection so the selection logic lives in
                // one place. Both set multiselect_mode_active true
                // unconditionally; harmless if already true, and
                // simpler than branching on first-entry vs already-active.
                let card_id = card.id;
                let right_click = gtk::GestureClick::new();
                right_click.set_button(gtk::gdk::BUTTON_SECONDARY);
                {
                    let selected_card_ids = selected_card_ids.clone();
                    let multiselect_mode_active = multiselect_mode_active.clone();
                    let rebuild_grid_handle = rebuild_grid_handle.clone();
                    right_click.connect_pressed(move |gesture, _, _, _| {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        multiselect_mode_active.set(true);
                        toggle_card_selection(&selected_card_ids, card_id);
                        (rebuild_grid_handle.borrow())();
                    });
                }
                tile.add_controller(right_click);

                let long_press = gtk::GestureLongPress::new();
                // ~33% longer than the system default, to cut down on
                // accidental triggers.
                long_press.set_delay_factor(1.33);
                {
                    let selected_card_ids = selected_card_ids.clone();
                    let multiselect_mode_active = multiselect_mode_active.clone();
                    let rebuild_grid_handle = rebuild_grid_handle.clone();
                    long_press.connect_pressed(move |gesture, _, _| {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        multiselect_mode_active.set(true);
                        toggle_card_selection(&selected_card_ids, card_id);
                        (rebuild_grid_handle.borrow())();
                    });
                }
                tile.add_controller(long_press);

                flow_box.append(&tile);
            }

            multiselect_action_bar_revealer.set_reveal_child(multiselect_active);
            add_button.set_visible(!multiselect_active);
            // "Unarchive" only when there's at least one visible
            // selected card and every one is already archived; "Archive"
            // otherwise. Sets the tooltip, not the label, since the
            // button is icon-only (same shoe-box-symbolic icon either
            // way). Also sets active on the same condition, matching
            // the per-card ToggleButton's set_active(card.archived).
            let should_show_unarchive = any_visible_selected && all_visible_selected_archived;
            multiselect_archive_button.set_tooltip_text(Some(if should_show_unarchive {
                "Unarchive"
            } else {
                "Archive"
            }));
            multiselect_archive_button.set_active(should_show_unarchive);

            // Same logic as Archive, for Star.
            let should_show_unstar = any_visible_selected && all_visible_selected_starred;
            multiselect_star_button.set_tooltip_text(Some(if should_show_unstar {
                "Unstar"
            } else {
                "Star"
            }));
            multiselect_star_button.set_active(should_show_unstar);

            // Checks the store's real card count, not the search
            // results: a search with no matches should still show the
            // empty grid, not this welcome page.
            let has_cards = !store_ref.cards.is_empty();
            grid_scroller.set_visible(has_cards);
            empty_status.set_visible(!has_cards);

            // Deferred: grid_scroller's adjustment doesn't have correct
            // bounds for the newly-appended content until GTK has run a
            // layout pass, so setting the value synchronously here
            // could clamp against the old bounds.
            let grid_scroller = grid_scroller.clone();
            glib::source::idle_add_local_once(move || {
                grid_scroller.vadjustment().set_value(saved_scroll_position);
            });
        }))
    };
    // Replaces rebuild_grid_handle's placeholder now that rebuild_grid
    // exists; every gesture handler above holds the handle itself, not
    // a snapshot, so they see this new content from now on.
    {
        let rebuild_grid = rebuild_grid.clone();
        *rebuild_grid_handle.borrow_mut() = Box::new(move || {
            (rebuild_grid.borrow())();
        });
    }

    // Only the Done button exits multiselect now. Esc used to double
    // as an exit, and deselecting the last card used to exit implicitly
    // under the old "mode == set non-empty" definition; neither fits
    // the new decoupled one (see multiselect_mode_active).
    {
        let selected_card_ids = selected_card_ids.clone();
        let multiselect_mode_active = multiselect_mode_active.clone();
        let rebuild_grid = rebuild_grid.clone();
        multiselect_done_button.connect_clicked(move |_| {
            selected_card_ids.borrow_mut().clear();
            multiselect_mode_active.set(false);
            (rebuild_grid.borrow())();
        });
    }

    {
        let selected_card_ids = selected_card_ids.clone();
        let rebuild_grid = rebuild_grid.clone();
        multiselect_clear_all_button.connect_clicked(move |_| {
            // Doesn't touch multiselect_mode_active, unlike Done, so
            // the mode stays active for a fresh sub-selection.
            selected_card_ids.borrow_mut().clear();
            (rebuild_grid.borrow())();
        });
    }

    // Plain gtk::Box (not FlowBox, which wraps to multiple rows) inside
    // a horizontal-only ScrolledWindow, so it scrolls sideways rather
    // than wrapping. Hidden entirely, not just an empty "All"-only
    // strip, until at least one group exists.
    let group_tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // halign only repositions a widget within its allocated space, and
    // only takes effect when that exceeds its natural size. When tabs
    // fit the viewport there's slack to center within; when they don't
    // (need scrolling), there's no slack and it starts from the left
    // edge on its own, so "center when there's room, left-justified
    // otherwise" falls out of this one property.
    group_tabs_box.set_halign(gtk::Align::Center);
    group_tabs_box.set_margin_start(4);
    group_tabs_box.set_margin_end(4);
    group_tabs_box.set_margin_top(4);
    group_tabs_box.set_margin_bottom(4);
    // Four attempts at hand-rolling "keep the selected tab visible"
    // (grab_focus, single idle callback, polling for non-zero bounds,
    // polling for stable bounds) all failed. GTK's native scrollbar
    // (Automatic) solves "what's off-screen" without reimplementing it.
    //
    // window_placement(BottomLeft) moves the scrollbar to the top edge:
    // GTK's default (TopLeft) pushes it to the bottom, where it was
    // expanding on hover and covering the tab row underneath it.
    let group_tabs_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .window_placement(gtk::CornerType::BottomLeft)
        .hexpand(true)
        .min_content_width(0)
        .max_content_width(240)
        .child(&group_tabs_box)
        .build();

    // Underline under the checked tab, flat otherwise, lighter than
    // the pill look used for the edit dialog's multi-select picker.
    // 13px top / 5px bottom padding (18px total) shifts the text down
    // within the row rather than changing the row's overall height.
    register_css(
        ".group-tab { padding: 13px 10px 5px 10px; border-radius: 0; \
         border-bottom: 2px solid transparent; background: none; box-shadow: none; } \
         .group-tab:checked { border-bottom: 2px solid var(--accent-color); font-weight: bold; }",
    );

    // Wired further below, after rebuild_group_tabs exists, since
    // jumping to the next/previous tab (rather than paging by a fixed
    // pixel amount) needs to inspect the current tab order.
    let group_tabs_prev = gtk::Button::from_icon_name("go-previous-symbolic");
    group_tabs_prev.add_css_class("flat");
    let group_tabs_next = gtk::Button::from_icon_name("go-next-symbolic");
    group_tabs_next.add_css_class("flat");

    let group_tabs_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    group_tabs_row.append(&group_tabs_prev);
    group_tabs_row.append(&group_tabs_scroller);
    group_tabs_row.append(&group_tabs_next);
    group_tabs_row.set_visible(false);
    // insert_child_after places this between search_revealer and
    // grid_scroller regardless of declaration order (append() would
    // otherwise tack it onto the end). References search_revealer, not
    // search_entry: search_entry is wrapped in the Revealer, so it's no
    // longer a direct child of sidebar_box. This was pointing at
    // search_entry until now, silently failing to find a valid sibling,
    // which was the actual cause of group tabs not showing up.
    sidebar_box.insert_child_after(&group_tabs_row, Some(&search_revealer));

    // Hides the </> paging buttons when the tab strip already fits the
    // visible width. Also disables each one at whichever extreme
    // scrolling further wouldn't do anything, since clicking a paging
    // button already at its extreme did "unexpected things." Driven by
    // the adjustment's "upper"/"page-size"/"value" notifications rather
    // than manually re-checking after every trigger, since paging and
    // sensitivity only depend on those three properties.
    {
        let hadjustment = group_tabs_scroller.hadjustment();
        let group_tabs_prev = group_tabs_prev.clone();
        let group_tabs_next = group_tabs_next.clone();
        let update_paging_visibility = move |adj: &gtk::Adjustment| {
            let scrolling_needed = adj.upper() > adj.page_size();
            group_tabs_prev.set_visible(scrolling_needed);
            group_tabs_next.set_visible(scrolling_needed);
            let max_value = (adj.upper() - adj.page_size()).max(adj.lower());
            group_tabs_prev.set_sensitive(adj.value() > adj.lower());
            group_tabs_next.set_sensitive(adj.value() < max_value);
        };
        let update_paging_visibility = Rc::new(update_paging_visibility);
        // Every invocation deferred via idle_add_local_once, not just
        // the initial one: deferring only the first call was an
        // incomplete fix, since rebuild_group_tabs destroys and
        // recreates every tab button on every change, which can fire
        // "upper"/"page-size" notify signals for intermediate,
        // not-yet-settled layout states, not just the final one.
        let defer_update: Rc<dyn Fn()> = {
            let update_paging_visibility = update_paging_visibility.clone();
            let hadjustment = hadjustment.clone();
            Rc::new(move || {
                let update_paging_visibility = update_paging_visibility.clone();
                let hadjustment = hadjustment.clone();
                glib::source::idle_add_local_once(move || {
                    update_paging_visibility(&hadjustment);
                });
            })
        };
        defer_update();
        {
            let defer_update = defer_update.clone();
            hadjustment.connect_notify_local(Some("upper"), move |_, _| {
                defer_update();
            });
        }
        {
            let defer_update = defer_update.clone();
            hadjustment.connect_notify_local(Some("page-size"), move |_, _| {
                defer_update();
            });
        }
        {
            let defer_update = defer_update.clone();
            hadjustment.connect_notify_local(Some("value"), move |_, _| {
                defer_update();
            });
        }
    }

    // Rebuilds the tab strip from scratch (same pattern as
    // rebuild_grid), needed whenever the groups list changes shape.
    // set_group links each tab to "All" for GTK4's native
    // mutual-exclusion behavior: exactly one tab active at a time.
    let rebuild_group_tabs: Rc<RefCell<dyn Fn()>> = {
        let store = store.clone();
        let group_tabs_box = group_tabs_box.clone();
        let group_tabs_row = group_tabs_row.clone();
        let selected_group_filter = selected_group_filter.clone();
        let rebuild_grid = rebuild_grid.clone();
        let show_archived = show_archived.clone();
        let show_valid_only = show_valid_only.clone();
        Rc::new(RefCell::new(move || {
            while let Some(child) = group_tabs_box.first_child() {
                group_tabs_box.remove(&child);
            }
            // Only groups with at least one card visible under the
            // current archived/valid-only filters, matching what the
            // grid itself shows. Search text is excluded though:
            // passing an empty query means groups don't flicker in and
            // out of the tab bar while someone's mid-search. Reuses
            // CardStore::search directly rather than reimplementing its
            // filtering rules separately. Still shown in full on the
            // add/edit dialog's own group picker, where an empty group
            // is exactly the one you'd want to assign a first card to.
            let store_ref = store.borrow();
            let today = today_iso();
            let groups: Vec<Group> = store_ref
                .groups
                .iter()
                .filter(|g| {
                    !store_ref
                        .search("", show_archived.get(), show_valid_only.get(), &today, Some(g.id))
                        .is_empty()
                })
                .cloned()
                .collect();
            drop(store_ref);

            // If the currently-selected filter points at a group that
            // just stopped being shown (its last card was removed or
            // unassigned, leaving it empty), reset to "All" instead of
            // leaving the grid stuck filtered to a tab that's no longer
            // there to click back from.
            //
            // Extracted as its own statement rather than borrowed
            // directly in the if-let condition: a temporary Ref guard
            // created in an if-let's scrutinee stays alive for the
            // entire block, not just the condition check, so the
            // borrow_mut() below was panicking ("RefCell already
            // borrowed") against it.
            let current_selection = *selected_group_filter.borrow();
            let mut selection_reset = false;
            if let Some(selected_id) = current_selection {
                if !groups.iter().any(|g| g.id == selected_id) {
                    *selected_group_filter.borrow_mut() = None;
                    selection_reset = true;
                }
            }
            if selection_reset {
                (rebuild_grid.borrow())();
            }

            group_tabs_row.set_visible(!groups.is_empty());
            if groups.is_empty() {
                return;
            }

            let all_tab = gtk::ToggleButton::builder()
                .label("All")
                .active(selected_group_filter.borrow().is_none())
                .css_classes(["flat", "group-tab"])
                .build();
            {
                let selected_group_filter = selected_group_filter.clone();
                let rebuild_grid = rebuild_grid.clone();
                all_tab.connect_toggled(move |btn| {
                    if btn.is_active() {
                        *selected_group_filter.borrow_mut() = None;
                        (rebuild_grid.borrow())();
                    }
                });
            }
            group_tabs_box.append(&all_tab);

            for group in &groups {
                let tab = gtk::ToggleButton::builder()
                    .label(&group.name)
                    .active(*selected_group_filter.borrow() == Some(group.id))
                    .css_classes(["flat", "group-tab"])
                    .build();
                tab.set_group(Some(&all_tab));
                let selected_group_filter = selected_group_filter.clone();
                let rebuild_grid = rebuild_grid.clone();
                let group_id = group.id;
                tab.connect_toggled(move |btn| {
                    if btn.is_active() {
                        *selected_group_filter.borrow_mut() = Some(group_id);
                        (rebuild_grid.borrow())();
                    }
                });
                group_tabs_box.append(&tab);
            }
            // No manual "scroll the active tab into view" logic; GTK's
            // own scrollbar keeps itself in sync (see group_tabs_scroller).
        }))
    };

    {
        let store = store.clone();
        let selected_card_ids = selected_card_ids.clone();
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let search_entry = search_entry.clone();
        let show_archived = show_archived.clone();
        let show_valid_only = show_valid_only.clone();
        let selected_group_filter = selected_group_filter.clone();
        let banner = banner.clone();
        // A separate clone for the closure, since connect_clicked
        // borrows its receiver for the whole call expression, so a
        // move closure built as part of that same call can't also move
        // the identical variable into itself (E0505).
        let multiselect_archive_button_for_tooltip = multiselect_archive_button.clone();
        multiselect_archive_button.connect_clicked(move |_| {
            // The button's tooltip already says which direction this
            // click means, kept in sync by rebuild_grid.
            let target_archived = multiselect_archive_button_for_tooltip.tooltip_text().as_deref() != Some("Unarchive");

            // See visible_selected_card_ids for why this re-derives at
            // click time rather than acting on the full selection.
            let visible_selected_ids = visible_selected_card_ids(
                &store.borrow(),
                &search_entry.text(),
                show_archived.get(),
                show_valid_only.get(),
                *selected_group_filter.borrow(),
                &selected_card_ids.borrow(),
            );

            report_if_save_failed(&banner, store.borrow_mut().set_archived_bulk(&visible_selected_ids, target_archived));

            // Selection is left intact so chaining several bulk actions
            // against it doesn't require re-selecting the same cards.
            // Only Done exits multiselect now.
            (rebuild_grid.borrow())();
            // Archiving/unarchiving changes whether a card counts as
            // visible under show_archived, which group tabs' filtering
            // depends on.
            (rebuild_group_tabs.borrow())();
        });
    }

    {
        let store = store.clone();
        let selected_card_ids = selected_card_ids.clone();
        let rebuild_grid = rebuild_grid.clone();
        let search_entry = search_entry.clone();
        let show_archived = show_archived.clone();
        let show_valid_only = show_valid_only.clone();
        let selected_group_filter = selected_group_filter.clone();
        let banner = banner.clone();
        let multiselect_star_button_for_tooltip = multiselect_star_button.clone();
        multiselect_star_button.connect_clicked(move |_| {
            // Same as Archive's click handler above; no
            // rebuild_group_tabs call here since starring doesn't
            // change whether a card counts as visible under any filter.
            let target_starred = multiselect_star_button_for_tooltip.tooltip_text().as_deref() != Some("Unstar");

            let visible_selected_ids = visible_selected_card_ids(
                &store.borrow(),
                &search_entry.text(),
                show_archived.get(),
                show_valid_only.get(),
                *selected_group_filter.borrow(),
                &selected_card_ids.borrow(),
            );

            report_if_save_failed(&banner, store.borrow_mut().set_starred_bulk(&visible_selected_ids, target_starred));

            (rebuild_grid.borrow())();
        });
    }

    {
        let window = window.clone();
        let banner = banner.clone();
        let store = store.clone();
        let selected_card_ids = selected_card_ids.clone();
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let search_entry = search_entry.clone();
        let show_archived = show_archived.clone();
        let show_valid_only = show_valid_only.clone();
        let selected_group_filter = selected_group_filter.clone();
        multiselect_delete_button.connect_clicked(move |_| {
            let visible_selected_ids = visible_selected_card_ids(
                &store.borrow(),
                &search_entry.text(),
                show_archived.get(),
                show_valid_only.get(),
                *selected_group_filter.borrow(),
                &selected_card_ids.borrow(),
            );

            if visible_selected_ids.is_empty() {
                return;
            }

            let heading = "Delete Cards?";
            let body = format!("This will permanently delete {} cards.", visible_selected_ids.len());
            let window = window.clone();
            let banner = banner.clone();
            let store = store.clone();
            let selected_card_ids = selected_card_ids.clone();
            let rebuild_grid = rebuild_grid.clone();
            let rebuild_group_tabs = rebuild_group_tabs.clone();
            confirm_delete(&window, heading, &body, move || {
                report_if_save_failed(&banner, store.borrow_mut().remove_bulk(&visible_selected_ids));
                // Removes just the ids that were actually deleted, not
                // the whole selection: a hidden-but-still-selected card
                // was never touched by this action, so it stays selected.
                let mut ids = selected_card_ids.borrow_mut();
                for id in &visible_selected_ids {
                    ids.remove(id);
                }
                drop(ids);
                (rebuild_grid.borrow())();
                // A deleted card could have been the last one in some group.
                (rebuild_group_tabs.borrow())();
            });
        });
    }

    {
        let window = window.clone();
        let banner = banner.clone();
        let store = store.clone();
        let selected_card_ids = selected_card_ids.clone();
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let search_entry = search_entry.clone();
        let show_archived = show_archived.clone();
        let show_valid_only = show_valid_only.clone();
        let selected_group_filter = selected_group_filter.clone();
        multiselect_change_groups_button.connect_clicked(move |_| {
            let visible_selected_ids = visible_selected_card_ids(
                &store.borrow(),
                &search_entry.text(),
                show_archived.get(),
                show_valid_only.get(),
                *selected_group_filter.borrow(),
                &selected_card_ids.borrow(),
            );

            if visible_selected_ids.is_empty() {
                return;
            }

            // Separate clones for the direct arguments versus
            // on_applied's closure (same borrow/move conflict as
            // elsewhere).
            let rebuild_grid_for_dialog = rebuild_grid.clone();
            let rebuild_group_tabs_for_dialog = rebuild_group_tabs.clone();
            let selected_group_filter_for_dialog = selected_group_filter.clone();
            let rebuild_grid = rebuild_grid.clone();
            let rebuild_group_tabs = rebuild_group_tabs.clone();
            open_change_groups_dialog(
                &window,
                &banner,
                store.clone(),
                visible_selected_ids,
                rebuild_group_tabs_for_dialog,
                rebuild_grid_for_dialog,
                selected_group_filter_for_dialog,
                None,
                Box::new(move || {
                    (rebuild_grid.borrow())();
                    // A group-membership change could make some
                    // group's tab appear or disappear.
                    (rebuild_group_tabs.borrow())();
                }),
            );
        });
    }


    // A tab-jumping version, with its own "keep the selected tab
    // visible" logic, led into the centering problem in the first
    // place; pixel-based paging doesn't need to know where any
    // particular tab is. Wraps at the ends as a scroll-position wrap
    // rather than a specific-tab jump.
    const GROUP_TABS_PAGE_STEP: f64 = 150.0;
    let step_group_filter: Rc<dyn Fn(i32)> = {
        let group_tabs_scroller = group_tabs_scroller.clone();
        Rc::new(move |delta: i32| {
            let adj = group_tabs_scroller.hadjustment();
            let max_value = (adj.upper() - adj.page_size()).max(adj.lower());
            let current = adj.value();
            let new_value = if delta > 0 && current >= max_value - 1.0 {
                adj.lower()
            } else if delta < 0 && current <= adj.lower() + 1.0 {
                max_value
            } else {
                (current + delta as f64 * GROUP_TABS_PAGE_STEP).clamp(adj.lower(), max_value)
            };
            adj.set_value(new_value);
        })
    };
    {
        let step_group_filter = step_group_filter.clone();
        group_tabs_prev.connect_clicked(move |_| step_group_filter(-1));
    }
    {
        let step_group_filter = step_group_filter.clone();
        group_tabs_next.connect_clicked(move |_| step_group_filter(1));
    }

    let refresh: Rc<RefCell<dyn Fn()>> = {
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let split_view = split_view.clone();
        let content_page = content_page.clone();
        let content_toolbar = content_toolbar.clone();
        let content_header = content_header.clone();
        let detail_status = detail_status.clone();
        let edit_button = edit_button.clone();
        let info_button = info_button.clone();
        let star_button = star_button.clone();
        let star_handler = star_handler.clone();
        let archive_button = archive_button.clone();
        let archive_handler = archive_handler.clone();
        let share_menu_button = share_menu_button.clone();
        let copy_code_action = copy_code_action.clone();
        let copy_code_handler = copy_code_handler.clone();
        let copy_image_action = copy_image_action.clone();
        let copy_image_handler = copy_image_handler.clone();
        let link_button = link_button.clone();
        let link_handler = link_handler.clone();
        let bottom_bar = bottom_bar.clone();
        let name_label_css = name_label_css.clone();

        Rc::new(RefCell::new(move || {
            (rebuild_grid.borrow())();
            // A card add/delete/edit reaching refresh() (the on_saved
            // path bypasses refresh to keep the detail view open,
            // calling rebuild_group_tabs separately) could change which
            // groups are empty under the current filters.
            (rebuild_group_tabs.borrow())();

            content_header.set_title_widget(Some(&adw::WindowTitle::new("", "")));
            // Neutral rather than leaving the last-viewed card's color
            // on a header with no card showing.
            name_label_css.load_from_string("");
            content_toolbar.set_content(Some(&detail_status));
            edit_button.set_visible(false);
            info_button.set_visible(false);
            info_button.set_popover(gtk::Popover::NONE);
            bottom_bar.set_visible(false);
            // set_active() fires "toggled" on programmatic changes too,
            // not just clicks, so disconnect before resetting.
            disconnect_if_present(&star_button, &star_handler);
            star_button.set_active(false);
            star_button.set_visible(false);
            disconnect_if_present(&archive_button, &archive_handler);
            archive_button.set_active(false);
            archive_button.set_visible(false);
            disconnect_if_present(&copy_code_action, &copy_code_handler);
            disconnect_if_present(&copy_image_action, &copy_image_handler);
            share_menu_button.set_visible(false);
            disconnect_if_present(&link_button, &link_handler);
            link_button.set_visible(false);
            let _ = (&split_view, &content_page);
        }))
    };

    // Expired-card housekeeping: once now, before the first render, so
    // a card that already expired while the app was closed doesn't
    // flash on screen unarchived/undeleted first, and again at every
    // local midnight (see schedule_midnight_expiration_check below).
    match store.borrow_mut().apply_expiration_actions(&today_iso()) {
        Ok(_) => {}
        Err(e) => show_banner(&banner, BannerSeverity::Error, &format!("Couldn't update expired cards: {e}")),
    }
    {
        let store = store.clone();
        let rebuild_grid = rebuild_grid.clone();
        let banner = banner.clone();
        schedule_midnight_expiration_check(store, rebuild_grid, banner);
    }

    // Called directly; rebuild_grid doesn't measure or depend on the
    // window's current size.
    (refresh.borrow())();
    (rebuild_group_tabs.borrow())();

    {
        let refresh = refresh.clone();
        search_entry.connect_search_changed(move |_| {
            (refresh.borrow())();
        });
    }

    {
        let store = store.clone();
        let content_toolbar = content_toolbar.clone();
        let content_header = content_header.clone();
        let split_view = split_view.clone();
        let refresh = refresh.clone();
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let window_weak = window.downgrade();
        let banner = banner.clone();
        let edit_button = edit_button.clone();
        let info_button = info_button.clone();
        let star_button = star_button.clone();
        let toast_overlay = toast_overlay.clone();
        let edit_handler = edit_handler.clone();
        let star_handler = star_handler.clone();
        let archive_button = archive_button.clone();
        let archive_handler = archive_handler.clone();
        let share_menu_button = share_menu_button.clone();
        let copy_code_action = copy_code_action.clone();
        let copy_code_handler = copy_code_handler.clone();
        let copy_image_action = copy_image_action.clone();
        let copy_image_handler = copy_image_handler.clone();
        let link_button = link_button.clone();
        let link_handler = link_handler.clone();
        let bottom_bar = bottom_bar.clone();
        let name_label_css = name_label_css.clone();
        let name_label_colors = name_label_colors.clone();
        let search_entry = search_entry.clone();
        let show_archived = show_archived.clone();
        let show_valid_only = show_valid_only.clone();
        let selected_group_filter = selected_group_filter.clone();
        let selected_card_ids = selected_card_ids.clone();
        let multiselect_mode_active = multiselect_mode_active.clone();
        flow_box.connect_child_activated(move |_, child| {
            let index = child.index();
            let store_ref = store.borrow();
            // Matches the query and filter state rebuild_grid actually
            // filtered flow_box by, not the unfiltered list: mapping
            // this index against the full card list landed on the
            // wrong card whenever a search was narrowing the grid.
            let cards = store_ref.search(
                &search_entry.text(),
                show_archived.get(),
                show_valid_only.get(),
                &today_iso(),
                *selected_group_filter.borrow(),
            );
            let Some(card) = cards.get(index as usize).cloned().cloned() else {
                return;
            };
            drop(store_ref);

            // While multiselect is active, a tap toggles selection
            // instead of opening the card. Checked before
            // show_card_detail so an active session never accidentally
            // opens a card's detail view.
            if multiselect_mode_active.get() {
                toggle_card_selection(&selected_card_ids, card.id);
                (rebuild_grid.borrow())();
                return;
            }

            show_card_detail(
                &card,
                &content_toolbar,
                &content_header,
                &store,
                &refresh,
                &rebuild_grid,
                &rebuild_group_tabs,
                &selected_group_filter,
                &split_view,
                &window_weak,
                &banner,
                &edit_button,
                &info_button,
                &star_button,
                &archive_button,
                &share_menu_button,
                &copy_code_action,
                &copy_image_action,
                &link_button,
                &bottom_bar,
                &edit_handler,
                &star_handler,
                &archive_handler,
                &copy_code_handler,
                &copy_image_handler,
                &link_handler,
                &show_archived,
                &name_label_css,
                &name_label_colors,
                &toast_overlay,
            );
            split_view.set_show_content(true);
        });
    }

    let enter_manually_action = gio::SimpleAction::new("enter-manually", None);
    {
        let store = store.clone();
        let refresh = refresh.clone();
        let split_view = split_view.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        let window_weak = window.downgrade();
        let banner = banner.clone();
        enter_manually_action.connect_activate(move |_, _| {
            let Some(win) = window_weak.upgrade() else { return };
            open_edit_dialog(&win, &banner, {
                let mut c = Card::new("");
                c.color = random_card_color(c.id);
                c
            }, true, store.clone(), refresh.clone(), split_view.clone(), Rc::new(None), rebuild_group_tabs.clone(), rebuild_grid.clone(), selected_group_filter.clone(), EditDialogTab::Card);
        });
    }
    window.add_action(&enter_manually_action);

    let scan_image_action = gio::SimpleAction::new("scan-image", None);
    {
        let store = store.clone();
        let refresh = refresh.clone();
        let split_view = split_view.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        let window_weak = window.downgrade();
        let banner = banner.clone();
        scan_image_action.connect_activate(move |_, _| {
            let Some(win) = window_weak.upgrade() else { return };
            let file_dialog = gtk::FileDialog::builder().title("Scan from Image").build();
            let store = store.clone();
            let refresh = refresh.clone();
            let split_view = split_view.clone();
            let rebuild_group_tabs = rebuild_group_tabs.clone();
            let rebuild_grid = rebuild_grid.clone();
            let selected_group_filter = selected_group_filter.clone();
            let win_for_result = win.clone();
            let banner = banner.clone();
            file_dialog.open(Some(&win), gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                match barcode::decode_image(&path) {
                    Some((value, kind)) => {
                        let mut prefill = Card::new("");
                        prefill.color = random_card_color(prefill.id);
                        prefill.code_value = value;
                        prefill.barcode_type = kind;
                        open_edit_dialog(&win_for_result, &banner, prefill, true, store.clone(), refresh.clone(), split_view.clone(), Rc::new(None), rebuild_group_tabs.clone(), rebuild_grid.clone(), selected_group_filter.clone(), EditDialogTab::Card);
                    }
                    None => show_banner(
                        &banner,
                        BannerSeverity::Error,
                        "Couldn't find a barcode or QR code in that image.",
                    ),
                }
            });
        });
    }
    window.add_action(&scan_image_action);

    let scan_camera_action = gio::SimpleAction::new("scan-camera", None);
    scan_camera_action.set_enabled(camera::camera_available());
    {
        let store = store.clone();
        let refresh = refresh.clone();
        let split_view = split_view.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        let window_weak = window.downgrade();
        let banner = banner.clone();
        scan_camera_action.connect_activate(move |_, _| {
            let Some(win) = window_weak.upgrade() else { return };
            let store = store.clone();
            let refresh = refresh.clone();
            let split_view = split_view.clone();
            let rebuild_group_tabs = rebuild_group_tabs.clone();
            let rebuild_grid = rebuild_grid.clone();
            let selected_group_filter = selected_group_filter.clone();
            let win_for_detect = win.clone();
            let banner_for_detect = banner.clone();
            let banner_for_error = banner.clone();
            camera::open_scan_dialog(
                &win,
                move |value, kind| {
                    let mut prefill = Card::new("");
                    prefill.color = random_card_color(prefill.id);
                    prefill.code_value = value;
                    prefill.barcode_type = kind;
                    open_edit_dialog(&win_for_detect, &banner_for_detect, prefill, true, store.clone(), refresh.clone(), split_view.clone(), Rc::new(None), rebuild_group_tabs.clone(), rebuild_grid.clone(), selected_group_filter.clone(), EditDialogTab::Card);
                },
                move |err| show_banner(&banner_for_error, BannerSeverity::Error, &err),
            );
        });
    }
    window.add_action(&scan_camera_action);

    // --- Import / Export actions ------------------------------------
    let export_action = gio::SimpleAction::new("export", None);
    {
        let store = store.clone();
        let window_weak = window.downgrade();
        let banner = banner.clone();
        export_action.connect_activate(move |_, _| {
            let Some(win) = window_weak.upgrade() else { return };
            let dialog = gtk::FileDialog::builder()
                .title("Export Cards")
                .initial_name(
                    &glib::DateTime::now_local()
                        .map(|d| d.format("barkeep-%Y%m%d.csv").unwrap_or_else(|_| "barkeep.csv".into()).to_string())
                        .unwrap_or_else(|_| "barkeep.csv".to_string()),
                )
                .build();
            let store = store.clone();
            let banner = banner.clone();
            dialog.save(Some(&win), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        // Not report_if_save_failed: that helper's
                        // wording implies the user's card data is at
                        // risk. It isn't; export_to only writes to this
                        // separate CSV file, never touching cards.json.
                        if let Err(e) = store.borrow().export_to(&path) {
                            show_banner(&banner, BannerSeverity::Error, &format!("Export failed: {e}"));
                        }
                    }
                }
            });
        });
    }
    window.add_action(&export_action);

    let import_action = gio::SimpleAction::new("import", None);
    {
        let store = store.clone();
        let refresh = refresh.clone();
        let window_weak = window.downgrade();
        let banner = banner.clone();
        import_action.connect_activate(move |_, _| {
            let Some(win) = window_weak.upgrade() else { return };
            let dialog = gtk::FileDialog::builder().title("Import Cards").build();
            let store = store.clone();
            let refresh = refresh.clone();
            let win_for_result = win.clone();
            let banner = banner.clone();
            dialog.open(Some(&win), gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                // Auto-detects a Barkeep export, a Catima backup, or a
                // LoyaltyCardKeychain CSV; no separate menu item per
                // format.
                //
                // Computed into `result` as its own statement: a
                // RefMut in a match scrutinee lives until the match
                // ends, and the Ok arm below calls refresh(), which
                // calls store.borrow(), panicking against the still-live
                // mutable borrow otherwise.
                let result = store.borrow_mut().import_any(&path);
                match result {
                    Ok(ImportOutcome::Imported(summary)) => {
                        (refresh.borrow())();
                        let message = if summary.duplicates > 0 {
                            format!(
                                "{} card(s) imported, {} duplicate(s) skipped.",
                                summary.imported, summary.duplicates
                            )
                        } else {
                            format!("Imported {} card(s).", summary.imported)
                        };
                        show_banner(&banner, BannerSeverity::Success, &message);
                    }
                    Ok(ImportOutcome::NeedsFallbackConfirmation(cards)) => {
                        // The fallback parser found something, but
                        // nothing confident enough to import
                        // automatically. Suggested styling, not
                        // Destructive, since attempting the import
                        // isn't destructive. A modal dialog, not a
                        // banner, since this needs a real yes/no
                        // decision before proceeding.
                        let store = store.clone();
                        let refresh = refresh.clone();
                        let win_for_result = win_for_result.clone();
                        let banner = banner.clone();
                        confirm_action(
                            &win_for_result,
                            "Unrecognized Format",
                            "The file's format is not recognized. A best-effort attempt can be made but some cards may be incomplete or have misidentified fields.",
                            "Attempt",
                            adw::ResponseAppearance::Suggested,
                            move || {
                                let result = store.borrow_mut().append_imported(cards.clone());
                                match result {
                                    Ok(summary) => {
                                        (refresh.borrow())();
                                        let message = if summary.duplicates > 0 {
                                            format!(
                                                "{} card(s) imported, {} duplicate(s) skipped.",
                                                summary.imported, summary.duplicates
                                            )
                                        } else {
                                            format!("Imported {} card(s).", summary.imported)
                                        };
                                        show_banner(&banner, BannerSeverity::Success, &message);
                                    }
                                    Err(e) => show_banner(&banner, BannerSeverity::Error, &format!("Import failed: {e}")),
                                }
                            },
                        );
                    }
                    Err(e) => show_banner(&banner, BannerSeverity::Error, &format!("Import failed: {e}")),
                }
            });
        });
    }
    window.add_action(&import_action);

    let about_action = gio::SimpleAction::new("about", None);
    {
        let window_weak = window.downgrade();
        about_action.connect_activate(move |_, _| {
            let Some(win) = window_weak.upgrade() else { return };
            let about = adw::AboutDialog::builder()
                .application_name("Barkeep")
                .application_icon(crate::APP_ID)
                .version(env!("CARGO_PKG_VERSION"))
                .developer_name("AJ")
                .license_type(gtk::License::Gpl30)
                .website("https://dev.dunevent.net/aj/")
                .build();
            about.add_link("Donate", "https://www.paypal.me/ajdunevent");
            about.add_link("Fediverse", "https://social.dunevent.net/@aj");
            about.present(Some(&win));
        });
    }
    window.add_action(&about_action);

    // Stateful (boolean) action; gio::Menu renders this as a checkbox
    // automatically. show_archived is the actual source of truth; this
    // action's state is kept in sync with it, but nothing else reads
    // the action's state back out.
    //
    // This action's boolean means "hide archived cards", the inverse
    // of show_archived's Cell (true = shown), so only the mapping here
    // is inverted rather than renaming show_archived everywhere it's
    // threaded through.
    let show_archived_action =
        gio::SimpleAction::new_stateful("show-archived", None, &ui_state.borrow().hide_archived.to_variant());
    {
        let show_archived = show_archived.clone();
        let ui_state = ui_state.clone();
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        show_archived_action.connect_activate(move |action, _| {
            let currently_hidden = action
                .state()
                .and_then(|v| v.get::<bool>())
                .unwrap_or(true);
            let new_hidden = !currently_hidden;
            show_archived.set(!new_hidden);
            action.set_state(&new_hidden.to_variant());
            ui_state.borrow_mut().hide_archived = new_hidden;
            ui_state.borrow().save();
            (rebuild_grid.borrow())();
            (rebuild_group_tabs.borrow())();
        });
    }
    window.add_action(&show_archived_action);

    // Same pattern as show_archived_action just above.
    let show_valid_only_action =
        gio::SimpleAction::new_stateful("show-valid-only", None, &ui_state.borrow().hide_invalid.to_variant());
    {
        let show_valid_only = show_valid_only.clone();
        let ui_state = ui_state.clone();
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        show_valid_only_action.connect_activate(move |action, _| {
            let new_value = !show_valid_only.get();
            show_valid_only.set(new_value);
            action.set_state(&new_value.to_variant());
            ui_state.borrow_mut().hide_invalid = new_value;
            ui_state.borrow().save();
            (rebuild_grid.borrow())();
            (rebuild_group_tabs.borrow())();
        });
    }
    window.add_action(&show_valid_only_action);

    let manage_groups_action = gio::SimpleAction::new("manage-groups", None);
    {
        let store = store.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        let window_weak = window.downgrade();
        let banner = banner.clone();
        manage_groups_action.connect_activate(move |_, _| {
            let Some(win) = window_weak.upgrade() else { return };
            open_manage_groups_dialog(
                &win,
                &banner,
                store.clone(),
                rebuild_group_tabs.clone(),
                rebuild_grid.clone(),
                selected_group_filter.clone(),
                None,
            );
        });
    }
    window.add_action(&manage_groups_action);

    // Surfaced now that the window exists to present a notification
    // on. Warning, not Error severity: this reports something already
    // handled automatically (data corruption recovered from), not
    // something currently failing.
    if !startup_warnings.is_empty() {
        show_banner(&banner, BannerSeverity::Warning, &startup_warnings.join("\n\n"));
    }

    window
}

/// Add/rename/remove/reorder groups. Reorder is Up/Down buttons per
/// row, not drag-and-drop; index-swap-and-rebuild is simpler and lower
/// risk than wiring up GtkDragSource/GtkDropTarget correctly.
/// Which bulk state a group is currently in, relative to the visible-
/// selected cards open_change_groups_dialog was opened for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupBulkState {
    Full,
    Partial,
    Off,
}

/// The next state a click on a group's pill should move it to, given
/// its ORIGINAL state (computed once, when the dialog opened) and its
/// CURRENT state. A group that started Full or Off toggles between the
/// two. A group that started Partial cycles Partial -> Off -> Full ->
/// back to Partial.
fn next_group_bulk_state(original: GroupBulkState, current: GroupBulkState) -> GroupBulkState {
    match original {
        GroupBulkState::Partial => match current {
            GroupBulkState::Partial => GroupBulkState::Off,
            GroupBulkState::Off => GroupBulkState::Full,
            GroupBulkState::Full => GroupBulkState::Partial,
        },
        GroupBulkState::Full | GroupBulkState::Off => match current {
            GroupBulkState::Full => GroupBulkState::Off,
            GroupBulkState::Off => GroupBulkState::Full,
            // Should never actually reach Partial for a Full/Off
            // original; falls back to original defensively rather than
            // panicking if this invariant doesn't hold.
            GroupBulkState::Partial => original,
        },
    }
}

/// See the tri-state pill CSS built in open_change_groups_dialog.
fn group_bulk_css_class(state: GroupBulkState) -> &'static str {
    match state {
        GroupBulkState::Full => "pill-full",
        GroupBulkState::Partial => "pill-partial",
        GroupBulkState::Off => "pill-off",
    }
}

/// Off if no given cards are members, Full if all are, Partial
/// otherwise.
fn compute_group_bulk_state(group_id: Uuid, cards: &[Card]) -> GroupBulkState {
    let member_count = cards.iter().filter(|c| c.group_ids.contains(&group_id)).count();
    if member_count == 0 {
        GroupBulkState::Off
    } else if member_count == cards.len() {
        GroupBulkState::Full
    } else {
        GroupBulkState::Partial
    }
}

/// The "Change Groups" bulk-editing dialog, opened from the
/// multiselect action bar's Groups button. Shows every group as a
/// tri-state pill (Full/Partial/Off) reflecting current membership,
/// lets each target state change independently, and on Apply changes
/// only groups whose final state differs from "leave alone": Full
/// adds the group to every card missing it, Off removes it from every
/// card that has it, Partial leaves membership completely untouched.
fn open_change_groups_dialog(
    window: &adw::ApplicationWindow,
    banner: &SharedBanner,
    store: Rc<RefCell<CardStore>>,
    visible_selected_ids: Vec<Uuid>,
    rebuild_group_tabs: Rc<RefCell<dyn Fn()>>,
    rebuild_grid: Rc<RefCell<dyn Fn()>>,
    selected_group_filter: Rc<RefCell<Option<Uuid>>>,
    // Carries in-progress tri-state choices across a detour through
    // Manage Groups: None for a fresh open, Some for a reopen, where
    // whatever wasn't touched should look untouched.
    preserved_states: Option<HashMap<Uuid, (GroupBulkState, GroupBulkState)>>,
    // Box<dyn Fn()>, not impl Fn() + 'static: this function calls
    // itself (see the "Manage Groups" link below), so a generic
    // parameter would force a new monomorphization on each self-call,
    // and that instantiation's own call to itself never stops growing.
    // Same reasoning as on_saved/on_closed elsewhere in this file.
    on_applied: Box<dyn Fn()>,
) {
    let cards: Vec<Card> = store
        .borrow()
        .cards
        .iter()
        .filter(|c| visible_selected_ids.contains(&c.id))
        .cloned()
        .collect();

    // (original, current) per group; original never changes once
    // computed, current is mutated by each pill's click handler. Built
    // by iterating the current group list, not preserved_states
    // directly, so a group deleted via Manage Groups since capture
    // never appears and a newly-added one gets a fresh entry.
    let group_states: Rc<RefCell<HashMap<Uuid, (GroupBulkState, GroupBulkState)>>> = Rc::new(RefCell::new(
        store
            .borrow()
            .groups
            .iter()
            .map(|g| {
                if let Some(preserved) = preserved_states.as_ref().and_then(|p| p.get(&g.id)) {
                    (g.id, *preserved)
                } else {
                    let initial = compute_group_bulk_state(g.id, &cards);
                    (g.id, (initial, initial))
                }
            })
            .collect(),
    ));

    // Same .compact-pill base as the Add/Edit screen's group picker.
    // No background-color override for the unchecked state; GTK's
    // native .pill treatment (border/shadow) handles contrast against
    // the dialog background on its own. pill-full/pill-partial
    // override the selected state to the accent color. Reached via an
    // explicit class rather than GTK's checked pseudo-class, since a
    // tri-state cycle doesn't map onto ToggleButton's binary
    // checked/unchecked. pill-partial uses alpha(var(--accent-bg-color),
    // 0.5), GTK4's native CSS function for partial opacity of a theme
    // variable.
    register_css(
        ".compact-pill { \
             padding: 2px 10px; \
             min-height: 0; \
         } \
         .compact-pill.pill-full { \
             background-color: var(--accent-bg-color); \
             color: var(--accent-fg-color); \
         } \
         .compact-pill.pill-partial { \
             background-color: alpha(var(--accent-bg-color), 0.5); \
         }",
    );

    let groups_flow_box = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .row_spacing(6)
        .column_spacing(6)
        .homogeneous(false)
        .margin_start(12)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(12)
        .build();

    // A plain Button, not a ToggleButton: this is a tri-state cycle,
    // so visual state is managed via CSS class add/remove on every
    // click rather than GTK's checked pseudo-class.
    for group in &store.borrow().groups {
        let (_, current) = group_states.borrow()[&group.id];
        let pill = gtk::Button::builder()
            .label(&group.name)
            .css_classes(["pill", "compact-pill", group_bulk_css_class(current)])
            .build();
        {
            let group_states = group_states.clone();
            let group_id = group.id;
            let pill_for_handler = pill.clone();
            pill.connect_clicked(move |_| {
                let next = {
                    let mut states = group_states.borrow_mut();
                    let (original, current) = states[&group_id];
                    let next = next_group_bulk_state(original, current);
                    states.insert(group_id, (original, next));
                    (current, next)
                };
                let (previous, next) = next;
                pill_for_handler.remove_css_class(group_bulk_css_class(previous));
                pill_for_handler.add_css_class(group_bulk_css_class(next));
            });
        }
        groups_flow_box.insert(&pill, -1);
    }

    let no_groups_label = gtk::Label::builder()
        .label("No groups yet")
        .halign(gtk::Align::Start)
        .css_classes(["dim-label"])
        .margin_start(12)
        .margin_top(12)
        .visible(store.borrow().groups.is_empty())
        .build();

    // Opens Manage Groups (create/rename/delete groups, a different
    // action from this dialog's "change which groups these cards
    // belong to") with the current tri-state choices preserved across
    // the trip, via preserved_states above.
    let manage_groups_link = gtk::Button::builder()
        .label("Manage Groups")
        .halign(gtk::Align::Center)
        .margin_top(26)
        .build();

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.append(&no_groups_label);
    content_box.append(&groups_flow_box);
    content_box.append(&manage_groups_link);

    let apply_button = gtk::Button::builder()
        .label("Apply")
        .css_classes(["suggested-action"])
        .build();
    let bottom_bar = gtk::ActionBar::new();
    bottom_bar.pack_end(&apply_button);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Change Groups", "")));
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content_box));
    toolbar.add_bottom_bar(&bottom_bar);

    let dialog = adw::Dialog::builder()
        .title("Change Groups")
        .content_width(360)
        .content_height(480)
        .child(&toolbar)
        .build();

    {
        let store = store.clone();
        let dialog = dialog.clone();
        let group_states = group_states.clone();
        let visible_selected_ids = visible_selected_ids.clone();
        let banner = banner.clone();
        apply_button.connect_clicked(move |_| {
            // Only groups whose final state isn't Partial become a
            // change; a Partial group is left out entirely, which is
            // what makes apply_group_changes_bulk leave it untouched.
            let changes: Vec<(Uuid, bool)> = group_states
                .borrow()
                .iter()
                .filter_map(|(&group_id, &(_, final_state))| match final_state {
                    GroupBulkState::Full => Some((group_id, true)),
                    GroupBulkState::Off => Some((group_id, false)),
                    GroupBulkState::Partial => None,
                })
                .collect();
            report_if_save_failed(&banner, store.borrow_mut().apply_group_changes_bulk(&visible_selected_ids, &changes));
            dialog.close();
            on_applied();
        });
    }

    {
        let window = window.clone();
        let banner = banner.clone();
        let store = store.clone();
        let dialog = dialog.clone();
        let group_states = group_states.clone();
        let visible_selected_ids = visible_selected_ids.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        manage_groups_link.connect_clicked(move |_| {
            let snapshot = group_states.borrow().clone();
            dialog.close();

            let window_for_reopen = window.clone();
            let banner_for_reopen = banner.clone();
            let store_for_reopen = store.clone();
            let visible_selected_ids_for_reopen = visible_selected_ids.clone();
            let rebuild_group_tabs_for_reopen = rebuild_group_tabs.clone();
            let rebuild_grid_for_reopen = rebuild_grid.clone();
            let selected_group_filter_for_reopen = selected_group_filter.clone();
            let rebuild_grid_for_applied = rebuild_grid.clone();
            let rebuild_group_tabs_for_applied = rebuild_group_tabs.clone();
            open_manage_groups_dialog(
                &window,
                &banner,
                store.clone(),
                rebuild_group_tabs.clone(),
                rebuild_grid.clone(),
                selected_group_filter.clone(),
                Some(Box::new(move || {
                    open_change_groups_dialog(
                        &window_for_reopen,
                        &banner_for_reopen,
                        store_for_reopen.clone(),
                        visible_selected_ids_for_reopen.clone(),
                        rebuild_group_tabs_for_reopen.clone(),
                        rebuild_grid_for_reopen.clone(),
                        selected_group_filter_for_reopen.clone(),
                        Some(snapshot.clone()),
                        Box::new({
                            let rebuild_grid = rebuild_grid_for_applied.clone();
                            let rebuild_group_tabs = rebuild_group_tabs_for_applied.clone();
                            move || {
                                (rebuild_grid.borrow())();
                                (rebuild_group_tabs.borrow())();
                            }
                        }),
                    );
                })),
            );
        });
    }

    // GTK's default focus (the first group pill) was reading as "this
    // group is selected", which it wasn't.
    apply_button.grab_focus();

    dialog.present(Some(window));
}

fn open_manage_groups_dialog(
    window: &adw::ApplicationWindow,
    banner: &SharedBanner,
    store: Rc<RefCell<CardStore>>,
    rebuild_group_tabs: Rc<RefCell<dyn Fn()>>,
    rebuild_grid: Rc<RefCell<dyn Fn()>>,
    selected_group_filter: Rc<RefCell<Option<Uuid>>>,
    // Fired once this dialog closes, regardless of how. None for call
    // sites with nothing to return to (the main menu's entry point).
    on_closed: Option<Box<dyn Fn()>>,
) {
    let list_box = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();

    // Box<dyn Fn()>, not this app's usual bare `dyn Fn()`, since this
    // needs to be reassigned after construction; a bare `dyn Fn()`
    // behind a RefCell can be called but not swapped for a different
    // closure later. Placeholder no-op built first, then replaced with
    // the real closure once it exists; the closures captured into each
    // row's buttons hold a clone of the handle, so they see whatever
    // the RefCell's content is at call time, not when they were created.
    let rebuild_rows: Rc<RefCell<Box<dyn Fn()>>> = Rc::new(RefCell::new(Box::new(|| {})));
    {
        let rebuild_rows_handle = rebuild_rows.clone();
        let store = store.clone();
        let list_box = list_box.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        // Captured here, at the outer level, so the inner
        // `let banner = banner.clone();` lines deep in this closure's
        // body clone from this owned value, not the borrowed
        // &adw::Banner function parameter. Box<dyn Fn()> has an
        // implicit 'static bound, and this closure was transitively
        // capturing that borrowed parameter through the nested clones,
        // which doesn't satisfy it ("lifetime may not live long
        // enough... must outlive 'static").
        let banner = banner.clone();
        let real_closure: Box<dyn Fn()> = Box::new(move || {
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }
            let groups = store.borrow().groups.clone();
            let count = groups.len();
            for (index, group) in groups.iter().enumerate() {
                let row = adw::ActionRow::builder().title(&group.name).build();

                let up_button = gtk::Button::from_icon_name("go-up-symbolic");
                up_button.add_css_class("flat");
                up_button.set_valign(gtk::Align::Center);
                up_button.set_sensitive(index > 0);
                {
                    let store = store.clone();
                    let rebuild_rows_handle = rebuild_rows_handle.clone();
                    let rebuild_group_tabs = rebuild_group_tabs.clone();
                    let group_id = group.id;
                    let banner = banner.clone();
                    up_button.connect_clicked(move |_| {
                        let order: Vec<Uuid> = {
                            let mut store_ref = store.borrow_mut();
                            if let Some(pos) = store_ref.groups.iter().position(|g| g.id == group_id) {
                                if pos > 0 {
                                    store_ref.groups.swap(pos, pos - 1);
                                }
                            }
                            store_ref.groups.iter().map(|g| g.id).collect()
                        };
                        report_if_save_failed(&banner, store.borrow_mut().set_group_order(&order));
                        (rebuild_rows_handle.borrow())();
                        (rebuild_group_tabs.borrow())();
                    });
                }

                let down_button = gtk::Button::from_icon_name("go-down-symbolic");
                down_button.add_css_class("flat");
                down_button.set_valign(gtk::Align::Center);
                down_button.set_sensitive(index + 1 < count);
                {
                    let store = store.clone();
                    let rebuild_rows_handle = rebuild_rows_handle.clone();
                    let rebuild_group_tabs = rebuild_group_tabs.clone();
                    let group_id = group.id;
                    let banner = banner.clone();
                    down_button.connect_clicked(move |_| {
                        let order: Vec<Uuid> = {
                            let mut store_ref = store.borrow_mut();
                            if let Some(pos) = store_ref.groups.iter().position(|g| g.id == group_id) {
                                if pos + 1 < store_ref.groups.len() {
                                    store_ref.groups.swap(pos, pos + 1);
                                }
                            }
                            store_ref.groups.iter().map(|g| g.id).collect()
                        };
                        report_if_save_failed(&banner, store.borrow_mut().set_group_order(&order));
                        (rebuild_rows_handle.borrow())();
                        (rebuild_group_tabs.borrow())();
                    });
                }

                let rename_button = gtk::Button::from_icon_name("document-edit-symbolic");
                rename_button.add_css_class("flat");
                rename_button.set_valign(gtk::Align::Center);
                {
                    let store = store.clone();
                    let rebuild_rows_handle = rebuild_rows_handle.clone();
                    let rebuild_group_tabs = rebuild_group_tabs.clone();
                    let group_id = group.id;
                    let current_name = group.name.clone();
                    let banner = banner.clone();
                    rename_button.connect_clicked(move |btn| {
                        let entry = gtk::Entry::builder().text(&current_name).build();
                        let save_button = gtk::Button::with_label("Save");
                        save_button.add_css_class("suggested-action");
                        let popover_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                        popover_box.set_margin_top(6);
                        popover_box.set_margin_bottom(6);
                        popover_box.set_margin_start(6);
                        popover_box.set_margin_end(6);
                        popover_box.append(&entry);
                        popover_box.append(&save_button);
                        let popover = gtk::Popover::new();
                        popover.set_child(Some(&popover_box));
                        popover.set_parent(btn);
                        {
                            let store = store.clone();
                            let rebuild_rows_handle = rebuild_rows_handle.clone();
                            let rebuild_group_tabs = rebuild_group_tabs.clone();
                            let entry = entry.clone();
                            let popover = popover.clone();
                            let banner = banner.clone();
                            save_button.connect_clicked(move |_| {
                                let new_name = entry.text().to_string();
                                if !new_name.trim().is_empty() {
                                    report_if_save_failed(&banner, store.borrow_mut().rename_group(group_id, new_name));
                                    (rebuild_rows_handle.borrow())();
                                    (rebuild_group_tabs.borrow())();
                                }
                                popover.popdown();
                            });
                        }
                        popover.popup();
                    });
                }

                let remove_button = gtk::Button::from_icon_name("user-trash-symbolic");
                remove_button.add_css_class("flat");
                remove_button.set_valign(gtk::Align::Center);
                {
                    let store = store.clone();
                    let rebuild_rows_handle = rebuild_rows_handle.clone();
                    let rebuild_group_tabs = rebuild_group_tabs.clone();
                    let rebuild_grid = rebuild_grid.clone();
                    let selected_group_filter = selected_group_filter.clone();
                    let group_id = group.id;
                    let banner = banner.clone();
                    remove_button.connect_clicked(move |_| {
                        report_if_save_failed(&banner, store.borrow_mut().remove_group(group_id));
                        // Falls back to "All" if the deleted group was
                        // the one narrowing the main grid.
                        if *selected_group_filter.borrow() == Some(group_id) {
                            *selected_group_filter.borrow_mut() = None;
                        }
                        (rebuild_rows_handle.borrow())();
                        (rebuild_group_tabs.borrow())();
                        (rebuild_grid.borrow())();
                    });
                }

                row.add_suffix(&up_button);
                row.add_suffix(&down_button);
                row.add_suffix(&rename_button);
                row.add_suffix(&remove_button);
                list_box.append(&row);
            }
        });
        *rebuild_rows.borrow_mut() = real_closure;
    }
    (rebuild_rows.borrow())();

    let add_row = adw::EntryRow::builder().title("New group name").build();
    let add_button = gtk::Button::from_icon_name("list-add-symbolic");
    add_button.add_css_class("flat");
    add_button.set_valign(gtk::Align::Center);
    add_row.add_suffix(&add_button);
    {
        let store = store.clone();
        let rebuild_rows = rebuild_rows.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let add_row_for_clear = add_row.clone();
        let banner = banner.clone();
        add_button.connect_clicked(move |_| {
            let name = add_row_for_clear.text().to_string();
            if !name.trim().is_empty() {
                report_if_save_failed(&banner, store.borrow_mut().add_group(name));
                add_row_for_clear.set_text("");
                (rebuild_rows.borrow())();
                (rebuild_group_tabs.borrow())();
            }
        });
    }
    let add_list_box = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    add_list_box.append(&add_row);

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content_box.set_margin_top(12);
    content_box.set_margin_bottom(12);
    content_box.set_margin_start(12);
    content_box.set_margin_end(12);
    content_box.append(&add_list_box);
    content_box.append(&list_box);

    let scroller = gtk::ScrolledWindow::builder()
        .child(&content_box)
        .vexpand(true)
        .propagate_natural_height(true)
        .build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Manage Groups", "")));
    // Computed before on_closed gets moved into dialog.connect_closed;
    // once moved, it can't be inspected again out here.
    let opened_from_edit_dialog = on_closed.is_some();
    // A back chevron instead of the default close button when opened
    // from the Add/Edit screen, since "back to the card I was editing"
    // reads as backward navigation rather than a plain close. Click
    // handler attached later, once dialog exists (same pattern
    // save_button/manage_groups_button use in open_edit_dialog).
    let back_button = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Back")
        .build();
    if opened_from_edit_dialog {
        header.set_show_end_title_buttons(false);
        header.pack_start(&back_button);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title("Manage Groups")
        .content_width(420)
        .content_height(600)
        .child(&toolbar)
        .build();
    if opened_from_edit_dialog {
        let dialog = dialog.clone();
        back_button.connect_clicked(move |_| {
            dialog.close();
        });
    }
    dialog.connect_closed(move |_| {
        if let Some(on_closed) = &on_closed {
            on_closed();
        }
    });
    dialog.present(Some(window));
}

/// Two AdwBanner instances, kept in sync by show_banner: one in
/// sidebar_toolbar, one in content_toolbar, each as a second top bar
/// below that page's AdwHeaderBar. Necessary since a single widget can
/// only have one parent, and only one of the split view's two pages is
/// ever visible at a time. A banner placed above the whole split view
/// instead sat above both pages' headerbars, not just below whichever
/// was showing.
#[derive(Clone)]
struct SharedBanner {
    sidebar: adw::Banner,
    content: adw::Banner,
    // Loaded fresh each time show_banner runs, targeting
    // #barkeep-banner-sidebar/#barkeep-banner-content by ID.
    css_provider: gtk::CssProvider,
}

/// Which of the three CSS classes registered in
/// build_split_view_and_window colors the banner (libadwaita's
/// success/warning/error bg/fg color pairs).
enum BannerSeverity {
    Error,
    Success,
    Warning,
}

/// The one shared notification surface for the whole app, replacing
/// every notification previously shown as a modal AdwAlertDialog. A
/// modal dialog blocks the rest of the app until dismissed, a mismatch
/// for anything not directly triggered by the user's own tap (most
/// notably the midnight expiration check, firing from a background
/// timer). No separate "Retry" action on any banner: a failed save
/// means the mutation never applied, so re-doing the same UI action is
/// already the retry (see mutate_then_save in card.rs). Updates both
/// of SharedBanner's instances identically. Each button is wired once,
/// at construction, to just dismiss that instance; every message uses
/// the same acknowledge-and-dismiss action.
fn show_banner(banner: &SharedBanner, severity: BannerSeverity, message: &str) {
    // ID selectors, reloaded fresh each call via SharedBanner's
    // css_provider, the same pattern update_header_style uses for the
    // detail header.
    let (bg_var, fg_var) = match severity {
        BannerSeverity::Error => ("--error-bg-color", "--error-fg-color"),
        BannerSeverity::Success => ("--success-bg-color", "--success-fg-color"),
        BannerSeverity::Warning => ("--warning-bg-color", "--warning-fg-color"),
    };
    banner.css_provider.load_from_string(&format!(
        "#barkeep-banner-sidebar, #barkeep-banner-content {{ \
             background-color: var({bg_var}); \
             color: var({fg_var}); \
         }}"
    ));
    for b in [&banner.sidebar, &banner.content] {
        b.set_title(message);
        b.set_revealed(true);
    }
}

/// Surfaces a save failure instead of silently discarding it (every
/// CardStore mutation site used to just `.ok()` it), which would leave
/// the in-memory state changed, the UI showing success, and the change
/// silently lost on restart. Generic over the error type (bounded by
/// Display, not fixed to String) since most CardStore mutation methods
/// return io::Error while the import-related ones already convert to
/// String.
fn report_if_save_failed<T, E: std::fmt::Display>(banner: &SharedBanner, result: Result<T, E>) {
    if let Err(e) = result {
        show_banner(banner, BannerSeverity::Error, &format!("Your change may not have been saved: {e}"));
    }
}

/// Disconnects whichever signal handler is stored in `handler`, if
/// any, and clears the slot. Extracted from 11 identically-shaped
/// sites across refresh and show_card_detail. Only the disconnect half
/// is unified; the reconnect half differs per button (toggled vs
/// clicked vs activate) with no shared signature. Generic over
/// W: IsA<glib::Object> since gtk::ToggleButton, gtk::Button, and
/// gio::SimpleAction all need to work here and share no more specific
/// trait.
fn disconnect_if_present<W: glib::object::IsA<glib::Object>>(
    widget: &W,
    handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
) {
    if let Some(id) = handler.borrow_mut().take() {
        widget.disconnect(id);
    }
}

/// Creates a CssProvider, loads `css` into it, and registers it
/// against the default display. Not used for name_label_css
/// (registered empty, then loaded repeatedly as the card changes) or
/// build_card_tile's per-card provider cache, both of which have
/// different lifecycle needs than "register once, forget about it".
fn register_css(css: &str) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    // PRIORITY_USER, not PRIORITY_APPLICATION: CSS confirmed correct
    // in GTK Inspector's live editor wasn't taking effect once
    // compiled into a PRIORITY_APPLICATION-level provider. A GNOME
    // Discourse thread describes the same problem with libadwaita/GTK
    // overriding properties at that level, and the docs confirm
    // PRIORITY_USER is the sanctioned highest priority an application
    // should use.
    gtk::style_context_add_provider_for_display(
        &default_display(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );
}

/// Shared by every "confirm before doing something, then do it"
/// dialog: bulk delete, single-card delete, and the
/// unrecognized-import-format prompt. The response id is always
/// "confirm" internally, never shown to the user (only action_label
/// is).
fn confirm_action(
    window: &adw::ApplicationWindow,
    heading: &str,
    body: &str,
    action_label: &str,
    appearance: adw::ResponseAppearance,
    on_confirm: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", action_label);
    dialog.set_response_appearance("confirm", appearance);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "confirm" {
            on_confirm();
        }
    });
    dialog.present(Some(window));
}

/// Thin wrapper over confirm_action with Delete/Destructive fixed.
/// Takes a caller-built heading/body rather than just a card name,
/// since bulk delete needs a different message than single-card delete.
fn confirm_delete(window: &adw::ApplicationWindow, heading: &str, body: &str, on_confirm: impl Fn() + 'static) {
    confirm_action(window, heading, body, "Delete", adw::ResponseAppearance::Destructive, on_confirm);
}

/// Colors the whole detail-view header to match the selected card, via
/// the same id-selector CSS approach used for the old card-name label.
/// Also colors the detail view's body (#barkeep-card-body, set fresh
/// each time show_card_detail runs) with a top-to-bottom gradient from
/// the card color into the theme's neutral background, reusing this
/// same provider rather than threading a second one through.
///
/// :backdrop (window loses focus): two earlier attempts didn't pan
/// out. Canceling GTK's header dimming outright didn't work. Computing
/// a matching dimmed color via a flat "reduce every channel by the
/// same percentage" formula fit light mode against 4 real before/after
/// samples (blend toward white by ~48%, under 5% error), but no
/// blend-toward-a-fixed-color formula fit dark mode, with real
/// outliers no single number explains. Dropped the gradient under
/// :backdrop instead: a flat var(--window-bg-color), no color-matching
/// attempted.
fn update_header_style(provider: &gtk::CssProvider, bg: &str, fg: &str) {
    provider.load_from_string(&format!(
        "#barkeep-content-toolbar {{ \
           --headerbar-bg-color: {bg}; \
           --headerbar-backdrop-color: {bg}; \
           --headerbar-darker-shade-color: transparent; \
         }} \
         #barkeep-detail-header {{ \
           background-color: {bg}; \
           color: {fg}; \
         }} \
         #barkeep-detail-header:backdrop {{ \
           background-color: {bg}; \
           color: {fg}; \
         }} \
         #barkeep-detail-header windowtitle {{ \
           color: {fg}; \
           font-weight: 800; \
         }} \
         #barkeep-detail-header windowtitle:backdrop {{ \
           color: {fg}; \
           font-weight: 800; \
         }} \
         #barkeep-card-body {{ \
           background: linear-gradient(to bottom, {bg} 0%, var(--window-bg-color) 50%); \
           padding: 24px; \
         }}"
    ));
}

/// Parses a "YYYY-MM-DD" string into a glib::DateTime at local
/// midnight, for handing to gtk::Calendar::select_day. None on
/// anything malformed; callers leave the calendar on its default
/// selection.
fn parse_iso_date(s: &str) -> Option<glib::DateTime> {
    let mut parts = s.trim().splitn(3, '-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: i32 = parts.next()?.parse().ok()?;
    let day: i32 = parts.next()?.parse().ok()?;
    glib::DateTime::new(&glib::TimeZone::local(), year, month, day, 0, 0, 0.0).ok()
}

/// Today's date as "YYYY-MM-DD" in local time, matching the format
/// Card's valid_from/expiry fields use. Empty string on failure;
/// CardStore::apply_expiration_actions treats an empty `today` as
/// "don't act on anything".
fn today_iso() -> String {
    glib::DateTime::now_local()
        .and_then(|d| d.format("%Y-%m-%d"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The display every CSS provider in this file gets registered
/// against, previously duplicated six separate times.
fn default_display() -> gtk::gdk::Display {
    gtk::gdk::Display::default().expect("no display")
}

/// Re-derives the currently visible+selected card ids at the moment a
/// bulk action fires, rather than acting on the full selection: only
/// what's visible under the current filters when the action fires
/// gets acted on. Same query inputs rebuild_grid uses. Shared by every
/// bulk-action handler, previously duplicated verbatim four times.
fn visible_selected_card_ids(
    store: &CardStore,
    search_query: &str,
    show_archived: bool,
    show_valid_only: bool,
    selected_group_filter: Option<Uuid>,
    selected_card_ids: &HashSet<Uuid>,
) -> Vec<Uuid> {
    store
        .search(search_query, show_archived, show_valid_only, &today_iso(), selected_group_filter)
        .into_iter()
        .filter(|card| selected_card_ids.contains(&card.id))
        .map(|card| card.id)
        .collect()
}

/// Duration from right now until the next local midnight. Constructs
/// today's midnight explicitly (year/month/day at 00:00:00) rather
/// than "round up to the next day", so this is correct even if called
/// at midnight exactly. Falls back to a flat 24 hours on failure.
///
/// new_local() was tried first and doesn't exist; fixed to new() with
/// an explicit TimeZone::local(), per the compiler's own error.
fn duration_until_next_local_midnight() -> std::time::Duration {
    let fallback = std::time::Duration::from_secs(24 * 60 * 60);
    (|| -> Option<std::time::Duration> {
        let now = glib::DateTime::now_local().ok()?;
        let today_midnight =
            glib::DateTime::new(
                &glib::TimeZone::local(),
                now.year(),
                now.month(),
                now.day_of_month(),
                0,
                0,
                0.0,
            )
                .ok()?;
        let next_midnight = today_midnight.add_days(1).ok()?;
        let secs = next_midnight.to_unix() - now.to_unix();
        (secs > 0).then(|| std::time::Duration::from_secs(secs as u64))
    })()
    .unwrap_or(fallback)
}

/// Re-checks expired cards at every local midnight while the app stays
/// open, rescheduling itself for the following midnight each time,
/// rather than a fixed-interval timer that would drift out of
/// alignment with the day boundary over time.
fn schedule_midnight_expiration_check(
    store: Rc<RefCell<CardStore>>,
    rebuild_grid: Rc<RefCell<dyn Fn()>>,
    banner: SharedBanner,
) {
    glib::source::timeout_add_local_once(duration_until_next_local_midnight(), move || {
        match store.borrow_mut().apply_expiration_actions(&today_iso()) {
            Ok(changed) => {
                if changed {
                    (rebuild_grid.borrow())();
                }
            }
            Err(e) => show_banner(&banner, BannerSeverity::Error, &format!("Couldn't update expired cards: {e}")),
        }
        schedule_midnight_expiration_check(store, rebuild_grid, banner);
    });
}

/// Wraps any http(s) URLs found in `text` in Pango markup <a> tags, so
/// GtkLabel renders them as actual clickable links. Escapes everything
/// else via markup_escape_text, since notes text going into markup
/// could otherwise contain characters like & or < that break it.
fn linkify_urls(text: &str) -> String {
    let mut result = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find("http://").or_else(|| rest.find("https://")) else {
            result.push_str(&glib::markup_escape_text(rest));
            break;
        };
        result.push_str(&glib::markup_escape_text(&rest[..start]));
        let url_part = &rest[start..];
        let end = url_part
            .find(|c: char| c.is_whitespace())
            .unwrap_or(url_part.len());
        let mut url = &url_part[..end];
        // Trims common trailing punctuation likely to be sentence
        // punctuation rather than actually part of the URL, e.g. a URL
        // at the end of a sentence followed by a period.
        while let Some(last) = url.chars().last() {
            if matches!(last, '.' | ',' | ')' | '!' | '?' | ';' | ':') {
                url = &url[..url.len() - last.len_utf8()];
            } else {
                break;
            }
        }
        let escaped_url = glib::markup_escape_text(url);
        result.push_str(&format!("<a href=\"{escaped_url}\">{escaped_url}</a>"));
        rest = &url_part[url.len()..];
    }
    result
}

/// A color for a brand-new card, picked from a small curated palette
/// rather than always the same flat default. Seeded from the card's
/// already-random Uuid rather than pulling in a `rand` crate.
fn random_card_color(seed: Uuid) -> String {
    const PALETTE: &[&str] = &[
        "#3584e4", // blue
        "#2ec27e", // green
        "#f6d32d", // yellow
        "#ff7800", // orange
        "#e01b24", // red
        "#9141ac", // purple
        "#e83f6f", // pink
        "#0e8a83", // teal
        "#986a44", // brown
    ];
    let index = (seed.as_u128() % PALETTE.len() as u128) as usize;
    PALETTE[index].to_string()
}

/// Converts a "#rrggbb" hex color to a CSS rgba(...) string at the
/// given alpha, used to soften a color's visual intensity without
/// changing which contrasting_text_color option it is. A symbolic
/// icon's solid-filled shape reads more starkly at pure black/white
/// than the same color on thin text glyphs. Named distinctly from
/// hex_to_rgba elsewhere in this file, which returns a gdk::RGBA
/// struct for widget APIs; this produces a CSS string instead.
fn hex_to_css_rgba(hex: &str, alpha: f32) -> String {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return format!("rgba(0, 0, 0, {alpha})");
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    format!("rgba({r}, {g}, {b}, {alpha})")
}

/// Picks black or white text so it stays readable against `hex`, using
/// perceived luminance.
fn contrasting_text_color(hex: &str) -> &'static str {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return "#000000";
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f32;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f32;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f32;
    let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
    if luminance > 150.0 {
        "#000000"
    } else {
        "#ffffff"
    }
}

/// Picks a point size for a tile's name label based on the name's
/// length, so a short name like "Harps" can render big while a long
/// one like "Bentonville Public Library" backs off enough to read
/// cleanly wrapped. A character-count heuristic, not a real text-width
/// measurement, since there's no realized Pango layout to measure
/// against before the label is added to a shown window. Tuned against
/// CARD_TILE_WIDTH; retune if that constant changes materially.
fn tile_font_size(name: &str) -> i32 {
    match name.chars().count() {
        0..=6 => 22,
        7..=10 => 18,
        11..=16 => 15,
        17..=24 => 13,
        _ => 11,
    }
}

/// Toggles one card's membership in the multiselect selection set, the
/// single place this logic lives, called identically by both the
/// right-click and long-press gesture handlers in rebuild_grid.
fn toggle_card_selection(selected_card_ids: &Rc<RefCell<HashSet<Uuid>>>, card_id: Uuid) {
    let mut ids = selected_card_ids.borrow_mut();
    if !ids.remove(&card_id) {
        ids.insert(card_id);
    }
}

/// Which selection badge a tile should show, if any. Distinct from a
/// bare bool since "not selected" needs two states: no badge when
/// multiselect isn't active, versus an explicit empty checkbox when it
/// is, so the mode stays visible even when every visible tile happens
/// to be unselected.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TileSelectionBadge {
    None,
    Checked,
    Unchecked,
}

/// Builds one grid tile for the sidebar's card grid: a small, rounded,
/// colored tile rather than a flat list row, similar in spirit to
/// Catima's grid view. Every tile is CARD_TILE_WIDTH x
/// CARD_TILE_HEIGHT; FlowBox's default reflow handles column count as
/// the window resizes.
fn build_card_tile(
    card: &Card,
    selection_badge: TileSelectionBadge,
    tile_css_providers: &Rc<RefCell<HashMap<Uuid, gtk::CssProvider>>>,
) -> gtk::Widget {
    // Pango markup sets the text color directly on the label's
    // rendering, so it can't lose a CSS specificity fight with the
    // theme the way a `label { color: ... }` rule could.
    let text_color = contrasting_text_color(&card.color);
    let label = gtk::Label::new(None);
    label.set_markup(&format!(
        "<span foreground=\"{}\" weight=\"bold\" size=\"{}\">{}</span>",
        text_color,
        tile_font_size(&card.name) * 1024,
        glib::markup_escape_text(&card.name)
    ));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    // A wrapping label's natural width still targets fitting the text
    // on one line; without a cap, a long name's uncapped natural width
    // could make FlowBox size every cell wider than CARD_TILE_WIDTH.
    // max_width_chars(18) caps that.
    //
    // Not smaller: WordChar wrap falls back to character-by-character
    // hyphenation when there's no room for even one whole word. 18 was
    // checked against tile_font_size's buckets: the riskiest is
    // "11..=16 chars => 15pt", and a 16-char name at 15pt bold comes
    // to roughly 130-140px, clearing CARD_TILE_WIDTH minus this
    // label's 20px margins with room to spare.
    label.set_max_width_chars(18);
    label.set_justify(gtk::Justification::Center);
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    // Without vexpand, the label only claims its natural height and
    // sits flush at the top; valign(Center) needs the extra space
    // vexpand provides to actually center into.
    label.set_vexpand(true);
    label.set_lines(3);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_margin_start(10);
    label.set_margin_end(10);

    let card_face = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card_face.append(&label);

    // Unique widget name (-> CSS ID selector) per card: specific
    // enough to beat FlowBoxChild's theme rules without `!important`.
    let widget_id = format!("barkeep-tile-{}", card.id.simple());
    card_face.set_widget_name(&widget_id);
    // A symbolic icon's color comes from real CSS (the `color`
    // property), not the Pango markup foreground attribute the
    // star/checkmark labels used as text glyphs. Included
    // unconditionally even when the badge itself may not exist this
    // rebuild, since a CSS rule targeting a nonexistent widget ID is
    // harmless.
    let badge_widget_id = format!("barkeep-badge-{}", card.id.simple());
    let archived_badge_widget_id = format!("barkeep-archived-badge-{}", card.id.simple());
    let star_widget_id = format!("barkeep-star-{}", card.id.simple());
    // Softened rather than full-intensity text_color, to calm down how
    // stark a solid-filled symbolic icon reads at pure black/white
    // compared to the same color on thin text. Shared across all three
    // icon badges. 0.7 is a starting point, not precisely tuned.
    let softened_badge_color = hex_to_css_rgba(text_color, 0.7);
    let css_text = format!(
        "box#{id} {{ \
           background-color: {bg}; \
           border-radius: 14px; \
           box-shadow: 0 2px 5px alpha(black, 0.35); \
         }} \
         image#{badge_id} {{ \
           color: {softened_badge_color}; \
         }} \
         image#{archived_badge_id} {{ \
           color: {softened_badge_color}; \
         }} \
         image#{star_id} {{ \
           color: {softened_badge_color}; \
         }}",
        id = widget_id,
        bg = card.color,
        badge_id = badge_widget_id,
        archived_badge_id = archived_badge_widget_id,
        star_id = star_widget_id,
    );
    // Reuses this card's cached provider if one exists rather than
    // always constructing a fresh one (see tile_css_providers).
    let mut providers = tile_css_providers.borrow_mut();
    match providers.get(&card.id) {
        Some(existing) => existing.load_from_string(&css_text),
        None => {
            let css = gtk::CssProvider::new();
            css.load_from_string(&css_text);
            gtk::style_context_add_provider_for_display(
                &default_display(),
                &css,
                // PRIORITY_USER, for consistency with register_css and
                // name_label_css's provider (see register_css).
                gtk::STYLE_PROVIDER_PRIORITY_USER,
            );
            providers.insert(card.id, css);
        }
    }
    drop(providers);

    // Wrapped in an Overlay so a starred card can get a small
    // filled-star badge in the top-right corner, and while multiselect
    // is active, every tile gets a checked or empty box badge in the
    // top-left (see TileSelectionBadge).
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&card_face));

    if card.starred {
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.set_widget_name(&star_widget_id);
        star.set_pixel_size(20);
        star.set_halign(gtk::Align::End);
        star.set_valign(gtk::Align::Start);
        star.set_margin_end(8);
        star.set_margin_top(6);
        // An overlay child covers its full allocation for input by
        // default, which would eat taps meant for the tile underneath.
        star.set_can_target(false);
        overlay.add_overlay(&star);
    }

    match selection_badge {
        TileSelectionBadge::None => {}
        TileSelectionBadge::Checked | TileSelectionBadge::Unchecked => {
            // Color comes from the CSS rule already added to this
            // tile's provider (targeting badge_widget_id).
            let icon_name = if selection_badge == TileSelectionBadge::Checked {
                "checkbox-checked-symbolic"
            } else {
                "checkbox-symbolic"
            };
            let badge = gtk::Image::from_icon_name(icon_name);
            badge.set_widget_name(&badge_widget_id);
            badge.set_pixel_size(24);
            badge.set_halign(gtk::Align::Start);
            badge.set_valign(gtk::Align::Start);
            badge.set_margin_start(8);
            badge.set_margin_top(6);
            badge.set_can_target(false);
            overlay.add_overlay(&badge);
        }
    }

    // Otherwise the only way to tell an archived card from a normal
    // one in the grid is to open it, or toggle Hide Archived Cards.
    // Smaller than the selection badges' 24px: a lower-priority
    // informational indicator, not an interactive selection state.
    if card.archived {
        let archived_badge = gtk::Image::from_icon_name("shoe-box-symbolic");
        archived_badge.set_widget_name(&archived_badge_widget_id);
        archived_badge.set_pixel_size(16);
        archived_badge.set_halign(gtk::Align::End);
        archived_badge.set_valign(gtk::Align::End);
        archived_badge.set_margin_end(8);
        archived_badge.set_margin_bottom(6);
        archived_badge.set_can_target(false);
        overlay.add_overlay(&archived_badge);
    }

    overlay.set_width_request(CARD_TILE_WIDTH);
    overlay.set_height_request(CARD_TILE_HEIGHT);

    overlay.upcast()
}

fn show_card_detail(
    card: &Card,
    content_toolbar: &adw::ToolbarView,
    content_header: &adw::HeaderBar,
    store: &Rc<RefCell<CardStore>>,
    refresh: &Rc<RefCell<dyn Fn()>>,
    rebuild_grid: &Rc<RefCell<dyn Fn()>>,
    rebuild_group_tabs: &Rc<RefCell<dyn Fn()>>,
    selected_group_filter: &Rc<RefCell<Option<Uuid>>>,
    split_view: &adw::NavigationSplitView,
    window_weak: &glib::WeakRef<adw::ApplicationWindow>,
    banner: &SharedBanner,
    edit_button: &gtk::Button,
    info_button: &gtk::MenuButton,
    star_button: &gtk::ToggleButton,
    archive_button: &gtk::ToggleButton,
    share_menu_button: &gtk::MenuButton,
    copy_code_action: &gio::SimpleAction,
    copy_image_action: &gio::SimpleAction,
    link_button: &gtk::Button,
    bottom_bar: &gtk::ActionBar,
    edit_handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
    star_handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
    archive_handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
    copy_code_handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
    copy_image_handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
    link_handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
    show_archived: &Rc<std::cell::Cell<bool>>,
    name_label_css: &gtk::CssProvider,
    name_label_colors: &Rc<RefCell<(String, String)>>,
    toast_overlay: &adw::ToastOverlay,
) {
    content_header.set_title_widget(Some(&adw::WindowTitle::new(&card.name, "")));
    let text_color = contrasting_text_color(&card.color);
    *name_label_colors.borrow_mut() = (card.color.clone(), text_color.to_string());
    update_header_style(name_label_css, &card.color, text_color);

    edit_button.set_visible(true);
    bottom_bar.set_visible(true);
    disconnect_if_present(link_button, &link_handler);
    match card.link.clone() {
        Some(link) => {
            link_button.set_tooltip_text(Some(&link));
            link_button.set_visible(true);
            let window_weak = window_weak.clone();
            let new_link_id = link_button.connect_clicked(move |_| {
                let window = window_weak.upgrade();
                gtk::UriLauncher::new(&link).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
            });
            *link_handler.borrow_mut() = Some(new_link_id);
        }
        None => link_button.set_visible(false),
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 16);
    // No set_margin_* here: GTK margin is space outside a widget's
    // painted background, which is why the gradient was only covering
    // an inset box. The same visual inset is done as CSS padding
    // instead, in update_header_style's #barkeep-card-body rule.
    vbox.set_vexpand(true);
    // Set fresh here since vbox is a brand new widget every call,
    // unlike content_header which only needs this once at construction.
    vbox.set_widget_name("barkeep-card-body");

    // Computed here since both groups and expiry need it, though
    // they're displayed separately below.
    let group_names: Vec<String> = {
        let store_ref = store.borrow();
        card.group_ids
            .iter()
            .filter_map(|id| store_ref.groups.iter().find(|g| g.id == *id))
            .map(|g| g.name.clone())
            .collect()
    };

    if let Some(path) = barcode::render_png(card) {
        let picture = gtk::Picture::for_filename(&path);
        // Fill for 1D barcodes: stretches to the full available height
        // and width rather than preserving the tall native aspect
        // ratio, which was causing them to go very narrow on a short
        // window (Contain shrinks width to keep that ratio intact
        // within limited height). Safe for 1D formats (see
        // BarcodeType::is_linear); 2D formats keep Contain, since
        // stretching would distort square modules into rectangles.
        picture.set_content_fit(if card.barcode_type.is_linear() {
            gtk::ContentFit::Fill
        } else {
            gtk::ContentFit::Contain
        });
        picture.set_vexpand(true);
        picture.set_hexpand(true);
        // A height_request floor was tried and didn't fix reported
        // fuzziness. The real fix is upstream in barcode.rs: 1D
        // barcodes render at 1px native height, so this Picture always
        // upscales rather than downscales.
        vbox.append(&picture);

        if !card.code_value.is_empty() {
            let code_label = gtk::Label::builder()
                .selectable(true)
                // selectable(true) makes this label focusable, and GTK
                // grabs focus on the first focusable descendant on
                // first show if nothing already has focus, which was
                // landing here and showing the code pre-selected.
                .can_focus(false)
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .justify(gtk::Justification::Center)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .max_width_chars(40)
                .css_classes(["monospace".to_string(), "dim-label".to_string()])
                .build();
            // Pango markup rather than a CSS "bold" class, since
            // Adwaita doesn't ship a bare utility class for that.
            code_label.set_markup(&format!(
                "<b>{}</b>",
                glib::markup_escape_text(&card.code_value)
            ));
            vbox.append(&code_label);
        }

        if !group_names.is_empty() {
            let groups_label = gtk::Label::new(Some(&group_names.join(", ")));
            groups_label.set_halign(gtk::Align::Center);
            groups_label.add_css_class("dim-label");
            vbox.append(&groups_label);
        }
        if let Some(expiry) = &card.expiry {
            let today = glib::DateTime::now_local()
                .map(|d| d.format("%Y-%m-%d").unwrap_or_default().to_string())
                .unwrap_or_default();
            let is_expired = !today.is_empty() && expiry.as_str() < today.as_str();
            let expiry_label = gtk::Label::new(Some(&if is_expired {
                format!("Expired {expiry}")
            } else {
                format!("Expires {expiry}")
            }));
            expiry_label.set_halign(gtk::Align::Center);
            if is_expired {
                expiry_label.add_css_class("error");
            } else {
                expiry_label.add_css_class("dim-label");
            }
            vbox.append(&expiry_label);
        }
    } else {
        let placeholder = adw::StatusPage::builder()
            .icon_name("image-missing-symbolic")
            .title("No Code Yet")
            .description("Edit this card to add a barcode or QR code.")
            .build();
        vbox.append(&placeholder);
    }

    // A fresh Popover per card since its content changes with the
    // selection; MenuButton owns showing/positioning/dismissing it,
    // including clamping to fit narrow phone-width windows.
    if card.notes.is_empty() {
        info_button.set_popover(gtk::Popover::NONE);
        info_button.set_visible(false);
    } else {
        let notes_label = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(32)
            .halign(gtk::Align::Start)
            .selectable(true)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        notes_label.set_markup(&linkify_urls(&card.notes));
        let popover = gtk::Popover::builder().child(&notes_label).build();
        info_button.set_popover(Some(&popover));
        info_button.set_visible(true);
    }

    content_toolbar.set_content(Some(&vbox));

    // Swap out the previous card's click handler rather than stacking a
    // new one on top of it.
    disconnect_if_present(edit_button, &edit_handler);

    let new_edit_id = {
        let store = store.clone();
        let refresh = refresh.clone();
        let rebuild_grid = rebuild_grid.clone();
        let split_view = split_view.clone();
        let content_toolbar = content_toolbar.clone();
        let content_header = content_header.clone();
        let window_weak = window_weak.clone();
        let banner = banner.clone();
        let edit_button = edit_button.clone();
        let info_button = info_button.clone();
        let star_button = star_button.clone();
        let edit_handler_outer = edit_handler.clone();
        let star_handler_outer = star_handler.clone();
        let archive_button = archive_button.clone();
        let archive_handler_outer = archive_handler.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let selected_group_filter = selected_group_filter.clone();
        let share_menu_button = share_menu_button.clone();
        let copy_code_action = copy_code_action.clone();
        let copy_code_handler_outer = copy_code_handler.clone();
        let copy_image_action = copy_image_action.clone();
        let copy_image_handler_outer = copy_image_handler.clone();
        let link_button = link_button.clone();
        let link_handler_outer = link_handler.clone();
        let bottom_bar = bottom_bar.clone();
        let toast_overlay = toast_overlay.clone();
        let show_archived = show_archived.clone();
        let name_label_css = name_label_css.clone();
        let name_label_colors = name_label_colors.clone();
        let card = card.clone();
        edit_button.clone().connect_clicked(move |_| {
            let Some(win) = window_weak.upgrade() else { return };
            // Re-shows the updated card's detail view after saving,
            // instead of leaving refresh()'s "no card selected"
            // placeholder up.
            let on_saved = {
                let content_toolbar = content_toolbar.clone();
                let content_header = content_header.clone();
                let store = store.clone();
                let refresh = refresh.clone();
                let rebuild_grid = rebuild_grid.clone();
                let rebuild_group_tabs = rebuild_group_tabs.clone();
                let selected_group_filter = selected_group_filter.clone();
                let split_view = split_view.clone();
                let window_weak = window_weak.clone();
                let banner = banner.clone();
                let edit_button = edit_button.clone();
                let info_button = info_button.clone();
                let star_button = star_button.clone();
                let edit_handler_outer = edit_handler_outer.clone();
                let star_handler_outer = star_handler_outer.clone();
                let archive_button = archive_button.clone();
                let archive_handler_outer = archive_handler_outer.clone();
                let share_menu_button = share_menu_button.clone();
                let copy_code_action = copy_code_action.clone();
                let copy_code_handler_outer = copy_code_handler_outer.clone();
                let copy_image_action = copy_image_action.clone();
                let copy_image_handler_outer = copy_image_handler_outer.clone();
                let link_button = link_button.clone();
                let link_handler_outer = link_handler_outer.clone();
                let bottom_bar = bottom_bar.clone();
                let show_archived = show_archived.clone();
                let name_label_css = name_label_css.clone();
                let name_label_colors = name_label_colors.clone();
                let toast_overlay = toast_overlay.clone();
                let split_view_for_show = split_view.clone();
                move |updated: Card| {
                    (rebuild_grid.borrow())();
                    // Bypasses refresh() to keep the detail view open
                    // on the just-saved card, so it needs its own
                    // explicit call here.
                    (rebuild_group_tabs.borrow())();
                    show_card_detail(
                        &updated,
                        &content_toolbar,
                        &content_header,
                        &store,
                        &refresh,
                        &rebuild_grid,
                        &rebuild_group_tabs,
                        &selected_group_filter,
                        &split_view,
                        &window_weak,
                        &banner,
                        &edit_button,
                        &info_button,
                        &star_button,
                        &archive_button,
                        &share_menu_button,
                        &copy_code_action,
                        &copy_image_action,
                        &link_button,
                        &bottom_bar,
                        &edit_handler_outer,
                        &star_handler_outer,
                        &archive_handler_outer,
                        &copy_code_handler_outer,
                        &copy_image_handler_outer,
                        &link_handler_outer,
                        &show_archived,
                        &name_label_css,
                        &name_label_colors,
                        &toast_overlay,
                    );
                    split_view_for_show.set_show_content(true);
                }
            };
            open_edit_dialog(&win, &banner, card.clone(), false, store.clone(), refresh.clone(), split_view.clone(), Rc::new(Some(Box::new(on_saved))), rebuild_group_tabs.clone(), rebuild_grid.clone(), selected_group_filter.clone(), EditDialogTab::Card);
        })
    };
    *edit_handler.borrow_mut() = Some(new_edit_id);

    // Disconnect before syncing set_active: ToggleButton fires
    // "toggled" on a programmatic change too, so without disconnecting
    // first, syncing here would re-toggle the previously shown card.
    disconnect_if_present(star_button, &star_handler);
    star_button.set_active(card.starred);
    star_button.set_visible(true);

    let new_star_id = {
        let store = store.clone();
        let rebuild_grid = rebuild_grid.clone();
        let card_id = card.id;
        let banner = banner.clone();
        let star_handler = star_handler.clone();
        star_button.connect_toggled(move |btn| {
            // rebuild_grid, not refresh: refresh also resets the
            // detail pane, which would close the card just starred.
            let result = store.borrow_mut().toggle_star(card_id);
            if result.is_err() {
                // GTK already set the button's active state
                // optimistically before this handler ran, so a failed
                // save (mutation never applied) needs the revert
                // signal-blocked, or set_active() would recursively
                // re-enter this same closure.
                if let Some(id) = star_handler.borrow().as_ref() {
                    btn.block_signal(id);
                    btn.set_active(!btn.is_active());
                    btn.unblock_signal(id);
                }
            }
            report_if_save_failed(&banner, result);
            (rebuild_grid.borrow())();
        })
    };
    *star_handler.borrow_mut() = Some(new_star_id);

    // Doesn't navigate back to the grid on archiving even though the
    // card may now be filtered out of it: an earlier version did that
    // and it was jarring to get forced out of the card you were just
    // looking at. The grid catches up whenever the person navigates
    // away themselves.
    disconnect_if_present(archive_button, &archive_handler);
    archive_button.set_active(card.archived);
    archive_button.set_tooltip_text(Some(if card.archived {
        "Unarchive"
    } else {
        "Archive"
    }));
    archive_button.set_visible(true);

    let new_archive_id = {
        let store = store.clone();
        let rebuild_grid = rebuild_grid.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let card_id = card.id;
        let banner = banner.clone();
        let archive_handler = archive_handler.clone();
        archive_button.connect_toggled(move |btn| {
            let result = store.borrow_mut().toggle_archived(card_id);
            if result.is_err() {
                // Same revert-on-failure reasoning as star_button above.
                if let Some(id) = archive_handler.borrow().as_ref() {
                    btn.block_signal(id);
                    btn.set_active(!btn.is_active());
                    btn.unblock_signal(id);
                }
            }
            report_if_save_failed(&banner, result);
            let now_archived = btn.is_active();
            btn.set_tooltip_text(Some(if now_archived {
                "Unarchive"
            } else {
                "Archive"
            }));
            (rebuild_grid.borrow())();
            // If this card is the last visible member of some group,
            // archiving it needs that group's tab to disappear.
            (rebuild_group_tabs.borrow())();
        })
    };
    *archive_handler.borrow_mut() = Some(new_archive_id);

    share_menu_button.set_visible(!card.code_value.is_empty());

    disconnect_if_present(copy_code_action, &copy_code_handler);
    let new_copy_code_id = {
        let code_value = card.code_value.clone();
        let toast_overlay_for_copy = toast_overlay.clone();
        let share_menu_button = share_menu_button.clone();
        copy_code_action.connect_activate(move |_action, _param| {
            // Widget::clipboard() works from any realized widget on the
            // relevant display; share_menu_button stands in since an
            // action has no widget of its own to call it from.
            share_menu_button.clipboard().set_text(&code_value);
            toast_overlay_for_copy.add_toast(adw::Toast::new("Copied to clipboard"));
        })
    };
    *copy_code_handler.borrow_mut() = Some(new_copy_code_id);

    // Copies the card's rendered barcode to the clipboard as an image,
    // standing in for a platform "share" action; no universal
    // share-sheet API exists on Linux desktop.
    disconnect_if_present(copy_image_action, &copy_image_handler);
    let new_copy_image_id = {
        let card_for_share = card.clone();
        let toast_overlay_for_share = toast_overlay.clone();
        let share_menu_button = share_menu_button.clone();
        copy_image_action.connect_activate(move |_action, _param| {
            let message = match barcode::render_png(&card_for_share) {
                Some(path) => match gtk::gdk::Texture::from_filename(&path) {
                    Ok(texture) => {
                        share_menu_button.clipboard().set_texture(&texture);
                        "Barcode image copied to clipboard"
                    }
                    Err(_) => "Couldn't copy the barcode image",
                },
                None => "Couldn't render a barcode to copy",
            };
            toast_overlay_for_share.add_toast(adw::Toast::new(message));
        })
    };
    *copy_image_handler.borrow_mut() = Some(new_copy_image_id);
}

/// Converts a "#RRGGBB" string to an RGBA GTK color, falling back to
/// Barkeep's default accent blue if the string doesn't parse.
fn hex_to_rgba(hex: &str) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::parse(hex).unwrap_or(gtk::gdk::RGBA::new(0.208, 0.518, 0.894, 1.0))
}

fn rgba_to_hex(rgba: &gtk::gdk::RGBA) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (rgba.red() * 255.0).round() as u8,
        (rgba.green() * 255.0).round() as u8,
        (rgba.blue() * 255.0).round() as u8,
    )
}

/// Shared add/edit form. `is_new` controls whether Save does an add or
/// an update: `true` for both a blank manual-entry card and a
/// scan-prefilled one (neither exists in the store yet), `false` only
/// when editing an already-saved card.
/// Which tab open_edit_dialog should start on: Card by default,
/// Options when re-opening after Manage Groups, since that's the tab
/// the Groups section lives on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditDialogTab {
    Card,
    Options,
}

fn open_edit_dialog(
    window: &adw::ApplicationWindow,
    banner: &SharedBanner,
    initial: Card,
    is_new: bool,
    store: Rc<RefCell<CardStore>>,
    refresh: Rc<RefCell<dyn Fn()>>,
    split_view: adw::NavigationSplitView,
    // Rc-wrapped rather than a bare Box<dyn Fn(Card)>: both the Save
    // button's handler and the Manage Groups button's handler need
    // their own access to this, and a boxed closure can only be moved
    // into one place, not cloned into two.
    on_saved: Rc<Option<Box<dyn Fn(Card)>>>,
    // Needed to pass along to open_manage_groups_dialog when Manage
    // Groups is clicked, since that function's signature wants them
    // directly, not a general "refresh everything" callback.
    rebuild_group_tabs: Rc<RefCell<dyn Fn()>>,
    rebuild_grid: Rc<RefCell<dyn Fn()>>,
    selected_group_filter: Rc<RefCell<Option<Uuid>>>,
    initial_tab: EditDialogTab,
) {
    let cache_key = initial.id.to_string();
    let card = Rc::new(RefCell::new(initial));

    let name_row = adw::EntryRow::builder()
        .title("Name")
        .text(&card.borrow().name)
        .build();
    let code_row = adw::EntryRow::builder()
        .title("Code value")
        .text(&card.borrow().code_value)
        .build();
    // Multi-line, not adw::EntryRow like the other fields: Enter needs
    // to insert a line break, which EntryRow (built on GtkEntry)
    // doesn't support. gtk::TextView handles Enter-as-newline by
    // default. Appended to form_box directly, not a ListBox row, since
    // a tall multi-line editor doesn't fit alongside single-line rows.
    let notes_title = gtk::Label::builder()
        .label("Notes")
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    let notes_buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    notes_buffer.set_text(&card.borrow().notes);
    let notes_view = gtk::TextView::builder()
        .buffer(&notes_buffer)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    let notes_scroller = gtk::ScrolledWindow::builder()
        .child(&notes_view)
        .min_content_height(100)
        .max_content_height(220)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["view", "frame"])
        .build();
    let notes_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    notes_box.set_margin_top(6);
    notes_box.set_margin_bottom(6);
    notes_box.append(&notes_title);
    notes_box.append(&notes_scroller);
    let link_row = adw::EntryRow::builder()
        .title("Link (URL)")
        .text(card.borrow().link.as_deref().unwrap_or(""))
        .build();

    // Pill-style multi-select group picker, replacing the old
    // single-string category field. A plain ToggleButton already gets
    // a distinct checked/highlighted appearance from Adwaita's
    // stylesheet, no custom CSS needed beyond the "pill" class for
    // shape. Reuses gtk::FlowBox, the same widget class as the card
    // grid.
    let selected_group_ids: Rc<RefCell<Vec<Uuid>>> =
        Rc::new(RefCell::new(card.borrow().group_ids.clone()));
    // Reduces the pills' internal padding below the stock .pill
    // default, which was generous enough that short labels like
    // "Group A" read as almost the same width as longer ones. No
    // background-color override for the unchecked state; GTK's stock
    // .pill styling provides contrast on its own. A fresh CssProvider
    // is registered every time this dialog opens rather than once
    // globally, since a realistic session doesn't open it often
    // enough for that to matter.
    register_css(
        ".compact-pill { \
             padding: 2px 10px; \
             min-height: 0; \
         } \
         .compact-pill:checked { \
             background-color: var(--accent-bg-color); \
             color: var(--accent-fg-color); \
         }",
    );
    let groups_flow_box = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .row_spacing(6)
        .column_spacing(6)
        .homogeneous(false)
        .build();
    for group in &store.borrow().groups {
        let pill = gtk::ToggleButton::builder()
            .label(&group.name)
            .active(selected_group_ids.borrow().contains(&group.id))
            .css_classes(["pill", "compact-pill"])
            .build();
        let selected_group_ids = selected_group_ids.clone();
        let group_id = group.id;
        pill.connect_toggled(move |btn| {
            let mut ids = selected_group_ids.borrow_mut();
            if btn.is_active() {
                if !ids.contains(&group_id) {
                    ids.push(group_id);
                }
            } else {
                ids.retain(|id| *id != group_id);
            }
        });
        groups_flow_box.insert(&pill, -1);
    }
    let groups_row_title = gtk::Label::builder()
        .label("Groups")
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    let groups_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    groups_box.set_margin_top(6);
    groups_box.set_margin_bottom(6);
    // Always shown, even with no groups yet, so there's still a way to
    // discover and reach group management from here.
    let has_groups = !store.borrow().groups.is_empty();
    groups_box.append(&groups_row_title);
    if has_groups {
        groups_box.append(&groups_flow_box);
    } else {
        let no_groups_label = gtk::Label::builder()
            .label("No groups yet")
            .halign(gtk::Align::Start)
            .css_classes(["dim-label"])
            .build();
        groups_box.append(&no_groups_label);
    }
    // Click handler attached later, alongside save_button's (see that
    // comment): several fields it needs to snapshot aren't constructed
    // yet at this point.
    let manage_groups_button = gtk::Button::builder()
        .label("Manage Groups")
        .halign(gtk::Align::Center)
        .margin_top(20)
        .build();
    groups_box.append(&manage_groups_button);

    // Date picker: an ActionRow that opens a popover with a
    // gtk::Calendar instead of free-text entry. `current_value` is
    // created by the caller and passed in rather than owned internally,
    // so `on_change` can close over the single real Rc directly instead
    // of needing a manually-resynced duplicate. `on_change` fires after
    // every mutation (a day picked, or Clear Date).
    fn build_date_row(
        title: &str,
        current_value: Rc<RefCell<Option<String>>>,
        on_change: impl Fn() + 'static,
    ) -> adw::ActionRow {
        let on_change: Rc<dyn Fn()> = Rc::new(on_change);

        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(current_value.borrow().as_deref().unwrap_or("Not set"))
            .activatable(true)
            .build();

        let calendar = gtk::Calendar::new();
        if let Some(date_str) = current_value.borrow().as_deref() {
            if let Some(date) = parse_iso_date(date_str) {
                calendar.select_day(&date);
            }
        }

        let popover_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        popover_box.append(&calendar);
        let clear_button = gtk::Button::with_label("Clear Date");
        clear_button.add_css_class("flat");
        popover_box.append(&clear_button);

        let popover = gtk::Popover::new();
        popover.set_child(Some(&popover_box));
        popover.set_parent(&row);

        {
            let popover = popover.clone();
            row.connect_activated(move |_| popover.popup());
        }

        {
            let row = row.clone();
            let current_value = current_value.clone();
            let on_change = on_change.clone();
            let popover = popover.clone();
            calendar.connect_day_selected(move |cal| {
                let date = cal.date();
                let iso = date.format("%Y-%m-%d").map(|s| s.to_string()).unwrap_or_default();
                if !iso.is_empty() {
                    *current_value.borrow_mut() = Some(iso.clone());
                    row.set_subtitle(&iso);
                    on_change();
                }
                popover.popdown();
            });
        }

        {
            let row = row.clone();
            let current_value = current_value.clone();
            let popover = popover.clone();
            clear_button.connect_clicked(move |_| {
                *current_value.borrow_mut() = None;
                row.set_subtitle("Not set");
                on_change();
                popover.popdown();
            });
        }

        row
    }

    let valid_from_value: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(
        card.borrow().valid_from.clone().filter(|s| !s.trim().is_empty()),
    ));
    let valid_from_row = build_date_row("Valid From", valid_from_value.clone(), || {});

    let expiry_value: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(
        card.borrow().expiry.clone().filter(|s| !s.trim().is_empty()),
    ));

    let on_expiration_model = gtk::StringList::new(
        &OnExpiration::all()
            .iter()
            .map(|o| o.label())
            .collect::<Vec<_>>(),
    );
    let on_expiration_row = adw::ComboRow::builder()
        .title("On Expiration")
        .model(&on_expiration_model)
        .selected(
            OnExpiration::all()
                .iter()
                .position(|o| *o == card.borrow().on_expiration)
                .unwrap_or(0) as u32,
        )
        .sensitive(expiry_value.borrow().is_some())
        .build();

    // Built after on_expiration_row so its on_change closure can
    // capture it and keep its sensitivity live-synced with expiry_value.
    let expiry_row = {
        let on_expiration_row = on_expiration_row.clone();
        let expiry_value_for_change = expiry_value.clone();
        build_date_row("Expiry", expiry_value.clone(), move || {
            on_expiration_row.set_sensitive(expiry_value_for_change.borrow().is_some());
        })
    };

    let color_dialog = gtk::ColorDialog::builder().with_alpha(false).build();
    let color_button = gtk::ColorDialogButton::builder()
        .dialog(&color_dialog)
        .rgba(&hex_to_rgba(&card.borrow().color))
        .valign(gtk::Align::Center)
        .build();
    let color_row = adw::ActionRow::builder().title("Color").build();
    color_row.add_suffix(&color_button);

    let type_model = gtk::StringList::new(
        &BarcodeType::all()
            .iter()
            .map(|t| t.label())
            .collect::<Vec<_>>(),
    );
    let type_row = adw::ComboRow::builder()
        .title("Code type")
        .model(&type_model)
        .selected(
            BarcodeType::all()
                .iter()
                .position(|t| *t == card.borrow().barcode_type)
                .unwrap_or(0) as u32,
        )
        .build();

    // Live preview of the barcode/QR as typed, so it's easy to match a
    // physical card's symbology to the right dropdown entry by eye.
    let preview_picture = gtk::Picture::builder()
        .hexpand(true)
        .height_request(100)
        .visible(false)
        .build();

    let update_preview: Rc<RefCell<dyn Fn()>> = {
        let preview_picture = preview_picture.clone();
        let code_row = code_row.clone();
        let type_row = type_row.clone();
        let cache_key = cache_key.clone();
        Rc::new(RefCell::new(move || {
            let value = code_row.text().to_string();
            let barcode_type = *BarcodeType::all()
                .get(type_row.selected() as usize)
                .unwrap_or(&BarcodeType::QrCode);
            // Same reasoning as the main detail view: 1D barcodes
            // render at 1px native height, so Contain was scaling by
            // the width constraint on an extreme aspect ratio,
            // shrinking the source to a fraction of a pixel. Set fresh
            // each time since barcode_type can change with the dropdown.
            preview_picture.set_content_fit(if barcode_type.is_linear() {
                gtk::ContentFit::Fill
            } else {
                gtk::ContentFit::Contain
            });
            match barcode::render_preview_png(&value, barcode_type, &cache_key) {
                Some(path) => {
                    preview_picture.set_filename(Some(&path));
                    preview_picture.set_visible(true);
                }
                None => preview_picture.set_visible(false),
            }
        }))
    };
    (update_preview.borrow())();
    {
        let update_preview = update_preview.clone();
        code_row.connect_changed(move |_| (update_preview.borrow())());
    }
    {
        let update_preview = update_preview.clone();
        type_row.connect_selected_notify(move |_| (update_preview.borrow())());
    }

    let card_list_box = gtk::ListBox::builder()
        .css_classes(vec!["boxed-list".to_string()])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    card_list_box.append(&name_row);
    card_list_box.append(&type_row);
    card_list_box.append(&code_row);
    card_list_box.append(&color_row);

    let options_list_box = gtk::ListBox::builder()
        .css_classes(vec!["boxed-list".to_string()])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    options_list_box.append(&link_row);
    options_list_box.append(&valid_from_row);
    options_list_box.append(&expiry_row);
    options_list_box.append(&on_expiration_row);

    // Only shown when editing an existing card. Icon + "Delete" text
    // via AdwButtonContent, since gtk::Button's icon_name/label
    // properties don't combine into one. In a bottom ActionBar rather
    // than the header, since ActionBar doesn't stretch children to
    // fill by default, keeping this compact instead of full-width.
    let dialog_delete_button = gtk::Button::new();
    dialog_delete_button.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("user-trash-symbolic")
            .label("Delete")
            .build(),
    ));
    dialog_delete_button.add_css_class("destructive-action");
    dialog_delete_button.set_visible(!is_new);

    let card_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card_page.append(&preview_picture);
    card_page.append(&card_list_box);

    let options_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    options_page.append(&options_list_box);
    options_page.append(&notes_box);
    options_page.append(&groups_box);

    // AdwViewStack rather than a single long scrolling form: the
    // dialog had grown tall enough (preview, six rows, notes, groups)
    // to be a real problem. Same pattern GNOME's Preferences-style
    // dialogs use.
    let view_stack = adw::ViewStack::new();
    view_stack.add_titled(&card_page, Some("card"), "Card");
    view_stack.add_titled(&options_page, Some("options"), "Options");

    // unit(Px): AdwClamp's maximum_size defaults to "sp" units, so 480
    // was being interpreted as 480sp (wider than intended with text
    // scaling above 100%) until set explicitly.
    let clamp = adw::Clamp::builder()
        .child(&view_stack)
        .maximum_size(480)
        .unit(adw::LengthUnit::Px)
        .build();
    clamp.set_margin_top(16);
    clamp.set_margin_bottom(16);
    clamp.set_margin_start(16);
    clamp.set_margin_end(16);

    // propagate_natural_height: without this, a ScrolledWindow's
    // reported natural height defaults to something minimal since it's
    // designed to scroll rather than grow. This makes it report its
    // child's real natural height, letting content_height below act as
    // a generous cap rather than a fixed size. One shared scroller
    // wrapping the whole view_stack, not one per tab.
    let scroller = gtk::ScrolledWindow::builder()
        .child(&clamp)
        .vexpand(true)
        .propagate_natural_height(true)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    // Custom tab strip instead of AdwViewSwitcher: the icons
    // ViewSwitcher fell back to (no explicit page icons were set)
    // didn't render on-device. Styled the same as the main view's
    // group-filter tabs (.group-tab, already registered globally). No
    // scroll/paging buttons needed here since this only ever has two
    // tabs and comfortably fits the dialog's width.
    let card_tab_button = gtk::ToggleButton::builder()
        .label("Card")
        .active(initial_tab == EditDialogTab::Card)
        .css_classes(["flat", "group-tab"])
        .build();
    let options_tab_button = gtk::ToggleButton::builder()
        .label("Options")
        .active(initial_tab == EditDialogTab::Options)
        .css_classes(["flat", "group-tab"])
        .build();
    options_tab_button.set_group(Some(&card_tab_button));
    // view_stack's default visible child ("card", added first) only
    // matches the Card case; Options needs setting explicitly or the
    // tab button and visible content would disagree on startup.
    if initial_tab == EditDialogTab::Options {
        view_stack.set_visible_child_name("options");
    }
    {
        let view_stack = view_stack.clone();
        card_tab_button.connect_toggled(move |btn| {
            if btn.is_active() {
                view_stack.set_visible_child_name("card");
            }
        });
    }
    {
        let view_stack = view_stack.clone();
        options_tab_button.connect_toggled(move |btn| {
            if btn.is_active() {
                view_stack.set_visible_child_name("options");
            }
        });
    }
    let tab_strip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tab_strip.set_halign(gtk::Align::Center);
    tab_strip.append(&card_tab_button);
    tab_strip.append(&options_tab_button);
    header.set_title_widget(Some(&tab_strip));
    toolbar.add_top_bar(&header);

    // In a bottom ActionBar rather than the header, matching the
    // detail view's bottom_bar pattern.
    let save_button = gtk::Button::new();
    save_button.set_child(Some(
        &adw::ButtonContent::builder()
            .icon_name("object-select-symbolic")
            .label("Save")
            .build(),
    ));
    save_button.add_css_class("suggested-action");
    let dialog_bottom_bar = gtk::ActionBar::new();
    dialog_bottom_bar.pack_start(&dialog_delete_button);
    dialog_bottom_bar.pack_end(&save_button);
    toolbar.add_bottom_bar(&dialog_bottom_bar);
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title(if is_new { "Add Card" } else { "Edit Card" })
        .content_width(420)
        // Generous cap, not a target: propagate_natural_height above
        // means the dialog only grows this tall if the content needs
        // it. 560 was being hit as a hard ceiling even with room to
        // spare.
        .content_height(900)
        .child(&toolbar)
        .build();

    {
        let window = window.clone();
        let banner = banner.clone();
        let store = store.clone();
        let refresh = refresh.clone();
        let dialog = dialog.clone();
        let split_view = split_view.clone();
        let card_id = card.borrow().id;
        let card_name = card.borrow().name.clone();
        dialog_delete_button.connect_clicked(move |_| {
            let store = store.clone();
            let refresh = refresh.clone();
            let dialog = dialog.clone();
            let split_view = split_view.clone();
            let banner = banner.clone();
            confirm_delete(&window, "Delete Card?", &format!("This will permanently delete \u{201c}{card_name}\u{201d}."), move || {
                report_if_save_failed(&banner, store.borrow_mut().remove(card_id));
                (refresh.borrow())();
                // refresh() alone only repopulates the content pane's
                // placeholder; this switches back to the sidebar grid
                // in the collapsed single-pane layout.
                split_view.set_show_content(false);
                dialog.close();
            });
        });
    }

    {
        // Owned clone since this closure outlives the current call to
        // open_edit_dialog, which the borrowed reference wouldn't.
        let window = window.clone();
        let dialog = dialog.clone();
        let card = card.clone();
        let store = store.clone();
        let refresh = refresh.clone();
        let split_view = split_view.clone();
        let on_saved = on_saved.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        // Each of these needs its own clone: without it, this
        // closure's move |_| moves each widget out of the surrounding
        // scope, leaving nothing for save_button's handler to use.
        let name_row = name_row.clone();
        let code_row = code_row.clone();
        let notes_buffer = notes_buffer.clone();
        let link_row = link_row.clone();
        let selected_group_ids = selected_group_ids.clone();
        let valid_from_value = valid_from_value.clone();
        let expiry_value = expiry_value.clone();
        let on_expiration_row = on_expiration_row.clone();
        let color_button = color_button.clone();
        let type_row = type_row.clone();
        let banner = banner.clone();
        manage_groups_button.connect_clicked(move |_| {
            // Same field-by-field snapshot save_button builds,
            // duplicated rather than shared: this one is never saved
            // to the store, just carried to the re-opened dialog.
            let snapshot = {
                let mut c = card.borrow_mut();
                c.name = name_row.text().to_string();
                c.code_value = code_row.text().to_string();
                c.notes = notes_buffer
                    .text(&notes_buffer.start_iter(), &notes_buffer.end_iter(), false)
                    .to_string();
                let link_text = link_row.text().to_string();
                c.link = if link_text.trim().is_empty() { None } else { Some(link_text) };
                c.group_ids = selected_group_ids.borrow().clone();
                c.valid_from = valid_from_value.borrow().clone();
                c.expiry = expiry_value.borrow().clone();
                c.on_expiration = *OnExpiration::all()
                    .get(on_expiration_row.selected() as usize)
                    .unwrap_or(&OnExpiration::DoNothing);
                c.color = rgba_to_hex(&color_button.rgba());
                c.barcode_type = *BarcodeType::all()
                    .get(type_row.selected() as usize)
                    .unwrap_or(&BarcodeType::QrCode);
                c.clone()
            };

            dialog.close();

            // A separate binding: open_manage_groups_dialog's first
            // argument borrows `window`, active for the whole call
            // expression, so moving the same variable into the
            // on_closed closure a few arguments later would conflict
            // with it (E0505).
            let window_for_reopen = window.clone();
            let banner_for_reopen = banner.clone();
            let store = store.clone();
            let refresh = refresh.clone();
            let split_view = split_view.clone();
            let on_saved = on_saved.clone();
            let rebuild_group_tabs = rebuild_group_tabs.clone();
            let rebuild_grid = rebuild_grid.clone();
            let selected_group_filter = selected_group_filter.clone();
            open_manage_groups_dialog(
                &window,
                &banner,
                store.clone(),
                rebuild_group_tabs.clone(),
                rebuild_grid.clone(),
                selected_group_filter.clone(),
                Some(Box::new(move || {
                    // Re-opens with the snapshot preserved rather than
                    // patching the still-open dialog's Groups section
                    // in place, reusing open_edit_dialog's existing
                    // construction logic instead of new, narrower
                    // "just refresh this one part" logic.
                    open_edit_dialog(
                        &window_for_reopen,
                        &banner_for_reopen,
                        snapshot.clone(),
                        is_new,
                        store.clone(),
                        refresh.clone(),
                        split_view.clone(),
                        on_saved.clone(),
                        rebuild_group_tabs.clone(),
                        rebuild_grid.clone(),
                        selected_group_filter.clone(),
                        EditDialogTab::Options,
                    );
                })),
            );
        });
    }

    {
        let dialog = dialog.clone();
        let card = card.clone();
        let on_saved = on_saved.clone();
        let banner = banner.clone();
        save_button.connect_clicked(move |_| {
            let snapshot = {
                let mut c = card.borrow_mut();
                c.name = name_row.text().to_string();
                c.code_value = code_row.text().to_string();
                c.notes = notes_buffer
                    .text(&notes_buffer.start_iter(), &notes_buffer.end_iter(), false)
                    .to_string();
                let link_text = link_row.text().to_string();
                c.link = if link_text.trim().is_empty() { None } else { Some(link_text) };
                c.group_ids = selected_group_ids.borrow().clone();
                c.valid_from = valid_from_value.borrow().clone();
                c.expiry = expiry_value.borrow().clone();
                c.on_expiration = *OnExpiration::all()
                    .get(on_expiration_row.selected() as usize)
                    .unwrap_or(&OnExpiration::DoNothing);
                c.color = rgba_to_hex(&color_button.rgba());
                c.barcode_type = *BarcodeType::all()
                    .get(type_row.selected() as usize)
                    .unwrap_or(&BarcodeType::QrCode);
                c.clone()
            };

            if is_new {
                report_if_save_failed(&banner, store.borrow_mut().add(snapshot));
                (refresh.borrow())();
            } else {
                report_if_save_failed(&banner, store.borrow_mut().update(snapshot.clone()));
                // Explicit deref: on_saved is Rc<Option<...>>, and Rc
                // doesn't get the same match-ergonomics deref a plain
                // reference does.
                match &*on_saved {
                    Some(on_saved) => on_saved(snapshot),
                    None => (refresh.borrow())(),
                }
            }
            dialog.close();
        });
    }

    dialog.present(Some(window));
}

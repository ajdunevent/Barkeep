use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use barkeep_core::barcode;
use barkeep_core::brightness;
use crate::camera;
use barkeep_core::card::{BarcodeType, Card, CardStore, Group, ImportOutcome, OnExpiration, UiState};
use uuid::Uuid;

/// Fixed size for every grid tile. FlowBox reflows its own column count
/// from this on its own -- no custom column-count math, no live resize
/// handling, nothing computed from the window's width at all. A prior,
/// much more involved attempt (explicit column-count breakpoints, a
/// polling timer, and later a circular-measurement bug that produced a
/// visible grow/snap-back animation on its own) never fully worked and
/// wasn't worth the complexity for what's a cosmetic detail -- this is
/// deliberately the plain, default FlowBox behavior instead.
const CARD_TILE_WIDTH: i32 = 160;
const CARD_TILE_HEIGHT: i32 = 100;

/// Shown instead of the normal app UI in the rare case
/// CardStore::open_default fails outright (permissions on the XDG data
/// directory, no data directory available at all, invalid UTF-8 in an
/// existing cards.json, etc.) -- a genuine I/O failure, not a parse
/// failure (which CardStore::open already recovers from gracefully on
/// its own, backing up the damaged file first; see that function's own
/// doc comment). Deliberately minimal and self-contained, not reusing
/// any of the main app's own widget-construction machinery below,
/// since all of that assumes a working store that doesn't exist in
/// this case. Quits the application on the only button here, rather
/// than leaving an inert error window open with nothing further to do
/// in it.
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

/// Every widget build_multiselect_toolbar constructs that's still needed
/// later in build_window (for the actual click-handler wiring, which
/// stays in build_window itself since it depends on far more of that
/// function's own state than this construction step does). Named
/// fields rather than a positional tuple, specifically so the call site
/// can't silently mix up which widget is which.
struct MultiselectToolbarWidgets {
    star_button: gtk::ToggleButton,
    archive_button: gtk::ToggleButton,
    change_groups_button: gtk::Button,
    delete_button: gtk::Button,
    clear_all_button: gtk::Button,
    done_button: gtk::Button,
    action_bar_revealer: gtk::Revealer,
}

/// Builds every widget in multiselect's own bottom toolbar and packs
/// them into their final layout -- first piece extracted out of
/// build_window (previously all inline there) as part of splitting that
/// function into smaller, focused pieces. Chosen as the first, safest
/// extraction specifically because this construction doesn't read any
/// of build_window's own state at all, unlike most of the rest of that
/// function -- purely self-contained widget-building, with the actual
/// event-handler wiring for these buttons deliberately left in
/// build_window itself, since that wiring does depend on much of its
/// other state.
fn build_multiselect_toolbar() -> MultiselectToolbarWidgets {
    // Star, by request -- works exactly like Archive: a ToggleButton
    // whose own active state (set from rebuild_grid below) reflects
    // whether all visible-selected cards are already starred, same
    // "unstar only when everything's already starred, star in every
    // other case" rule as Archive's own "unarchive only when everything
    // visible-selected is already archived" one. starred-symbolic --
    // confirmed directly against star_button's own existing icon_name
    // elsewhere in this file (the per-card star toggle), not a fresh
    // guess.
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
    // "Done," not "Cancel" -- reconsidered by request. See this
    // session's own discussion for the reasoning: multiselect no longer
    // auto-exits after a bulk action, so this button's real meaning has
    // shifted from "abort without doing anything" (what "Cancel"
    // implies) to "finished with this multiselect session, exit and
    // deselect everything" -- closer to "Done" regardless of the
    // checkmark-icon question raised alongside it.
    let done_button = gtk::Button::builder().label("Done").build();
    let action_bar = gtk::ActionBar::new();
    // pack_start's own direction is the mirror of pack_end's (see that
    // comment further down): the FIRST pack_start call lands at the
    // true start edge, each subsequent one pushes rightward. Star
    // packed first so it's leftmost, by request.
    action_bar.pack_start(&star_button);
    action_bar.pack_start(&archive_button);
    action_bar.pack_start(&change_groups_button);
    action_bar.pack_start(&delete_button);
    // pack_end order matters here, and my earlier reasoning about which
    // direction it goes was backwards (confirmed by direct observation,
    // not just re-derived): the FIRST pack_end call lands at the true
    // end edge, and each subsequent call pushes inward, away from the
    // edge -- the opposite of what the previous version of this comment
    // claimed. So Done (meant to sit at the true edge) is packed first;
    // Clear All (meant to sit just inside it) is packed second.
    action_bar.pack_end(&done_button);
    action_bar.pack_end(&clear_all_button);
    // Animated show/hide, by request -- same Revealer pattern as
    // search_revealer elsewhere in build_window (same 200ms duration),
    // but SlideUp rather than SlideDown: this bar is anchored to the
    // bottom of the window, so it should slide up into view from the
    // bottom edge, the direction-appropriate analog of search's
    // top-anchored slide down.
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

/// Both add_button and menu_button are gtk::MenuButton -- named fields
/// here, not a positional tuple, specifically because a same-typed
/// tuple's own two positions wouldn't be distinguishable by the
/// compiler at all if the call site ever mixed them up.
struct HeaderMenuButtons {
    add_button: gtk::MenuButton,
    menu_button: gtk::MenuButton,
}

/// The two header menu buttons -- "+" (scan/enter/import) and the
/// hamburger menu (view toggles, export, about) -- built together since
/// both are the same kind of purely self-contained concern: a gio::Menu
/// structure whose items reference action names as plain strings (e.g.
/// "win.scan-camera"), not any actual gio::SimpleAction object. The
/// real actions themselves are created and wired up separately, much
/// later in build_window's own "Wiring" section -- this only has to
/// exist first because the menu items reference their action names by
/// string, not because anything here depends on those actions already
/// existing.
fn build_header_menu_buttons() -> HeaderMenuButtons {
    // "+" opens a menu: Scan with Camera / Scan from Image / Enter Manually,
    // rather than jumping straight to manual entry.
    let add_menu_model = gio::Menu::new();
    add_menu_model.append(Some("Scan with Camera"), Some("win.scan-camera"));
    add_menu_model.append(Some("Scan from Image…"), Some("win.scan-image"));
    add_menu_model.append(Some("Enter Manually"), Some("win.enter-manually"));
    // Its own section (separator above it) rather than appended to the
    // three scan/entry options directly above -- those are all ways of
    // adding one new card by hand; importing a whole file of existing
    // cards is a different kind of action, so it reads better set apart
    // rather than looking like a fourth "add one card" option.
    let add_import_section = gio::Menu::new();
    add_import_section.append(Some("Import from File…"), Some("win.import"));
    add_menu_model.append_section(None, &add_import_section);
    let add_button = gtk::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .menu_model(&add_menu_model)
        .build();
    add_button.set_tooltip_text(Some("Add card"));

    let menu_model = gio::Menu::new();
    let view_section = gio::Menu::new();
    view_section.append(Some("Hide Presently Invalid Cards"), Some("win.show-valid-only"));
    view_section.append(Some("Hide Archived Cards"), Some("win.show-archived"));
    view_section.append(Some("Manage Groups"), Some("win.manage-groups"));
    menu_model.append_section(None, &view_section);
    // Export Cards gets its own section (dividers above and below, from
    // being between two other sections) rather than sitting loose at
    // the top the way both Export and Import used to -- positioned here
    // between Manage Groups and About by request, now that Import has
    // moved to the Add menu instead.
    let export_section = gio::Menu::new();
    export_section.append(Some("Export Collection…"), Some("win.export"));
    menu_model.append_section(None, &export_section);
    // Its own section (rather than appended to the list above) gets GTK to
    // render a separator before it -- standard GNOME placement for About.
    let about_section = gio::Menu::new();
    about_section.append(Some("About Barkeep"), Some("win.about"));
    menu_model.append_section(None, &about_section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu_model)
        .build();

    HeaderMenuButtons { add_button, menu_button }
}

/// Assembles the sidebar's own headerbar (menu button, search toggle,
/// add button, title) and wraps it with sidebar_box into the
/// NavigationPage build_window's split view actually uses. All four
/// parameters are taken by reference, not by value -- most are only
/// ever used once here and could have been moved in safely, but
/// sidebar_box specifically is still mutated later in build_window
/// (group_tabs_row gets inserted into it after this point), so taking
/// it by value here would have silently broken that later call. Taking
/// all four by reference uniformly, rather than three by value and one
/// by reference, keeps the signature simple to reason about instead of
/// needing per-parameter justification for why some move and one
/// doesn't.
fn build_sidebar_headerbar(
    menu_button: &gtk::MenuButton,
    search_toggle_button: &gtk::ToggleButton,
    add_button: &gtk::MenuButton,
    sidebar_box: &gtk::Box,
    banner: &adw::Banner,
) -> adw::NavigationPage {
    let sidebar_header = adw::HeaderBar::new();
    // Far left of the (collapsed, single-pane) headerbar, as its own
    // pack_start widget rather than grouped with the title/add button on
    // the end -- there's nothing else pack_start'd here to contend with.
    sidebar_header.pack_start(menu_button);
    // pack_start again, called after menu_button's own pack_start above
    // -- pack_start pushes each subsequent widget further from the
    // edge, so this lands immediately to the right of the menu button,
    // matching the request directly ("to the right of the menu").
    sidebar_header.pack_start(search_toggle_button);
    sidebar_header.pack_end(add_button);
    let sidebar_title = adw::WindowTitle::new("Barkeep", "");
    sidebar_header.set_title_widget(Some(&sidebar_title));

    let sidebar_toolbar = adw::ToolbarView::new();
    // Raised, not the default Flat -- confirmed directly from the real
    // Adw.ToolbarStyle docs as the officially documented, intended
    // property for exactly this situation ("suitable for content such
    // as utility panes, where some elements are directly adjacent to
    // the top/bottom bars"). Flat itself is documented as "No
    // background, shadow only for scrolled content" -- directly
    // confirming that's the real, root cause of a reported "white/
    // black bands above the headerbar" bug: AdwToolbarView's own
    // internal top-bar wrapper has no background at all in Flat mode,
    // letting the window's raw default background show through
    // wherever the header bar/banner don't fully cover it. This
    // replaces an extended, unsuccessful attempt to fix the same
    // symptom via CSS overrides on that internal wrapper directly (a
    // plain GtkBox this app never constructs and has no direct handle
    // to at all) -- Raised is the real, supported, root-cause fix this
    // library already provides for it, not a CSS workaround.
    sidebar_toolbar.set_top_bar_style(adw::ToolbarStyle::Raised);
    sidebar_toolbar.add_top_bar(&sidebar_header);
    // This page's own instance of the app's shared notification banner
    // (see SharedBanner's own doc comment) -- a second top bar, so it
    // sits directly below the headerbar rather than above it. Added
    // right after the headerbar itself, since AdwToolbarView stacks
    // multiple top bars in the order they're added.
    sidebar_toolbar.add_top_bar(banner);
    // No width clamp here anymore (there used to be one, capping this at
    // a fixed width even in the collapsed single-pane state, purely to
    // avoid a visible snap when the second pane appeared). A grid wants
    // the opposite: full window width when collapsed is the whole point,
    // so it can actually lay out multiple columns on a phone. In the
    // non-collapsed split view, split_view's own min/max-sidebar-width
    // below still pins this pane to a narrow range, same as before.
    sidebar_toolbar.set_content(Some(sidebar_box));

    adw::NavigationPage::builder()
        .title("Barkeep")
        .child(&sidebar_toolbar)
        .build()
}

/// Every widget/state from the detail pane's construction that's still
/// needed later in build_window -- confirmed individually, one variable
/// at a time, not assumed from a low reference count alone (detail_status
/// and content_page both looked "probably local" by count but turned out
/// to be referenced again much later, inside refresh's own closure and
/// split_view's own logic respectively -- exactly the kind of dependency
/// a count-threshold shortcut would have missed). share_actions,
/// share_menu_model, and saved_brightness are the only three genuinely
/// local to this construction and don't appear here at all.
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
    brightness_button: gtk::ToggleButton,
    edit_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    star_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    archive_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    copy_code_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    copy_image_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    link_handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    content_page: adw::NavigationPage,
}

/// Builds the detail pane -- the barcode/card-info side of the split
/// view, its bottom action bar, and every per-card button that lives in
/// it. Purely self-contained construction, no input parameters needed:
/// nothing here reads any of build_window's own other state. The actual
/// per-card wiring (swapping each button's click handler to the
/// currently-selected card) happens later, in show_card_detail, not
/// here -- this only constructs the widgets and the *_handler slots
/// that wiring will fill in.
fn build_detail_pane(banner: &adw::Banner) -> DetailPaneWidgets {
    let detail_status = adw::StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title("No card selected")
        .description("Pick a card from the list, or add a new one.")
        .build();

    let content_toolbar = adw::ToolbarView::new();
    // RaisedBorder, not plain Raised -- by request, to remove the
    // shadow between the header and the content below. Confirmed
    // directly against the official ToolbarStyle docs: "similar to
    // Raised, but with the shadow replaced with a more subtle border,"
    // documented as intended for exactly this ("applications... where a
    // shadow over the content might be undesired"). Still opaque (still
    // reads --headerbar-bg-color the same way Raised does -- see
    // content_toolbar's own widget-name comment further below for that
    // full reasoning), so this doesn't reopen the white/black-bands fix
    // that was the actual reason Flat couldn't be used here at all.
    content_toolbar.set_top_bar_style(adw::ToolbarStyle::RaisedBorder);
    let content_header = adw::HeaderBar::new();
    content_toolbar.add_top_bar(&content_header);
    // This page's own instance of the app's shared notification banner
    // (see SharedBanner's own doc comment) -- a second top bar, so it
    // sits directly below the headerbar rather than above it. Added
    // right after the headerbar itself, since AdwToolbarView stacks
    // multiple top bars in the order they're added.
    content_toolbar.add_top_bar(banner);
    // Targeted by update_header_style (an id-selector CSS rule, the
    // same proven pattern this app already used for the detail view's
    // card-name label before this redesign moved the name into the
    // header itself) -- colors the whole header to match the selected
    // card's own color, set once here since the widget itself is
    // permanent; only the CSS rule's content changes per-card.
    content_header.set_widget_name("barkeep-detail-header");
    // Separate from content_header's own name just above -- confirmed
    // directly against libadwaita's own official CSS variables docs
    // ("AdwToolbarView uses [--headerbar-bg-color/--headerbar-backdrop-
    // color] for the top and bottom bars") as the real, root-cause fix
    // for the reported white/black bands persisting even with Raised
    // already applied: Raised's own "opaque background" is that CSS
    // variable, read by the toolbar view's own internal wrapper, not by
    // content_header itself -- #barkeep-detail-header's own background-
    // color rule only ever painted the HeaderBar widget's own surface,
    // never the separate, Raised-provided background layer sitting
    // behind/around it, which kept rendering the theme's own default
    // header color wherever content_header/the banner didn't happen to
    // cover it exactly. Named here specifically so update_header_style
    // can override that variable, scoped to just this subtree, rather
    // than needing a direct handle to a wrapper widget this app never
    // constructs.
    content_toolbar.set_widget_name("barkeep-content-toolbar");

    // All per-card action buttons live in a bottom ActionBar instead of
    // the headerbar -- with edit/delete/star/archive/copy/brightness
    // (previously all crammed into content_header, six-plus buttons and
    // still growing) plus info/link, the header was genuinely too
    // crowded on a phone-width window. gtk::ActionBar is the real,
    // purpose-built GTK4 widget for exactly this ("a bar of action
    // buttons," pack_start/pack_end matching HeaderBar's own API
    // shape), not something bolted on.
    let bottom_bar = gtk::ActionBar::new();
    content_toolbar.add_bottom_bar(&bottom_bar);

    // Created once and reused -- if these got recreated and re-packed
    // into the bar on every selection instead, they'd pile up. Hidden
    // until something's selected; show_card_detail swaps their click
    // handler rather than creating new buttons each time.
    let edit_button = gtk::Button::from_icon_name("document-edit-symbolic");
    edit_button.set_tooltip_text(Some("Edit"));
    edit_button.set_visible(false);

    // Toggle-able star -- wired per-card in show_card_detail via
    // star_handler, using the same disconnect-then-reconnect-a-fresh-
    // closure pattern edit_button uses -- the handler needs
    // to know *which* card's star it's toggling, and set_active() has
    // to run without that handler firing (a ToggleButton's "toggled"
    // signal fires on programmatic set_active() calls too, not just
    // clicks) when just syncing the button to a newly selected card's
    // actual starred state.
    let star_button = gtk::ToggleButton::builder()
        .icon_name("starred-symbolic")
        .tooltip_text("Star")
        .visible(false)
        .build();

    // Single fixed icon (not a swapping pair like star's) -- tooltip
    // text communicates archived/not-archived state instead. History:
    // "mail-archive-symbolic" (original pick) reportedly didn't render
    // at all on desktop -> "folder-symbolic" -> "folder-work-symbolic"
    // per an explicit icon list -> that one reportedly rendered as a
    // full-color (non-symbolic) icon on phone -> back to
    // "mail-archive-symbolic", which didn't resolve it either. Bundled
    // now instead of guessing a fourth icon-theme name: shoe-box-
    // symbolic.svg, added directly to resources/ and registered in
    // barkeep.gresource.xml the same way external-link-symbolic.svg
    // already was, so this no longer depends on what any given icon
    // theme happens to ship.
    let archive_button = gtk::ToggleButton::builder()
        .icon_name("shoe-box-symbolic")
        .tooltip_text("Archive")
        .visible(false)
        .build();

    // Copy code / copy barcode image, combined into one share menu
    // instead of two separate ActionBar buttons -- folder-publicshare-
    // symbolic is the icon; the two former buttons' actions become menu
    // entries instead. gio::SimpleAction rather than button clicks for
    // the two entries -- gio::Menu items are wired to actions, not
    // click signals, so this is the standard way a GMenu's entries do
    // anything. Each action still gets disconnected/reconnected
    // per-card the same way the old buttons' click handlers did
    // (connect_activate returns a real SignalHandlerId too, GSimpleAction
    // being a GObject like anything else here), via copy_code_handler/
    // copy_image_handler below.
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
    // "share" is this button's own action-group prefix (matching what
    // the menu model above references as "share.copy-code" etc.) --
    // scoped to this widget rather than the window's own action map,
    // since these two actions only make sense in the context of the
    // currently-selected card, the same way the buttons they replaced
    // did.
    share_menu_button.insert_action_group("share", Some(&share_actions));

    // Notes live behind this info icon -- a MenuButton's built-in
    // popover handles narrow-screen placement/clamping itself, so
    // there's no manual width math needed for phone-sized windows.
    // Hidden by default and only shown (with a popover attached) by
    // show_card_detail, for whichever selected card actually has notes.
    // help-about-symbolic is the plain circle-with-"i" glyph GNOME apps
    // use for their About entry -- it ships with Adwaita itself, unlike
    // dialog-information-symbolic, which renders closer to a lightbulb/
    // exclamation mark depending on icon theme.
    let info_button = gtk::MenuButton::builder()
        .icon_name("help-about-symbolic")
        .tooltip_text("Notes")
        .build();
    info_button.set_visible(false);

    // The card's own external link, if it has one. Reconnected per-card
    // (the URL itself changes) the same way as the other per-card
    // buttons, via link_handler below.
    let link_button = gtk::Button::from_icon_name("external-link-symbolic");
    link_button.set_tooltip_text(Some("Open link"));
    link_button.set_visible(false);
    link_button.add_css_class("flat");

    bottom_bar.pack_start(&star_button);
    bottom_bar.pack_start(&archive_button);
    bottom_bar.pack_start(&info_button);
    bottom_bar.pack_start(&edit_button);
    // pack_end pushes each subsequent widget further from the edge (the
    // first one packed ends up closest to it), so to get share/link
    // reading left-to-right on the right side, link is packed first
    // (ends up rightmost/closest to the edge), then share_menu_button
    // (ends up just left of it, closer to center).
    bottom_bar.pack_end(&link_button);
    bottom_bar.pack_end(&share_menu_button);
    // delete_button removed from here entirely -- moved into the
    // edit dialog instead (see open_edit_dialog), by request.

    // Forces the screen to full brightness while a card's barcode is on
    // screen -- useful in bright daylight, where a dim or auto-dimmed
    // screen can be unreadable to a store's barcode scanner. Unlike
    // edit/delete/info above, its click behavior doesn't depend on which
    // card is selected, so it's wired up once here rather than rewired
    // per-card in show_card_detail -- that function only needs to show it
    // and reset it to "off" for the newly selected card (see
    // brightness_button.set_active(false) there). Top-left of the
    // header, separate from the bottom bar's card-content actions,
    // reflecting that it's a display-mode toggle rather than a card
    // action. Icon now swaps with state (weather-clear-symbolic when
    // off/not-maxed, display-brightness-symbolic when forcing full) --
    // wired in its own toggled handler below, alongside the actual
    // brightness-forcing logic, rather than a fixed single icon.
    let brightness_button = gtk::ToggleButton::builder()
        .icon_name("weather-clear-symbolic")
        .tooltip_text("Full Brightness")
        .visible(false)
        .build();
    content_header.pack_start(&brightness_button);

    // Whatever the brightness was right before the button was switched
    // on, so switching it back off restores it instead of just leaving
    // the screen at 100%. `set_active(false)` -- whether from the user's
    // own click, a card switch, `refresh`, or the window closing -- always
    // runs the same "toggled" handler below, so every one of those paths
    // gets the restore for free rather than needing its own copy of this
    // logic.
    let saved_brightness: Rc<RefCell<Option<i32>>> = Rc::new(RefCell::new(None));
    {
        let saved_brightness = saved_brightness.clone();
        brightness_button.connect_toggled(move |btn| {
            if btn.is_active() {
                *saved_brightness.borrow_mut() = brightness::get_brightness();
                if brightness::set_brightness(100).is_none() {
                    // Couldn't reach the brightness D-Bus service --
                    // don't leave the button showing "on" for an effect
                    // that didn't actually happen.
                    btn.set_active(false);
                }
            } else if let Some(prev) = saved_brightness.borrow_mut().take() {
                brightness::set_brightness(prev);
            }

            // weather-clear-symbolic (not maxed) / display-brightness-
            // symbolic (forcing full) -- reflects the *final* state
            // after the active/inactive handling above, so the
            // set_active(false) fallback on failure just above still
            // ends up showing the correct "off" icon rather than
            // whatever was set right before that reset.
            btn.set_icon_name(if btn.is_active() {
                "display-brightness-symbolic"
            } else {
                "weather-clear-symbolic"
            });

            // Rate limit: toggling this rapidly was confirmed to crash
            // Phosh -- almost certainly hammering logind/backlight calls
            // faster than the compositor's own brightness handling can
            // keep up with. Disabling the button for a short cooldown
            // after every toggle (successful or not) makes rapid
            // re-tapping physically impossible rather than trying to
            // debounce the D-Bus calls themselves, which would still
            // leave a window for a fast enough tap sequence to get
            // through.
            btn.set_sensitive(false);
            let btn_for_reenable = btn.clone();
            glib::source::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                btn_for_reenable.set_sensitive(true);
            });
        });
    }

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
        brightness_button,
        edit_handler,
        star_handler,
        archive_handler,
        copy_code_handler,
        copy_image_handler,
        link_handler,
        content_page,
    }
}

/// split_view, toast_overlay, the main window itself, and the card-name
/// label's own reusable CSS provider/color cache -- all still needed
/// later in build_window (window especially, referenced well over a
/// hundred times throughout the rest of that function). sidebar_page,
/// content_page, and brightness_button are all taken by reference, not
/// by value -- content_page in particular is still needed again later
/// in build_window too (see DetailPaneWidgets' own doc comment for that
/// same dependency, already discovered once building the previous
/// extraction), so it can't be safely consumed here.
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
    brightness_button: &gtk::ToggleButton,
) -> SplitViewAndWindow {
    // Always single-pane/stack-navigation style now (tap a card -> full-
    // width detail view with an automatic back button, regardless of
    // how wide the window is) rather than switching to a side-by-side
    // sidebar+detail layout above some width breakpoint -- collapsed(true)
    // set directly rather than left to an adw::Breakpoint watching window
    // width, and sidebar_width_unit/min_sidebar_width/max_sidebar_width
    // are gone entirely along with it, since none of them mean anything
    // once the split view can never actually show both panes side by
    // side.
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

    // Belt-and-suspenders: if the window closes while brightness is still
    // forced on, restore it rather than leaving the display maxed out for
    // whatever session comes after this one. Reuses the toggle's own
    // "toggled" handler (set up in build_detail_pane) rather than
    // duplicating the restore logic here.
    // Explicit app.quit() here, not relying on GApplication's own
    // implicit "quit once the last window closes" behavior -- confirmed,
    // directly, via a real gdb backtrace and a real, temporary
    // application.windows() diagnostic, that this implicit behavior
    // genuinely doesn't fire correctly here: the main thread is
    // confirmed healthy (cleanly sitting in GTK's own normal main loop,
    // not stuck on anything), and the window count itself is confirmed
    // to genuinely reach zero within 500ms of this handler running --
    // yet the process still didn't exit on its own even after waiting
    // over a minute, ruling out a reasonable inactivity-timeout grace
    // period too. Root cause not fully identified -- this is a direct,
    // deliberate fix for the observed effect (the app should always
    // quit once its only window closes, since it never has more than
    // one) rather than a diagnosis of the exact internal mechanism
    // preventing GApplication's own default behavior from working.
    // Guarded on the window count actually being zero rather than
    // called unconditionally, matching the intended, standard
    // behavior this replaces.
    {
        let brightness_button = brightness_button.clone();
        window.connect_close_request(move |win| {
            brightness_button.set_active(false);
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
    // background/text color matching the card) from this one reusable
    // provider (targeted at a fixed widget ID, so it applies to whichever
    // name label currently exists) rather than a fresh provider each
    // time. Font size is set once when a card is opened (see
    // show_card_detail) rather than kept live during a resize -- no
    // resize-driven update loop exists anymore (see CARD_TILE_WIDTH's
    // doc comment for why); the name label just isn't wired back up to
    // one after removing it, and this is a cosmetic detail, not worth
    // reintroducing that complexity for.
    let name_label_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &default_display(),
        &name_label_css,
        // PRIORITY_USER, not PRIORITY_APPLICATION -- same fix and same
        // reasoning as register_css's own change (see that function's
        // own doc comment for the full explanation). This provider
        // directly drives update_header_style's own gradient CSS
        // (#barkeep-card-body's background), part of the same reported
        // "white/black bands" bug register_css's own padding fix only
        // partially addressed, since this provider is separate from
        // register_css entirely and wasn't covered by that earlier fix.
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
    // Loaded once here, early -- needed both by search_toggle_button
    // (constructed shortly below) and by the two hide-toggle actions
    // (constructed much later in this function). Mutated and re-saved
    // whenever any of the three persisted toggles it holds actually
    // changes. Real user data (the cards/groups themselves) is
    // untouched by this -- CardStore's own persistence above is
    // entirely separate; see UiState's own doc comment in card.rs for
    // why these live in a different XDG location entirely.
    let ui_state: Rc<RefCell<UiState>> = Rc::new(RefCell::new(UiState::open_default()));

    // --- Sidebar: search + card grid ---------------------------------
    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text("Search cards")
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    // Revealer instead of plain set_visible -- a real GTK4 widget
    // purpose-built for animated show/hide, rather than the instant pop
    // in/out set_visible gives on its own. search_entry itself stays
    // visible=true (GTK4's default) at all times now; reveal_child on
    // this wrapper is what actually controls whether it's shown, with
    // an animated transition instead of an instant toggle.
    let search_revealer = gtk::Revealer::builder()
        .child(&search_entry)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .reveal_child(false)
        .build();

    // A grid of card-shaped tiles rather than a flat list, to read as
    // "a stack of your loyalty cards" (see build_card_tile). Every tile
    // is a fixed CARD_TILE_WIDTH x CARD_TILE_HEIGHT -- FlowBox's own
    // default reflow picks the column count on its own from that and
    // whatever width it's actually given, no custom sizing logic needed.
    let flow_box = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .homogeneous(true)
        .row_spacing(4)
        .column_spacing(4)
        .margin_start(8)
        .margin_end(8)
        .margin_bottom(12)
        .valign(gtk::Align::Start)
        .build();

    let grid_scroller = gtk::ScrolledWindow::builder()
        .child(&flow_box)
        .vexpand(true)
        .build();

    // Shown instead of grid_scroller when the store has zero cards at
    // all (not when a search just has no matches -- rebuild_grid checks
    // the store's real card count for this, separately from whatever
    // the current search filtered down to).
    let empty_status = adw::StatusPage::builder()
        .title("No Cards Yet")
        .description("Tap the + button to add your first card, or use the menu to import existing ones.")
        .vexpand(true)
        .visible(false)
        .build();

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.append(&search_revealer);
    sidebar_box.append(&grid_scroller);
    sidebar_box.append(&empty_status);

    // Multiselect's own bottom toolbar -- hidden by default (multiselect
    // starts inactive), shown/hidden and the Archive button's own
    // tooltip/active state kept current from within rebuild_grid below,
    // since that's where the current visible+selected set (what these
    // buttons actually need to know) is already being computed.
    // Archive/Groups/Delete grouped on the start side in that order;
    // Clear All and Done on the end side (Clear All just to Done's left,
    // reflecting that it's an infrastructure action over the selection
    // itself, not a card-modifying action like the others, by request)
    // -- a dismiss/reset pair set apart from the real actions next to
    // them, the common GNOME convention for action-bar layouts like
    // this. See build_multiselect_toolbar's own doc comment for the
    // construction details (icons, styling, packing order reasoning).
    let MultiselectToolbarWidgets {
        star_button: multiselect_star_button,
        archive_button: multiselect_archive_button,
        change_groups_button: multiselect_change_groups_button,
        delete_button: multiselect_delete_button,
        clear_all_button: multiselect_clear_all_button,
        done_button: multiselect_done_button,
        action_bar_revealer: multiselect_action_bar_revealer,
    } = build_multiselect_toolbar();
    sidebar_box.append(&multiselect_action_bar_revealer);

    // Menu items reference action names as plain strings; the real
    // actions are wired up separately, much later in this function's
    // own "Wiring" section. See build_header_menu_buttons' own doc
    // comment for the construction details (menu structure, why Import
    // and Export live where they do).
    let HeaderMenuButtons { add_button, menu_button } = build_header_menu_buttons();

    // system-search-symbolic is the standard, near-universal GNOME icon
    // for exactly this -- used across essentially every search-capable
    // GNOME app, unlike some of this app's other icon guesses that
    // turned out to be missing on one platform or another.
    let search_toggle_button = gtk::ToggleButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text("Search")
        .active(ui_state.borrow().search_bar_visible)
        .build();
    // Explicitly matches the button's own initial active state set
    // just above, rather than relying on connect_toggled (attached
    // below) to fire for it -- that handler is attached after .build()
    // already ran, so it wouldn't see whatever active state the builder
    // itself set.
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
                // hidden -- an invisible search term silently still
                // filtering the grid would be confusing. set_text fires
                // search_entry's own existing search-changed handler
                // (search-changed fires on programmatic text changes
                // too, not just typing), which already calls refresh(),
                // so no separate rebuild call is needed here -- and
                // rebuild_grid isn't even declared yet at this point in
                // the function anyway.
                search_entry.set_text("");
            }
        });
    }

    // The shared notification surface for the whole app -- see
    // show_banner's own doc comment for the full reasoning behind
    // replacing every notification dialog with this, and
    // SharedBanner's own doc comment for why there are two AdwBanner
    // instances rather than one. Constructed here, before
    // build_sidebar_headerbar/build_detail_pane are called just below,
    // specifically so one instance can be threaded into each -- each
    // one needs to already exist to be added as that page's own second
    // top bar, right below its own AdwHeaderBar. use_markup(false) on
    // both: error text in particular routinely contains raw file paths
    // and io::Error messages, which can easily contain characters (&,
    // <, >) that would otherwise be parsed as Pango markup rather than
    // shown as plain text.
    // ID selectors, not classes -- .banner-error/etc (an earlier
    // version of this code, alongside a since-removed !important that
    // turned out to be a hard parse error in GTK's own CSS parser, per
    // a real GTK-WARNING seen on a real run) never visibly applied.
    // This file already had the answer, in a pre-existing comment on
    // the card-tile code: "an ID selector is specific enough to beat
    // FlowBoxChild's theme rules on its own, without !important" --
    // the same proven pattern used there is applied here too, rather
    // than continuing to rely on a class selector this codebase had
    // already, previously found insufficient for beating libadwaita's
    // own internal styling.
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
    // Each instance's own button is wired here, once, rather than
    // per-notification -- every notification show_banner displays uses
    // this same unified acknowledge-and-dismiss action, no separate
    // "Retry" (see show_banner's own doc comment for why).
    //
    // Both instances are dismissed together, from either button --
    // fixes a real, reported bug where dismissing the banner on one
    // pane left the other pane's own instance still revealed the next
    // time that pane became visible (a single-pane, collapsed layout
    // means only one instance is ever seen at a time, but show_banner
    // itself already reveals both together for exactly this reason; an
    // earlier version of this dismiss handler only ever dismissed
    // whichever instance's own button was actually clicked, not its
    // sibling, so the two could end up out of sync with each other).
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

    // See build_sidebar_headerbar's own doc comment for the construction
    // details (packing order, title, why sidebar_box is passed by
    // reference).
    let sidebar_page = build_sidebar_headerbar(&menu_button, &search_toggle_button, &add_button, &sidebar_box, &banner.sidebar);

    // --- Detail pane: barcode + card info ---------------------------------
    // See build_detail_pane's own doc comment for the construction
    // details (per-card buttons, bottom bar packing order, the
    // brightness-toggle logic).
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
        brightness_button,
        edit_handler,
        star_handler,
        archive_handler,
        copy_code_handler,
        copy_image_handler,
        link_handler,
        content_page,
    } = build_detail_pane(&banner.content);

    // --- Split view: permanently collapsed, not responsive to width
    // anymore -- see build_split_view_and_window's own doc comment for
    // the construction details --------------------------------------
    let SplitViewAndWindow {
        split_view,
        toast_overlay,
        window,
        name_label_css,
        name_label_colors,
    } = build_split_view_and_window(app, &sidebar_page, &content_page, &brightness_button);

    // --- Wiring --------------------------------------------------------
    // Grid-only half of refresh, split out so the star-toggle handler
    // (added in show_card_detail) can re-sort/rebuild the grid without
    // also resetting the detail pane back to "no card selected" --
    // toggling a card's star shouldn't close the card you're looking at.
    // show_archived's own internal meaning is "show" (true = shown, per
    // its own existing doc comments elsewhere in this file), the
    // inverse of UiState's "hide_archived" -- inverted here on load,
    // same direction show_archived_action's own handler already
    // inverts every time the toggle changes.
    let show_archived: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(!ui_state.borrow().hide_archived));
    // show_valid_only matches UiState's "hide_invalid" directly, not
    // inverted -- confirmed against show_valid_only_action's own
    // existing logic, which already treats the action's own boolean
    // state as equal to show_valid_only itself, unlike show_archived's
    // inverted pair.
    let show_valid_only: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(ui_state.borrow().hide_invalid));
    // None = "All" (the group filter tab strip's default selection).
    // Restored from ui_state now, not always None -- safe even if the
    // persisted group id no longer exists (e.g. deleted since last
    // run), since rebuild_group_tabs' own existing "does this group
    // still exist" check (further down) already resets to None on its
    // very first run if not, the same safety net it already provides
    // for a group that gets deleted while still selected mid-session.
    let selected_group_filter: Rc<RefCell<Option<Uuid>>> = Rc::new(RefCell::new(ui_state.borrow().selected_group));
    // Multiselect's own selection state, tracked by card ID rather than
    // widget reference -- rebuild_grid (below) tears down and recreates
    // every tile from scratch on every call (search, filter changes,
    // saves, deletes, anything), so a selection tracked on the widgets
    // themselves wouldn't survive any of that.
    let selected_card_ids: Rc<RefCell<HashSet<Uuid>>> = Rc::new(RefCell::new(HashSet::new()));
    // One CssProvider per card, kept and reused across every rebuild_grid
    // call rather than a fresh one being constructed and registered
    // every time (which never got cleaned up -- providers accumulated
    // unbounded for the lifetime of the app session, since GTK has no
    // way to know an old one should be dropped once its widget goes
    // away). build_card_tile now reuses this cache's existing provider
    // for a card it's already seen (just calling load_from_string again
    // with whatever's changed, e.g. a different color), only creating
    // and registering a genuinely new one the first time a given card
    // id is seen at all. Pruned of any id no longer in the store at the
    // top of every rebuild_grid call, so a deleted card's own provider
    // doesn't linger forever either.
    let tile_css_providers: Rc<RefCell<HashMap<Uuid, gtk::CssProvider>>> = Rc::new(RefCell::new(HashMap::new()));
    // Whether multiselect mode is active, now a genuinely separate flag
    // from whether selected_card_ids happens to be empty -- reconsidered
    // by request, since the scope grew to include chaining several bulk
    // actions (and a Clear All button) against the same session, where
    // briefly emptying the selection to start a fresh sub-selection
    // shouldn't silently exit the whole mode the way "mode == set
    // non-empty" would have forced. The only thing that turns this back
    // off is the Done button.
    let multiselect_mode_active: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));

    // Self-referential handle so each tile's own right-click/long-press
    // gesture handlers (below) can trigger a full grid rebuild after
    // toggling a selection, without rebuild_grid's own closure needing
    // to capture itself -- same established pattern as
    // open_manage_groups_dialog's own rebuild_rows, and for the same
    // reason (see that comment): a bare `dyn Fn()` behind a RefCell
    // can't be reassigned to a different underlying closure after
    // construction, only Box<dyn Fn()> can. Deliberately a new,
    // separate handle rather than changing rebuild_grid's own type --
    // that type (Rc<RefCell<dyn Fn()>>) is already threaded through
    // several other function signatures across this file, and widening
    // that everywhere just to support this one, local self-reference
    // would be a much larger, riskier change than adding one small,
    // purpose-built variable.
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
        let multiselect_archive_button = multiselect_archive_button.clone();
        let multiselect_star_button = multiselect_star_button.clone();
        let tile_css_providers = tile_css_providers.clone();

        Rc::new(RefCell::new(move || {
            // Saved before tearing down flow_box below, restored (via
            // idle_add_local_once, see the restore site further down)
            // once rebuilding is done -- otherwise every rebuild_grid
            // call, including one triggered by nothing more than
            // toggling a card's own selection, was jumping the whole
            // list back to the top, since flow_box's old widgets (which
            // the scroll position was relative to) no longer exist once
            // torn down and recreated.
            let saved_scroll_position = grid_scroller.vadjustment().value();
            // Only saved when the group filter actually differs from
            // what's currently persisted -- rebuild_grid runs on every
            // search keystroke and every selection toggle too, neither
            // of which touches the group filter itself, so comparing
            // first avoids writing to disk on every one of those.
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
            // Prunes any cached provider whose card no longer exists at
            // all (deleted since the last rebuild) -- not just currently
            // filtered out by search/group/archive visibility, which
            // shouldn't drop a card's own cached provider just because
            // it's temporarily hidden. See tile_css_providers' own doc
            // comment for why this cache exists. Explicitly unregisters
            // each pruned provider from the display's own style context,
            // not just dropped from this HashMap -- removing only this
            // struct's own reference wouldn't actually deregister it,
            // since GTK's style-context machinery holds its own separate
            // reference once a provider's been registered, until told
            // otherwise.
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
            // Reads the now-decoupled mode flag directly, not set
            // emptiness -- see multiselect_mode_active's own doc
            // comment above for why that decoupling matters.
            let multiselect_active = multiselect_mode_active.get();
            // Tracked alongside the tile-building loop below rather than
            // as a separate pass over the same cards -- whether every
            // currently-visible selected card is already archived (for
            // the Archive/Unarchive button's own dynamic label) is
            // naturally available from the same iteration that's
            // already looking at each visible card's own state anyway.
            // Same reasoning for all_visible_selected_starred, added
            // alongside it for the Star button -- any_visible_selected
            // itself is shared between the two, since "is there at
            // least one visible-selected card at all" doesn't depend on
            // which status is being checked.
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

                // Right-click (secondary button) and long-press both
                // enter/toggle multiselect for this tile -- by request.
                // Both call the same toggle_card_selection helper
                // (defined below rebuild_grid) so the actual selection
                // logic lives in exactly one place regardless of which
                // gesture triggered it. Both also set
                // multiselect_mode_active true unconditionally --
                // harmless if it's already true (this is how the mode
                // actually turns on in the first place), and simpler
                // than branching on whether this is really "first
                // entry" versus "already active."
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
                // ~33% longer than the system default, by request, to
                // cut down on accidental triggers -- confirmed against
                // the real GTK4 API this is meant for exactly this:
                // delay_factor multiplies the default long-press
                // timeout, valid range [0.5, 2.0], not a workaround.
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
            // "Unarchive" only when there's at least one visible
            // selected card AND every one of them is already archived
            // -- any other case (including zero visible selected cards)
            // says "Archive," matching the rule as given: "if all
            // visible selected cards are already archived... In any
            // other case, it says Archive." Sets the tooltip, not the
            // label -- the button itself became icon-only (same
            // shoe-box-symbolic icon either way, matching how the
            // per-card archive toggle also keeps one icon and only
            // changes its own tooltip text between the two states).
            // Also sets active on the same condition -- matching the
            // per-card ToggleButton's own set_active(card.archived), so
            // this one visibly reads as pressed/on when it would
            // unarchive, unpressed/off otherwise, by request.
            let should_show_unarchive = any_visible_selected && all_visible_selected_archived;
            multiselect_archive_button.set_tooltip_text(Some(if should_show_unarchive {
                "Unarchive"
            } else {
                "Archive"
            }));
            multiselect_archive_button.set_active(should_show_unarchive);

            // Star's own version of the exact same logic just above --
            // "Unstar" only when there's at least one visible selected
            // card and every one of them is already starred, "Star" in
            // every other case, by request.
            let should_show_unstar = any_visible_selected && all_visible_selected_starred;
            multiselect_star_button.set_tooltip_text(Some(if should_show_unstar {
                "Unstar"
            } else {
                "Star"
            }));
            multiselect_star_button.set_active(should_show_unstar);

            // Checks the store's real card count, not the search
            // results above -- a search with no matches should still
            // show the (empty) grid, not this welcome page. This is
            // only for the case where there are no cards at all yet.
            let has_cards = !store_ref.cards.is_empty();
            grid_scroller.set_visible(has_cards);
            empty_status.set_visible(!has_cards);

            // Deferred, not set immediately -- same reasoning as the
            // other idle_add_local_once use elsewhere in this file:
            // grid_scroller's adjustment doesn't have correct bounds
            // for the newly-appended content until GTK has actually run
            // a layout pass over it, so setting the value synchronously
            // right here could clamp against the *old* bounds rather
            // than the new ones.
            let grid_scroller = grid_scroller.clone();
            glib::source::idle_add_local_once(move || {
                grid_scroller.vadjustment().set_value(saved_scroll_position);
            });
        }))
    };
    // rebuild_grid_handle's placeholder gets replaced here, now that
    // rebuild_grid itself actually exists -- every gesture handler
    // above that captured a clone of the handle sees this new content
    // from now on, since they only hold the handle (the Rc<RefCell<...>>
    // itself), not a snapshot of whatever it pointed to when they were
    // created.
    {
        let rebuild_grid = rebuild_grid.clone();
        *rebuild_grid_handle.borrow_mut() = Box::new(move || {
            (rebuild_grid.borrow())();
        });
    }

    // Only the Done button actually exits multiselect now, by
    // request -- Esc used to double as an exit as well (removed
    // entirely here, not just left disconnected), and deselecting the
    // last card used to exit implicitly back when mode was just "is
    // this set non-empty." Both of those were fine under that old
    // definition, but neither fits the new, deliberately-decoupled one
    // (see multiselect_mode_active's own doc comment) -- flagging this
    // explicitly in case Esc was actually meant to stay as an
    // equivalent shortcut for Done specifically, rather than go away
    // entirely.
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
            // Clears the selection only -- deliberately doesn't touch
            // multiselect_mode_active, unlike Done, so the mode stays
            // active and a fresh sub-selection can start right away
            // without needing to re-trigger right-click/long-press.
            // This is exactly the case the mode/selection decoupling
            // above exists to support.
            selected_card_ids.borrow_mut().clear();
            (rebuild_grid.borrow())();
        });
    }

    // Horizontal, single-line, scrolls sideways rather than wrapping --
    // deliberately a plain gtk::Box (not FlowBox, which wraps to
    // multiple rows -- right for the edit dialog's group picker above,
    // wrong for a tab strip that should stay one line) inside a
    // horizontal-only ScrolledWindow. Hidden entirely (not just an
    // empty "All"-only strip) until at least one group actually exists,
    // matching the edit dialog's own hint text for the same case.
    let group_tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // Center-aligned rather than left-justified by default -- halign
    // only repositions a widget within whatever space it's actually
    // *allocated*, and only takes effect when that allocation exceeds
    // the widget's own natural size. When every tab fits within the
    // scroller's viewport, there's slack space to center within; when
    // there isn't (the tabs are wider than the viewport, needing
    // scrolling), there's no slack to distribute and it just starts
    // from the left edge on its own -- both halves of "center when
    // there's room, left-justified otherwise" fall out of this one
    // property for free, no reactive width-checking logic needed.
    group_tabs_box.set_halign(gtk::Align::Center);
    group_tabs_box.set_margin_start(4);
    group_tabs_box.set_margin_end(4);
    group_tabs_box.set_margin_top(4);
    group_tabs_box.set_margin_bottom(4);
    // min/max_content_width still apply here regardless of scrollbar
    // policy -- Automatic (like External before it) keeps the
    // underlying scroll/clip capability active, which is the actual
    // thing these two properties need to have anything to enforce
    // against (Never was the one policy value that seemingly broke
    // that).
    //
    // Back to GTK's own native scrollbar (Automatic) instead of hiding
    // it -- four separate attempts at hand-rolling "keep the selected
    // tab visible" (grab_focus, single idle callback, polling for
    // non-zero bounds, polling for stable bounds) all failed to fully
    // solve it, which said more about the approach than about any one
    // bug. A real, always-there scrollbar solves "what's off-screen"
    // simply because it's not something being reimplemented -- it's the
    // same scrollbar GTK already knows how to keep in sync.
    //
    // window_placement(BottomLeft) is what actually moves it, though: GTK's
    // default (TopLeft) anchors the child's top-left corner in the
    // scrolled window's top-left, which is what pushes the horizontal
    // scrollbar to the *bottom* edge (the remaining edge) -- the actual
    // original complaint, since that's what was expanding on hover and
    // covering the tab row underneath it. BottomLeft anchors the
    // child's bottom-left corner at the bottom-left instead, which
    // frees up the *top* edge for the horizontal scrollbar instead.
    // Overlay scrolling itself is untouched here (still GTK's own
    // default), per the ask to try that first now that its expansion
    // has somewhere to go that isn't the tabs themselves.
    let group_tabs_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .window_placement(gtk::CornerType::BottomLeft)
        .hexpand(true)
        .min_content_width(0)
        .max_content_width(240)
        .child(&group_tabs_box)
        .build();

    // Real tab styling instead of the pill look (used deliberately for
    // the edit dialog's multi-select picker, but too visually heavy for
    // a filter strip) -- an underline under the checked tab, flat
    // otherwise. var(--accent-color) is libadwaita's own real, current
    // named-color syntax -- confirmed directly against the official
    // libadwaita CSS variables docs (which use exclusively hyphenated
    // var(--name) custom properties throughout), replacing an earlier
    // @accent_color reference (the older GTK3-era @define-color form)
    // that was never actually verified and, per a real, on-device
    // report of banner colors not applying at all, most likely doesn't
    // resolve in modern libadwaita at all.
    // Same total vertical padding as before (18px), just no longer
    // split evenly -- 13px top / 5px bottom shifts the text down
    // within the row rather than changing the row's overall height.
    register_css(
        ".group-tab { padding: 13px 10px 5px 10px; border-radius: 0; \
         border-bottom: 2px solid transparent; background: none; box-shadow: none; } \
         .group-tab:checked { border-bottom: 2px solid var(--accent-color); font-weight: bold; }",
    );

    // "<"/">" buttons are the intended way to move through the strip --
    // hscrollbar_policy(External) above suppresses GTK's own scrollbar
    // *widget* specifically (the overlay scrollbar was expanding on
    // hover and covering the bottom of the tabs), while leaving the
    // underlying scroll/clip capability itself active (needed for
    // max_content_width above to have anything to actually enforce
    // against). Touch/drag panning may still work as a side effect of
    // that -- not something this was designed around, just a
    // consequence of keeping scrolling functionally alive. Wired
    // further below, after rebuild_group_tabs exists, since jumping to
    // the next/previous *tab* (rather than paging by a fixed pixel
    // amount) needs to inspect the actual current tab order.
    let group_tabs_prev = gtk::Button::from_icon_name("go-previous-symbolic");
    group_tabs_prev.add_css_class("flat");
    let group_tabs_next = gtk::Button::from_icon_name("go-next-symbolic");
    group_tabs_next.add_css_class("flat");

    let group_tabs_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    group_tabs_row.append(&group_tabs_prev);
    group_tabs_row.append(&group_tabs_scroller);
    group_tabs_row.append(&group_tabs_next);
    group_tabs_row.set_visible(false);
    // insert_child_after rather than another sidebar_box.append() call,
    // since this widget is constructed well after search_entry/
    // grid_scroller/empty_status already got appended above -- this
    // places it in between them regardless of that declaration-order
    // difference (gtk::Box's append() would otherwise just tack it onto
    // the end, after grid_scroller/empty_status, which is the wrong
    // visual position).
    // References search_revealer, not search_entry directly -- since
    // search_entry was wrapped in a Revealer for the search-reveal
    // animation, it's no longer a direct child of sidebar_box itself
    // (search_revealer is); insert_child_after needs an actual direct
    // child as its sibling argument. This was pointing at search_entry
    // itself until now, which meant it likely wasn't finding a valid
    // sibling at all and silently failing to position (or insert)
    // group_tabs_row correctly -- the actual cause of group tabs not
    // showing up despite groups existing.
    sidebar_box.insert_child_after(&group_tabs_row, Some(&search_revealer));

    // Hides the </> paging buttons whenever the tab strip's content
    // already fits within the visible width on its own -- no point
    // offering to page through something that isn't scrolled at all.
    // Also disables each one individually right at whichever extreme
    // scrolling further that direction wouldn't do anything -- clicking
    // a paging button already at its extreme was reported to do
    // "unexpected things," so this makes that click impossible rather
    // than trying to make it safe. Driven by the adjustment's own
    // "upper"/"page-size"/"value" property-change notifications rather
    // than manually re-checking after every possible trigger (a new tab
    // added, a tab removed, the window resized, a paging click itself)
    // -- upper is the total content extent, page_size is the visible
    // extent, value is the current scroll position, and both paging and
    // sensitivity only depend on those three. This covers every one of
    // those triggers uniformly for free, since all of them change one
    // or more of these properties, rather than needing separate
    // handling per trigger.
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
        // Every invocation deferred via idle_add_local_once now, not
        // just the initial one -- deferring only the first call (tried
        // previously) turned out to be an incomplete fix: the warning
        // cascade was reported recurring repeatedly during normal use,
        // seconds to tens of seconds apart, not just once at startup.
        // rebuild_group_tabs destroys and recreates every tab button on
        // every selection change and every group add/rename/remove/
        // reorder -- each of those churns the scroller's content, which
        // can fire "upper"/"page-size" notify signals for intermediate,
        // not-yet-settled layout states along the way, not just the
        // final one. Reading the adjustment synchronously inside the
        // notify callback (as the reactive handlers below were doing)
        // risked acting on any of those, on every rebuild -- not just
        // the one case (construction time) the previous fix covered.
        // Deferring unconditionally avoids ever reading it before GTK's
        // queue goes idle and layout has actually settled, regardless
        // of what triggered the change.
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

    // Rebuilds the tab strip from scratch (same pattern as rebuild_grid
    // itself) -- needed whenever the groups list changes shape (add/
    // rename/remove/reorder via Manage Groups), not just once at
    // startup. set_group (not the builder's own .group() property,
    // whose exact Option-wrapping convention isn't verified here) links
    // each tab to "All" for GTK4's native mutual-exclusion behavior --
    // exactly one tab active at a time, no manual bookkeeping needed.
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
            // *current* archived/valid-only filters -- corrected from
            // an earlier version that checked against all cards
            // regardless of those filters, which was the wrong call on
            // reflection: this tab bar is a view into what's currently
            // shown, so it should track those two filters the same way
            // the grid itself does. Search text is deliberately excluded
            // from this, though -- passing an empty query to search()
            // below means only show_archived/show_valid_only actually
            // apply, not whatever's currently typed in the search box,
            // so groups don't flicker in and out of the tab bar while
            // someone's mid-search. Reuses CardStore::search directly
            // (with group_filter set to each group in turn) rather than
            // reimplementing its filtering rules separately, so this
            // stays consistent with whatever the grid itself considers
            // "visible" by construction, not by keeping two copies of
            // the same logic in sync by hand. Still shown in full on the
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
            // current_selection is extracted as its own statement
            // rather than borrowed directly in the if-let condition
            // below -- a real panic was reported here ("RefCell already
            // borrowed"), and the cause was that exact pattern: a
            // temporary Ref guard created in an if-let's scrutinee stays
            // alive for the *entire* if-let block in Rust, not just the
            // condition check, so the borrow_mut() a few lines later
            // (still inside that same block) was conflicting with the
            // still-alive immutable borrow from the condition. Pulling
            // the value out first, as its own complete statement, drops
            // that borrow immediately instead.
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
            // No manual "scroll the active tab into view" logic anymore
            // -- four separate attempts at that all failed to fully
            // solve it (see group_tabs_scroller's own comment on the
            // reasoning for switching back to a real, visible
            // scrollbar). GTK's own scrollbar keeps itself in sync with
            // scroll position on its own; nothing to reimplement here.
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
        // A second, separate clone specifically for use inside the
        // closure below, distinct from the one connect_clicked is
        // actually called on -- confirmed via a real E0505 compiler
        // error, not assumed: connect_clicked borrows its receiver for
        // the whole call expression, so a move closure built as part of
        // that same call can't also move the identical variable into
        // itself. Same fix as the earlier window/window_for_reopen case
        // a few stages back.
        let multiselect_archive_button_for_tooltip = multiselect_archive_button.clone();
        multiselect_archive_button.connect_clicked(move |_| {
            // The button's own current tooltip already says which
            // direction this click means -- kept in sync by rebuild_grid
            // every time the grid (and therefore the visible+selected
            // set it depends on) changes, so there's no need to
            // recompute that decision again here separately. Reads the
            // tooltip, not the label, now that the button is icon-only.
            let target_archived = multiselect_archive_button_for_tooltip.tooltip_text().as_deref() != Some("Unarchive");

            // See visible_selected_card_ids' own doc comment for why
            // this re-derives at click time rather than acting on the
            // full selection.
            let visible_selected_ids = visible_selected_card_ids(
                &store.borrow(),
                &search_entry.text(),
                show_archived.get(),
                show_valid_only.get(),
                *selected_group_filter.borrow(),
                &selected_card_ids.borrow(),
            );

            report_if_save_failed(&banner, store.borrow_mut().set_archived_bulk(&visible_selected_ids, target_archived));

            // Selection is deliberately left intact here -- reconsidered
            // from an earlier "clear after every action" default:
            // chaining several bulk actions against the same selection
            // (e.g. unarchive a mixed batch, then also add them to a
            // group) shouldn't require re-finding and re-selecting the
            // same cards after every single step. Only the "Done"
            // button actually exits multiselect now.
            (rebuild_grid.borrow())();
            // Same reasoning as the existing single-card archive
            // toggle's own call to this: archiving/unarchiving changes
            // whether a card counts as visible under the current
            // show_archived filter, which group tabs' own filtering
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
            // Star's own version of Archive's click handler just above,
            // mirrored exactly -- see that one's own comments for the
            // fuller reasoning on each piece (reading the tooltip for
            // direction, re-deriving the visible-selected set at click
            // time, leaving the selection intact afterward). One real
            // difference: no rebuild_group_tabs call here -- starring
            // doesn't change whether a card counts as visible under any
            // filter the way archiving does, so there's nothing for the
            // group tabs to need refreshing over.
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
            // Same visible-selected re-derivation as every other bulk
            // action's own handler -- see visible_selected_card_ids'
            // own doc comment.
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
                // the whole selection -- any other, hidden-but-still-
                // selected card (not visible under the current filters
                // when this fired) was never touched by this action at
                // all, so it should stay selected, same "only act on
                // what's visible" philosophy as everywhere else in this
                // feature.
                let mut ids = selected_card_ids.borrow_mut();
                for id in &visible_selected_ids {
                    ids.remove(id);
                }
                drop(ids);
                (rebuild_grid.borrow())();
                // A deleted card could have been the last one in some
                // group, same reasoning as archiving's own call to this.
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
            // Same visible-selected re-derivation as every other bulk
            // action's own handler -- see visible_selected_card_ids'
            // own doc comment.
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

            // Separate clones for the direct arguments below versus
            // on_applied's own closure -- passing the same variable
            // both ways in one call is exactly the borrow/move conflict
            // this file now specifically guards against.
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
                    // A group-membership change could make some group's own
                    // tab appear or disappear (rebuild_group_tabs only shows
                    // tabs for groups with at least one currently-visible
                    // card), same reasoning as archiving's/deleting's own
                    // calls to this.
                    (rebuild_group_tabs.borrow())();
                }),
            );
        });
    }


    // Pixel-based paging again, not tab-jumping -- the tab-jumping
    // version (with its own manual "keep the selected tab visible"
    // logic) is what led into the whole centering problem in the first
    // place; simple paging doesn't need to know where any particular
    // tab is, just move the scroll position, so it has nothing to get
    // wrong the way the centering math did. Still wraps at the ends
    // (right past the last tab goes back to the start, left past the
    // start goes to the end) in the spirit of the original ask, just as
    // a scroll-position wrap rather than a specific-tab jump.
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
        let brightness_button = brightness_button.clone();
        let name_label_css = name_label_css.clone();

        Rc::new(RefCell::new(move || {
            (rebuild_grid.borrow())();
            // A card add/delete/edit that reaches refresh() (rather than
            // the on_saved path, which bypasses refresh to keep the
            // detail view open -- rebuild_group_tabs is called
            // separately there, see its own comment) can just as easily
            // have changed which groups are empty/non-empty under the
            // current filters, so this needs rebuilding here too, not
            // just the grid.
            (rebuild_group_tabs.borrow())();

            content_header.set_title_widget(Some(&adw::WindowTitle::new("", "")));
            // Neutral (no background/text override) rather than leaving
            // the last-viewed card's color on a header with no card
            // showing.
            name_label_css.load_from_string("");
            content_toolbar.set_content(Some(&detail_status));
            edit_button.set_visible(false);
            info_button.set_visible(false);
            info_button.set_popover(gtk::Popover::NONE);
            bottom_bar.set_visible(false);
            // Disconnect the previous card's handler before resetting --
            // both are ToggleButtons now (their pressed/depressed look
            // is what shows starred/archived state), reconnected fresh
            // per-card, same reasoning as brightness_button below for
            // why disconnecting first matters: set_active() fires
            // "toggled" on programmatic changes too, not just clicks.
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
            brightness_button.set_active(false);
            brightness_button.set_visible(false);
            let _ = (&split_view, &content_page);
        }))
    };

    // Expired-card housekeeping: once now (before the very first render,
    // so a card that already expired while the app was closed doesn't
    // flash on screen unarchived/undeleted for a moment first), and
    // again at every local midnight while the app stays open -- see
    // schedule_midnight_expiration_check below. Both now surface a save
    // failure via the shared banner, matching the "<action> failed:
    // {e}" wording already used for export/import failures elsewhere in
    // this file -- confirmed safe to do here specifically by directly
    // tracing execution order, not just checking that banner itself was
    // already constructed (an earlier version of this comment stopped
    // there): the window itself is already built with toast_overlay as
    // its own content by this point (line 689), and both banner
    // instances are already parented into sidebar_toolbar/content_toolbar
    // via build_sidebar_headerbar/build_detail_pane, called at lines
    // 960/987 -- both well before this call. GTK doesn't require a
    // widget's full parent chain to finish assembling before a property
    // like `revealed` can be set on it, so this correctly persists and
    // shows once the window is eventually presented in main.rs, however
    // much further widget-tree assembly happens between here and then.
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

    // Called directly, no deferral needed -- rebuild_grid doesn't
    // measure or depend on anything about the window's current size.
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
        let brightness_button = brightness_button.clone();
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
        // child-activated fires on every activation (and, with
        // activate-on-single-click set above, on every tap) rather than
        // only when the selection changes -- tapping the already-
        // selected tile again wouldn't otherwise do anything.
        flow_box.connect_child_activated(move |_, child| {
            let index = child.index();
            let store_ref = store.borrow();
            // Matches the query (and show_archived/show_valid_only/
            // group-filter state) rebuild_grid actually filtered
            // flow_box by, rather than always the unfiltered list --
            // searching for something, then tapping a result, was
            // mapping this index against the *full* card list instead
            // of the filtered one actually on screen, landing on the
            // wrong card whenever a search filter was narrowing the
            // grid. Same class of bug applies to any of these filters
            // if not matched here too.
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

            // While multiselect is active, a plain click/tap toggles
            // this card's own selection instead of opening it -- by
            // request. Checked before, not after, dropping into
            // show_card_detail below, so an already-active multiselect
            // session never accidentally opens a card's detail view on
            // a plain click. Reads the decoupled mode flag now, not set
            // emptiness -- see multiselect_mode_active's own doc
            // comment for why.
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
                &brightness_button,
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
                        // Not report_if_save_failed -- that helper's own
                        // wording ("your change may not have been
                        // saved") is specifically about a CardStore
                        // mutation's own save, which would misleadingly
                        // suggest the user's actual card data is at
                        // risk here. It isn't: export_to only ever
                        // writes to this separate, user-chosen CSV
                        // file, never touching cards.json at all, so a
                        // failure here can't lose or corrupt anything
                        // the user already has -- only the export
                        // itself didn't happen.
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
                // Auto-detects a Barkeep export, a Catima backup
                // (.zip or bare .csv), or a LoyaltyCardKeychain .csv --
                // no separate menu item needed per format.
                //
                // Computed into `result` as its own statement rather than
                // `match store.borrow_mut().import_any(&path) { ... }` --
                // the RefMut from `borrow_mut()` would otherwise stay
                // alive for the whole match (a temporary in a match
                // scrutinee lives until the match ends), and the Ok arm
                // below calls `refresh()`, which itself calls
                // `store.borrow()` to rebuild the grid. Two live borrows
                // of the same RefCell -- one mutable -- panics. Ending
                // the statement here drops the RefMut before any arm
                // runs.
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
                        // This importer's own last-resort fallback found
                        // something, but nothing it's confident enough
                        // about to import automatically -- confirmed
                        // with the user first, exact wording already
                        // decided for this. Not Destructive styling
                        // (that's for confirm_delete's own, genuinely
                        // destructive case) -- Suggested instead, since
                        // attempting this import isn't destructive at
                        // all. See confirm_action's own doc comment for
                        // why this goes through that shared helper now
                        // rather than its own separate dialog
                        // construction. Still a modal AdwAlertDialog,
                        // not a banner -- this needs a real yes/no
                        // decision before proceeding, unlike the
                        // notifications below it that just report
                        // something that already happened.
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

    // -----------------------------------------------------------------
    // NOTE ON CONFIDENCE: same caveat as barcode.rs/camera.rs/brightness.rs
    // -- AdwAboutDialog is new (libadwaita 1.5+, gated behind this crate's
    // "v1_5" feature, which Cargo.toml already enables), so its exact Rust
    // builder surface below is a best-effort match to the C API docs
    // (application-icon, application-name, version, license-type,
    // developer-name, website, add_link()), not a compiled/verified call.
    // If `cargo build` errors here, check docs.rs/libadwaita for the
    // current AboutDialog API -- the fix should be renamed calls, not a
    // redesign.
    //
    // developer_name is a guess -- "AJ" pulled from the URLs you gave
    // (dev.dunevent.net/aj, /ajdunevent, @aj) since no name was given
    // directly; change it if you'd rather show something else there.
    // -----------------------------------------------------------------
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

    // Stateful (boolean) action -- gio::Menu renders this as a checkbox
    // item automatically because of the boolean state, no separate
    // widget needed. show_archived (the plain Cell rebuild_grid and the
    // tile-tap handler above both already read from) is the actual
    // source of truth; this action's own state is kept in sync with it
    // so the menu checkbox reflects reality, but nothing else reads the
    // action's state back out.
    // This action's own boolean state represents "hide archived cards"
    // (matching the menu's new "Hide Archived Cards" wording) -- the
    // inverse of what the internal show_archived Cell means (true =
    // shown). Rather than rename show_archived and everywhere it's
    // threaded through (rebuild_grid, the tile-tap handler, etc.), only
    // the mapping here is inverted: checking this box sets
    // show_archived to false, not true. Defaults to checked (hidden),
    // matching show_archived's own existing default of false -- neither
    // the Cell's initial value nor its meaning needed to change, only
    // this action's own checkbox-facing state and label did.
    // Initial checkbox state now comes from ui_state (persisted across
    // runs), not a hardcoded true -- see ui_state's own doc comment
    // above for the loading/inversion details.
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

    // Surfaced now that the window actually exists to present a
    // notification on -- see CardStore::open's own doc comment for what
    // these mean and why they're never silent. join (not just the first
    // element) since Vec<String> is the general shape
    // read_json_with_backup_on_corruption returns, even though in
    // practice there's only ever at most one warning now that cards and
    // groups share a single file. Warning severity, by request -- this
    // is reporting that something already went wrong (data corruption)
    // even though it was already handled automatically, distinct from
    // the error severity used for something currently, actively
    // failing.
    if !startup_warnings.is_empty() {
        show_banner(&banner, BannerSeverity::Warning, &startup_warnings.join("\n\n"));
    }

    window
}

/// Add/rename/remove/reorder groups. Reorder is Up/Down buttons per
/// row, not true drag-and-drop -- a deliberate, lower-risk choice given
/// how many GTK API guesses have needed correction over the course of
/// this app so far; index-swap-and-rebuild is something I'm actually
/// confident about, unlike wiring up GtkDragSource/GtkDropTarget
/// correctly on the first try. Fully satisfies "rearrange order" as a
/// result, just via simpler mechanics.
/// Which bulk state a group is currently in, relative to the visible-
/// selected cards open_change_groups_dialog was opened for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupBulkState {
    Full,
    Partial,
    Off,
}

/// The next state a click on a group's own pill should move it to,
/// given both its ORIGINAL state (computed once, when the dialog
/// opened, never changes afterward) and its CURRENT state (what's
/// actually displayed right now, changed by every click). A group that
/// started Full or Off just toggles between the two, Partial never
/// involved. A group that started Partial cycles Partial -> Off ->
/// Full -> back to Partial -- a third click resets to the original
/// partial state rather than continuing to toggle Full/Off forever, by
/// request.
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
            // A Full/Off-original group's current state should never
            // actually reach Partial at all -- the cycle above only
            // ever produces Partial for a group whose own original was
            // Partial to begin with. Falls back to original defensively
            // rather than panicking if this invariant somehow doesn't
            // hold.
            GroupBulkState::Partial => original,
        },
    }
}

/// The CSS class matching a given bulk state -- see the tri-state pill
/// CSS built in open_change_groups_dialog itself.
fn group_bulk_css_class(state: GroupBulkState) -> &'static str {
    match state {
        GroupBulkState::Full => "pill-full",
        GroupBulkState::Partial => "pill-partial",
        GroupBulkState::Off => "pill-off",
    }
}

/// Computes a group's initial bulk state from how many of the given
/// cards are currently its members -- Off if none are, Full if every
/// one is, Partial for anything in between.
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

/// The "Change Groups" bulk-editing dialog, opened from the multiselect
/// action bar's own Groups button. Shows every group as a tri-state
/// pill (Full/Partial/Off, see GroupBulkState) reflecting the
/// visible-selected cards' current membership, lets each group's own
/// target state be changed independently (see next_group_bulk_state for
/// the click-cycling rule), and on Apply, changes only the groups whose
/// final state actually differs from "leave alone": Full adds the group
/// to every visible-selected card that doesn't already have it, Off
/// removes it from every one that does, and Partial (whether never
/// touched, or cycled back to its own original value) means this
/// group's own membership is left completely untouched for every card
/// -- genuinely skipped, not narrowed down to "the ones that don't
/// already match."
fn open_change_groups_dialog(
    window: &adw::ApplicationWindow,
    banner: &SharedBanner,
    store: Rc<RefCell<CardStore>>,
    visible_selected_ids: Vec<Uuid>,
    rebuild_group_tabs: Rc<RefCell<dyn Fn()>>,
    rebuild_grid: Rc<RefCell<dyn Fn()>>,
    selected_group_filter: Rc<RefCell<Option<Uuid>>>,
    // Carries the user's own in-progress tri-state choices across a
    // detour through Manage Groups (see the "Manage Groups" link below)
    // -- None for a fresh open (every group starts at its real computed
    // state), Some for a reopen after managing groups, where whatever
    // wasn't touched should look untouched, not reset back to square
    // one just because the user briefly left to rename or add a group.
    preserved_states: Option<HashMap<Uuid, (GroupBulkState, GroupBulkState)>>,
    // Box<dyn Fn()>, not impl Fn() + 'static -- needed specifically
    // because this function calls itself (see the "Manage Groups" link
    // below): with a generic parameter, each self-call would force a
    // new monomorphization of the whole function for whatever new
    // closure type gets passed, and since that new instantiation's own
    // body contains another call to itself, the type genuinely never
    // stops growing -- no recursion_limit bump fixes an unbounded
    // generic chain like that, only breaking the genericness does. Same
    // reasoning as on_saved/on_closed elsewhere in this file, both
    // already Box<dyn Fn(...)> for the same kind of reopening-callback
    // pattern.
    on_applied: Box<dyn Fn()>,
) {
    let cards: Vec<Card> = store
        .borrow()
        .cards
        .iter()
        .filter(|c| visible_selected_ids.contains(&c.id))
        .cloned()
        .collect();

    // (original, current) per group -- original never changes once
    // computed here; current is what's actually displayed and
    // eventually applied, mutated by each pill's own click handler
    // below. Built by iterating the *current* group list, not the old
    // preserved_states map directly -- a group deleted via Manage
    // Groups since preserved_states was captured naturally never
    // appears here at all, and a newly-added one naturally gets a
    // freshly-computed entry, without needing separate reconciliation
    // logic for either case.
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

    // Same .compact-pill base as the Add/Edit screen's own group picker
    // -- no background-color override for the unchecked state anymore
    // (reconsidered by request): the contrast problem an earlier
    // card-color attempt ran into was specifically that #ffffff
    // read as nearly identical to the dialog's own window-background
    // background, not that GTK's stock .pill styling itself was
    // indistinguishable from its surroundings -- letting GTK's own
    // native .pill treatment (border/shadow, not just a flat color)
    // handle that contrast natively, in both light and dark mode, sidesteps
    // that problem rather than needing a third color choice to solve
    // it. Selected/unselected contrast is a separate concern, already
    // solved independently by pill-full/pill-partial below overriding
    // the *selected* state to the accent color regardless of whatever
    // the unselected background actually is. Two new classes for the
    // two states this picker never needed: pill-full (the same
    // var(--accent-bg-color)/var(--accent-fg-color) as that picker's
    // own :checked state, just reached via an explicit class here
    // instead of GTK's native checked pseudo-class, since a tri-state
    // cycle doesn't map onto ToggleButton's binary checked/unchecked at
    // all) and pill-partial (alpha(var(--accent-bg-color), 0.5) --
    // GTK4's own native CSS function for partial opacity of a color,
    // including a theme variable, without needing to know its actual
    // resolved hex value at all). Hyphenated var(--name) form
    // throughout -- confirmed directly against the official libadwaita
    // CSS variables docs as the real, current syntax, replacing an
    // earlier @name reference (the older GTK3-era @define-color form)
    // that was never actually verified.
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

    // A plain Button, not a ToggleButton like the Add/Edit screen's own
    // pills -- deliberately: this is a tri-state cycle, not a binary
    // toggle, so the visual state here is fully, explicitly managed via
    // CSS class add/remove on every click rather than leaning on GTK's
    // own checked pseudo-class at all.
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

    // Below the pills, by request -- opens Manage Groups (create/
    // rename/delete groups themselves, a different action from this
    // dialog's own "change which groups these cards belong to") with
    // the current in-progress tri-state choices preserved across the
    // trip, via preserved_states above. Default (non-flat) styling, by
    // request -- matching the Add/Edit screen's own "Manage Groups"
    // button there, which never had flat styling of its own to begin
    // with (this one previously did, despite an earlier version of
    // this comment claiming they already matched).
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
            // Only groups whose final state isn't Partial actually
            // become a change here -- a still-Partial group (never
            // touched, or cycled back to its own original value) is
            // deliberately left out of this list entirely, which is
            // exactly what makes CardStore::apply_group_changes_bulk
            // leave it untouched for every card, per this whole
            // dialog's own "Partial means genuinely skipped" design.
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
            // Snapshotting is just cloning the current HashMap -- no
            // save/restore mechanism needed, since it never leaves
            // memory in the first place.
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

    // Explicitly focused rather than left to GTK's own default (the
    // first focusable widget, which would have been the first group
    // pill) -- by request: a focus ring around the first pill was
    // reading as "this group is selected," which it wasn't. Apply is
    // this dialog's own primary action, a reasonable, conventional
    // place to land focus instead.
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
    // Fired once this dialog actually closes, regardless of how (its own
    // close button, Esc, etc.) -- lets a caller do something once the
    // user is done managing groups, without this function needing to
    // know or care what that something is. None for every existing call
    // site that doesn't need this (the main menu's own entry point,
    // which has nothing to return to).
    on_closed: Option<Box<dyn Fn()>>,
) {
    let list_box = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();

    // Self-referential rebuild closure (each row's buttons need to call
    // this same closure again after mutating the store) -- Box<dyn Fn()>
    // rather than this app's usual bare `dyn Fn()` specifically because
    // this one needs to be *reassigned* after construction, which needs
    // Box's fixed-size indirection; a bare `dyn Fn()` behind a RefCell
    // can be called but not swapped for a different underlying closure
    // later. Placeholder no-op built first, then replaced with the real
    // closure once it exists -- the closures captured into each row's
    // buttons hold a clone of the *handle*, so they see whatever the
    // RefCell's content actually is at call time, not whatever it was
    // when they were created.
    let rebuild_rows: Rc<RefCell<Box<dyn Fn()>>> = Rc::new(RefCell::new(Box::new(|| {})));
    {
        let rebuild_rows_handle = rebuild_rows.clone();
        let store = store.clone();
        let list_box = list_box.clone();
        let rebuild_group_tabs = rebuild_group_tabs.clone();
        let rebuild_grid = rebuild_grid.clone();
        let selected_group_filter = selected_group_filter.clone();
        // Captured here, at the outer level, specifically so the 5
        // inner `let banner = banner.clone();` lines deep in this
        // closure's own body (one per row button) clone from *this*
        // owned value, not the borrowed &adw::Banner function parameter
        // -- Box<dyn Fn()> has an implicit 'static bound, and this
        // closure was transitively capturing that borrowed parameter
        // through those nested clones (move closures capture everything
        // referenced anywhere in their body), which doesn't satisfy it.
        // Confirmed via a real compiler error ("lifetime may not live
        // long enough... must outlive 'static"), not assumed. window
        // needed this same treatment too, once -- removed once its own
        // 5 nested clones were switched from window to banner (report_if_save_failed
        // now takes banner, not window), which left this own outer
        // capture genuinely unused; caught by a real compiler warning
        // ("unused variable: `window`"), not just assumed dead.
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
                        // The group that was just deleted might be the
                        // one currently narrowing the main grid -- if
                        // so, fall back to "All" rather than leaving the
                        // filter pointing at a group that no longer
                        // exists.
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
    // Computed before on_closed gets moved into dialog.connect_closed
    // below -- once moved, it can't be inspected again out here.
    let opened_from_edit_dialog = on_closed.is_some();
    // A back chevron on the left instead of the default "X" close
    // button on the right, specifically when opened from the Add/Edit
    // screen -- by request, since "back to the card I was editing"
    // reads more naturally as backward navigation than a plain close
    // action in that context. Reuses on_closed.is_some() as the signal
    // for this rather than a separate, redundant parameter -- it's
    // already exactly the fact that distinguishes the two call sites
    // (Some only when opened from Add/Edit's own Manage Groups button).
    // The button's own click handler is attached later, once dialog
    // itself exists (see the construct-early-attach-late pattern
    // save_button/manage_groups_button already use in open_edit_dialog).
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

/// Two AdwBanner instances, kept in sync by show_banner below -- one
/// living in sidebar_toolbar, one in content_toolbar, each as a second
/// top bar right below that page's own AdwHeaderBar (see
/// build_sidebar_headerbar/build_detail_pane). Necessary because a
/// single GTK widget can only ever have one parent at a time, and this
/// app's own NavigationSplitView is permanently collapsed (single-pane,
/// stack-navigation style) -- only one of its two pages is ever
/// actually visible, but a notification could be triggered while either
/// one is showing, so there's no single place to put one shared banner
/// that would be positioned correctly (just below the current
/// headerbar) regardless of which page that turns out to be. A banner
/// placed above the whole split view instead (an earlier version of
/// this) sat above both pages' own headerbars, not just below whichever
/// one was showing -- confirmed wrong by direct observation, not just a
/// stylistic guess. Both instances are always updated together, so
/// whichever one happens to be visible always reflects the current
/// notification, and the other stays ready in case the user navigates
/// to it while the notification is still up.
#[derive(Clone)]
struct SharedBanner {
    sidebar: adw::Banner,
    content: adw::Banner,
    // Loaded fresh each time show_banner runs, targeting
    // #barkeep-banner-sidebar/#barkeep-banner-content by ID rather
    // than a class -- see show_banner's own doc comment for why.
    css_provider: gtk::CssProvider,
}

/// Severity determines which of the three CSS classes registered in
/// build_split_view_and_window colors the banner -- see that function's
/// own doc comment for the actual color values (the official
/// libadwaita success/warning/error bg/fg color pairs, not invented
/// ones).
enum BannerSeverity {
    Error,
    Success,
    Warning,
}

/// The one shared notification surface for the whole app -- replaces
/// every notification previously shown as a modal AdwAlertDialog (via
/// show_message) with this single, persistent, non-modal AdwBanner
/// instead. Reasoning from this session's own discussion: a modal
/// dialog blocks the rest of the app until dismissed, which is a
/// mismatch for anything not directly triggered by the user's own,
/// immediate tap (most notably the midnight expiration check, which
/// fires from an unattended background timer) -- and even for the
/// directly-triggered cases (save failures, import results, camera/
/// scan errors), a banner's non-modal, stays-until-acknowledged nature
/// fits equally well without that downside. There's deliberately no
/// separate "Retry" action on any banner, even for save/export
/// failures -- see mutate_then_save's own doc comment in card.rs: a
/// failed save now means the underlying mutation never applied at all,
/// so re-doing the same UI action the user already knows how to reach
/// (tap the same button again) already is the retry, without this
/// needing its own dedicated action. Updates both of SharedBanner's own
/// instances identically -- see that struct's own doc comment for why
/// there are two. Each one's own button is wired once, at construction
/// (see build_sidebar_headerbar/build_detail_pane), to just dismiss
/// that instance -- not reconnected per-message the way per-card
/// handler slots like star_handler are, since every message this shows
/// uses that exact same acknowledge-and-dismiss action.
fn show_banner(banner: &SharedBanner, severity: BannerSeverity, message: &str) {
    // ID selectors (#barkeep-banner-sidebar/#barkeep-banner-content,
    // set once on the two SharedBanner instances at construction), not
    // classes -- .banner-error/etc, an earlier version of this file's
    // own approach, never visibly applied, even paired with !important
    // (itself later confirmed a hard parse error in GTK's own CSS
    // parser, via a real GTK-WARNING on a real run). This file already
    // had the actual answer, in a pre-existing comment on the card-tile
    // code: "an ID selector is specific enough to beat FlowBoxChild's
    // theme rules on its own, without !important" -- the same, already-
    // proven pattern applied here too, via SharedBanner's own
    // css_provider, reloaded fresh each call the same way
    // update_header_style's own provider already is for the detail
    // header. libadwaita's own real, current named-color variables
    // (var(--error-bg-color) etc, confirmed against the official docs
    // earlier this session) -- not a hex value guessed at.
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

/// Surfaces a save failure to the user instead of silently discarding
/// it, which is what every CardStore mutation site in this file used to
/// do (just `.ok()`) -- a failed disk write (disk full, permissions
/// changed mid-session, etc.) would leave the in-memory state changed,
/// the UI showing success, and the change silently lost the next time
/// the app restarts. Generic over both the Ok type (callers don't need
/// the success value here, only whether it failed) and the error type
/// (bounded by Display, not fixed to String) -- most CardStore mutation
/// methods return the raw io::Error straight from self.save(), while a
/// handful of others (the import-related ones) already convert to
/// String themselves; this covers both without needing to know or care
/// which one a given call site's method happens to return. Just one
/// version now, unlike the window-based show_message this replaced,
/// which needed a separate _weak variant for show_card_detail's own
/// handlers -- the banner is a persistent child widget for the whole
/// app's lifetime, not something that could be independently destroyed
/// out from under a still-live closure the way the window itself could
/// be, so every caller can just pass it directly.
fn report_if_save_failed<T, E: std::fmt::Display>(banner: &SharedBanner, result: Result<T, E>) {
    if let Err(e) = result {
        show_banner(banner, BannerSeverity::Error, &format!("Your change may not have been saved: {e}"));
    }
}

/// Disconnects whichever signal handler is currently stored in
/// `handler` (if any) from `widget`, and clears the slot. Extracted
/// from 11 separate, identically-shaped sites across refresh and
/// show_card_detail -- each one previously repeated this same two-line
/// disconnect-if-present check by hand, once per per-card
/// button/action (star, archive, copy-code, copy-image, link, edit).
/// Only the disconnect half is unified here, not the reconnect half
/// that follows it at 6 of those 11 sites (in show_card_detail) --
/// each button's own reconnect closure is genuinely different signal
/// (toggled vs clicked vs activate) and body, so there's no single
/// signature that could cover all of them the way this one covers
/// every disconnect. Generic over W: IsA<glib::Object> -- confirmed
/// against glib's own ObjectExt trait definition (disconnect's real
/// home) rather than guessed, since gtk::ToggleButton, gtk::Button,
/// and gio::SimpleAction all need to work here and don't share any
/// more specific common trait.
fn disconnect_if_present<W: glib::object::IsA<glib::Object>>(
    widget: &W,
    handler: &Rc<RefCell<Option<glib::SignalHandlerId>>>,
) {
    if let Some(id) = handler.borrow_mut().take() {
        widget.disconnect(id);
    }
}

/// Creates a CssProvider, loads `css` into it, and registers it against
/// the default display at application priority -- the common shape for
/// a one-time, static CSS provider that's registered once and never
/// touched again (confirmed none of its 3 call sites ever read the
/// provider back afterward, so this returns nothing rather than a
/// value nobody would use). Not used for name_label_css (registered
/// empty, then loaded repeatedly as the selected card changes) or
/// build_card_tile's own per-card provider cache (wrapped in reuse
/// logic keyed by card id) -- both have genuinely different lifecycle
/// needs from the "register once, forget about it" shape this covers.
fn register_css(css: &str) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    // PRIORITY_USER, not PRIORITY_APPLICATION (an earlier version of
    // this function used) -- confirmed directly relevant, not a guess:
    // a real developer's own first-hand account on GNOME Discourse
    // describes exactly this problem ("I find it impossible to style a
    // GtkButton properly without using PRIORITY_APPLICATION. Something
    // from either libadwaita or GTK... overrides several properties
    // that I am trying to declare"), and the official docs confirm
    // PRIORITY_USER is explicitly the sanctioned highest priority an
    // application should use ("you should not use priorities higher
    // than this, to give the user the last word") -- one level above
    // PRIORITY_APPLICATION, specifically for exactly this kind of
    // override. This is very likely why CSS confirmed correct when
    // pasted directly into GTK Inspector's own live CSS editor (which
    // very likely applies at this same, higher priority) didn't
    // actually take effect once compiled into this app's own
    // PRIORITY_APPLICATION-level provider -- both the header/banner
    // padding fix and the banner color fix go through this same
    // function, so this one change addresses both at once if this
    // diagnosis is right.
    gtk::style_context_add_provider_for_display(
        &default_display(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );
}

/// Shared by every "confirm before doing something, then do it" dialog
/// in this file -- previously this exact response/appearance/default/
/// close-response wiring was duplicated across bulk delete, single-card
/// delete, and the unrecognized-import-format prompt, with only the
/// heading/body, the action button's own label, its styling, and what
/// happens on confirm actually differing between them. The response id
/// itself is always "confirm" internally -- opaque, never shown to the
/// user (only action_label is), so there's nothing for a caller to
/// customize there.
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

/// Confirms before deleting -- worth the extra tap now that Delete lives
/// inside a menu item instead of its own dedicated (harder to mis-tap)
/// button. A thin wrapper over confirm_action with Delete/Destructive
/// fixed, rather than its own separate dialog-construction copy, so its
/// two existing callers (bulk delete, single-card delete) keep their
/// same simple signature (no action_label/appearance to pass) while the
/// underlying dialog logic itself is unified into one place.
/// Generalized to take a caller-built heading/body rather than just a
/// single card name -- bulk delete (below) needs a genuinely different
/// message ("This will permanently delete 5 cards." vs a specific
/// name).
fn confirm_delete(window: &adw::ApplicationWindow, heading: &str, body: &str, on_confirm: impl Fn() + 'static) {
    confirm_action(window, heading, body, "Delete", adw::ResponseAppearance::Destructive, on_confirm);
}

/// Colors the whole detail-view header to match the selected card, via
/// the same id-selector CSS approach this app already used successfully
/// for the old card-name label (#barkeep-detail-header is set once on
/// content_header itself at construction; only this rule's content
/// changes per-card). Also colors the detail view's body
/// (#barkeep-card-body, set fresh on vbox each time show_card_detail
/// runs, since vbox itself is rebuilt from scratch every card switch)
/// with a top-to-bottom gradient from the same card color into the
/// theme's own neutral background -- reuses this same provider/function
/// rather than threading a second CssProvider through every signature
/// this one's already threaded through, for one more rule that happens
/// to want the same "recompute whenever the card's color is known"
/// timing. var(--window-bg-color) is libadwaita's own real, current
/// named-color syntax -- confirmed directly against the official
/// libadwaita CSS variables docs, replacing an earlier @window_bg_color
/// reference (the older GTK3-era @define-color form) that was flagged
/// as unverified when first written and, per a real, on-device report
/// of white/black bands appearing in exactly this header/body area,
/// most likely never actually resolved at all in modern libadwaita.
///
/// :backdrop (window loses focus): two earlier attempts didn't pan out
/// -- first, trying to cancel GTK's own header dimming outright (tested,
/// confirmed not to work); then, computing a matching dimmed color for
/// the body gradient from a flat "reduce every channel by the same
/// percentage" guess. That second one was checked against 4 real
/// before/after color pairs the person captured directly from their own
/// desktop, in both light and dark style -- light mode actually fits a
/// clean, consistent formula (blend toward white by ~48%, error well
/// under 5% across all 4 samples), but dark mode does not fit any
/// blend-toward-a-fixed-color formula tried, with real outliers no
/// single number explains. Even the working light formula would need
/// runtime light/dark detection to apply correctly, which stacks
/// another unverified mechanism on top of an already-uncertain one. Per
/// the person's own explicit fallback for exactly this situation: drop
/// the gradient under :backdrop rather than guess -- a flat
/// var(--window-bg-color) instead, no color-matching attempted.
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

/// Today's date as "YYYY-MM-DD" in local time -- matches the format
/// Card's own valid_from/expiry fields use, so a plain string comparison
/// against either is a correct "has this happened yet" check. Empty
/// string on any failure; CardStore::apply_expiration_actions treats an
/// empty `today` as "don't act on anything" rather than risk a wrong
/// comparison from a malformed date.
/// Parses a "YYYY-MM-DD" string into a glib::DateTime at local midnight,
/// for handing to gtk::Calendar::select_day. None on anything malformed
/// -- callers just leave the calendar on its default selection rather
/// than treating this as an error.
fn parse_iso_date(s: &str) -> Option<glib::DateTime> {
    let mut parts = s.trim().splitn(3, '-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: i32 = parts.next()?.parse().ok()?;
    let day: i32 = parts.next()?.parse().ok()?;
    glib::DateTime::new(&glib::TimeZone::local(), year, month, day, 0, 0, 0.0).ok()
}

fn today_iso() -> String {
    glib::DateTime::now_local()
        .and_then(|d| d.format("%Y-%m-%d"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Re-derives the currently visible+selected card ids at the moment a
/// bulk action actually fires, rather than acting on the full (possibly
/// filter-hidden) selection -- by request: only whatever's actually
/// visible under the current search/filters when the action fires gets
/// acted on, so a selection hidden by a filter change can't be silently
/// swept into it. Same query inputs rebuild_grid itself uses, so
/// "visible" here means the same thing it means on screen. Shared by
/// every bulk-action handler (Archive/Star/Delete/Groups) -- previously
/// this exact block was duplicated verbatim four times.
/// The display every CSS provider in this file gets registered against
/// -- previously this exact "get the default display or panic" call was
/// duplicated six separate times. A GTK4 app already requires a display
/// to run at all, so if this ever actually panicked, the app would have
/// failed to even construct its main window long before reaching any
/// of this file's own CSS registration.
fn default_display() -> gtk::gdk::Display {
    gtk::gdk::Display::default().expect("no display")
}

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
/// today's own midnight explicitly (year/month/day at 00:00:00) rather
/// than something like "round up to the next day", so this is correct
/// even if called *at* midnight exactly. Falls back to a flat 24 hours
/// on any failure -- still checks daily, just not necessarily aligned to
/// the actual midnight boundary in that unlikely case.
///
/// now_local()/.format() are already proven working elsewhere in this
/// file (the export filename uses them); .year()/.month()/
/// .day_of_month()/glib::DateTime::new()/glib::TimeZone::local()/
/// .add_days()/.to_unix() aren't used anywhere else here, so treat
/// those specifically with the same confidence caveat as this app's
/// other less-common API calls. (new_local() was tried first and
/// doesn't actually exist -- confirmed by a real compile error -- fixed
/// to new() with an explicit TimeZone::local(), which is what the
/// compiler's own error pointed at.)
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
/// open, rescheduling itself for the following midnight each time it
/// runs -- rather than a single repeating fixed-interval timer, which
/// would drift out of alignment with the actual day boundary over time.
/// A save failure here now surfaces via the app's own shared banner
/// (BannerSeverity::Error, matching the "<action> failed: {e}" wording
/// already used for export/import failures elsewhere in this file) --
/// this was flagged, in an earlier version of this comment, as
/// deliberately not yet wired up ("the banner discussed but not yet
/// built"), since a modal dialog firing from an unattended midnight
/// timer is exactly the jarring UX this method was already redesigned
/// around avoiding. The banner itself has since been built and is
/// already in scope by the time this function's own caller runs (see
/// that call site's own comment), so this now does what that original
/// comment described as the eventual right fix.
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

/// Picks black or white text so it stays readable against `hex` (a
/// "#RRGGBB" background), using perceived luminance.
/// A color for a brand-new card, picked from a small curated palette
/// rather than always the same flat default -- so a batch of cards
/// added without anyone bothering to set a custom color don't all look
/// identical. No new dependency for randomness: seeded from the card's
/// own freshly-generated Uuid (already random) rather than pulling in a
/// `rand` crate just for this.
/// Wraps any http(s) URLs found in `text` in Pango markup <a> tags, so
/// GtkLabel renders them as actual clickable links -- a real, native
/// GtkLabel feature (markup containing <a href="..."> is automatically
/// clickable/keyboard-navigable, no extra signal wiring needed).
/// Escapes everything else via markup_escape_text, since this is
/// user-typed notes text going into markup, not a literal -- needed in
/// case notes ever contain characters like & or < that would otherwise
/// break the markup.
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

/// Converts a "#rrggbb" hex color to a CSS rgba(...) string at the given
/// alpha -- used to soften a color's visual intensity without changing
/// which of the two contrasting_text_color options it actually is.
/// Needed specifically because a symbolic icon's solid-filled shape
/// reads far more starkly at pure black/white than the same color does
/// applied to thin text glyphs or a small star -- same underlying color,
/// very different perceived intensity, by request. Named distinctly
/// from the existing, differently-purposed hex_to_rgba elsewhere in this
/// file (which returns a real gdk::RGBA struct for widget APIs like
/// ColorDialogButton) -- this one produces a CSS text string instead, a
/// genuinely different thing despite the similar name; the two
/// collided under the same name in an earlier pass and failed to
/// compile as a result.
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

/// Picks a point size for a tile's name label based on the name's length,
/// so a short name like "Harps" can render big and confident while a long
/// one like "Bentonville Public Library" backs off enough to still read
/// cleanly wrapped across a few lines instead of getting cut off. This is
/// a character-count heuristic rather than a real text-width measurement
/// (there's no realized Pango layout to measure against at this point,
/// before the label has been added to a shown window), tuned against
/// CARD_TILE_WIDTH -- if that constant changes materially, these
/// breakpoints likely need retuning too.
fn tile_font_size(name: &str) -> i32 {
    match name.chars().count() {
        0..=6 => 22,
        7..=10 => 18,
        11..=16 => 15,
        17..=24 => 13,
        _ => 11,
    }
}

/// Builds one grid tile for the sidebar's card grid -- a small, rounded,
/// colored tile rather than a flat list row, so the sidebar reads as "a
/// stack of your loyalty cards" instead of a plain list -- similar in
/// spirit to Catima's own grid view. Every tile is CARD_TILE_WIDTH x
/// CARD_TILE_HEIGHT; FlowBox's own default reflow handles column count
/// as the window resizes, nothing here reacts to window size at all.
/// Toggles one card's membership in the multiselect selection set --
/// the single place this logic lives, called identically by both the
/// right-click and long-press gesture handlers in rebuild_grid, so
/// there's exactly one implementation to get right regardless of which
/// gesture triggered it.
fn toggle_card_selection(selected_card_ids: &Rc<RefCell<HashSet<Uuid>>>, card_id: Uuid) {
    let mut ids = selected_card_ids.borrow_mut();
    if !ids.remove(&card_id) {
        ids.insert(card_id);
    }
}

/// Which selection badge a tile should show, if any -- distinct from a
/// bare bool since "not selected" now needs two different states: no
/// badge at all when multiselect isn't active, versus an explicit empty
/// checkbox when it is. By request: an empty-box badge on every
/// unselected tile while multiselect is active gives a constant,
/// unmistakable "we're in multiselect mode" signal even when every
/// currently-visible tile happens to be unselected (scrolled off,
/// filtered out, searched away) -- the mode itself stays visible even
/// when the actual selections aren't.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TileSelectionBadge {
    None,
    Checked,
    Unchecked,
}

fn build_card_tile(
    card: &Card,
    selection_badge: TileSelectionBadge,
    tile_css_providers: &Rc<RefCell<HashMap<Uuid, gtk::CssProvider>>>,
) -> gtk::Widget {
    // Pango markup sets the text color directly on the label's rendering,
    // so it can't lose a CSS specificity fight with the theme the way a
    // `label { color: ... }` rule could (that's what was making list
    // text disappear before -- same fix, carried over to the tile).
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
    // A wrapping label's *natural* width preference is still based on
    // fitting the text on one line -- wrapping only kicks in when GTK
    // is actually forced to give it less than that. Without a cap, a
    // long card name's uncapped natural width propagates up through
    // card_face and the Overlay, and since set_width_request is a
    // floor (raises a too-small natural size, doesn't shrink an
    // already-larger one), that could make FlowBox size every cell
    // wider than CARD_TILE_WIDTH to fit it. max_width_chars(18) caps
    // that natural width to a small enough value it can't win.
    //
    // 18, not something smaller: WordChar wrap mode falls back to
    // character-by-character wrapping (with hyphenation) when there's
    // no room for even one whole word, so too small a cap forces every
    // line to hyphenate. 18 was checked against tile_font_size's actual
    // buckets: the riskiest is "11..=16 chars => 15pt", and a
    // 16-character name at 15pt bold comes to roughly 130-140px, which
    // still clears CARD_TILE_WIDTH minus this label's 20px of margins
    // (140px) with some room, and 18 exceeds that bucket's own max
    // length anyway so it isn't the binding constraint there. Longer
    // names (17+ chars) get progressively smaller fonts (13pt, then
    // 11pt), so they stay safe too despite being numerically longer.
    label.set_max_width_chars(18);
    label.set_justify(gtk::Justification::Center);
    label.set_halign(gtk::Align::Center);
    label.set_valign(gtk::Align::Center);
    // Without vexpand, the label only ever claims its own natural
    // (minimum) height, so it sits flush at the top of the tile and
    // valign(Center) above has no extra space to actually center it
    // into. vexpand lets it claim the tile's full height so centering
    // has somewhere to happen.
    label.set_vexpand(true);
    label.set_lines(3);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_margin_start(10);
    label.set_margin_end(10);

    let card_face = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card_face.append(&label);

    // Unique widget name (-> CSS ID selector) per card, same trick the
    // list row used: an ID selector is specific enough to beat
    // FlowBoxChild's theme rules on its own, without `!important` (a
    // hard parse error in GTK's CSS parser).
    let widget_id = format!("barkeep-tile-{}", card.id.simple());
    card_face.set_widget_name(&widget_id);
    // Also colors the selection badge icon below (badge_widget_id) --
    // included unconditionally here even though the badge itself may
    // not exist this rebuild (TileSelectionBadge::None), since the
    // badge's own construction happens later in this function and a
    // CSS rule targeting a widget ID that doesn't exist is harmless,
    // just unused. Needed at all because a symbolic icon's color comes
    // from real CSS (the `color` property), not the Pango markup
    // foreground attribute the star/checkmark labels used when this was
    // still a text glyph.
    let badge_widget_id = format!("barkeep-badge-{}", card.id.simple());
    // Same reasoning as badge_widget_id just above -- a second, distinct
    // widget ID for the archived-status icon (added below), included
    // unconditionally here even for a non-archived card, since an
    // unmatched CSS rule is harmless.
    let archived_badge_widget_id = format!("barkeep-archived-badge-{}", card.id.simple());
    // A third, for the star icon (added below, once it becomes a real
    // symbolic icon instead of a Pango-markup text glyph) -- same
    // reasoning, included unconditionally even for an unstarred card.
    let star_widget_id = format!("barkeep-star-{}", card.id.simple());
    // Softened, not the plain full-intensity text_color -- by request,
    // to calm down how stark a solid-filled symbolic icon reads at pure
    // black/white compared to the same color on thin text. Shared
    // across all three icon badges now (star, the checkbox selection
    // badge, and the archived badge) -- all three are solid-filled
    // symbolic icons with the identical stark-at-full-intensity problem,
    // by request. 0.7 is a reasonable starting point, not a
    // precisely-tuned value; easy to adjust if it still reads too
    // strong or too faint.
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
    // Reuses this card's own existing provider (from a previous
    // rebuild_grid call) if one's already cached, rather than always
    // constructing and registering a fresh one -- see
    // tile_css_providers' own doc comment above for why. A card seen
    // for the first time still gets a brand new provider, registered
    // exactly once; every rebuild after that just replaces its content.
    let mut providers = tile_css_providers.borrow_mut();
    match providers.get(&card.id) {
        Some(existing) => existing.load_from_string(&css_text),
        None => {
            let css = gtk::CssProvider::new();
            css.load_from_string(&css_text);
            gtk::style_context_add_provider_for_display(
                &default_display(),
                &css,
                // PRIORITY_USER, not PRIORITY_APPLICATION -- same fix
                // as register_css's own change and name_label_css's own
                // provider above, for consistency (see register_css's
                // own doc comment for the full reasoning). Not tied to
                // a specific currently-reported bug for this provider
                // specifically, but the same class of issue (needing to
                // reliably override libadwaita's own internal
                // defaults), so left inconsistent with the other two
                // fixed sites otherwise.
                gtk::STYLE_PROVIDER_PRIORITY_USER,
            );
            providers.insert(card.id, css);
        }
    }
    drop(providers);

    // Wrapped in an Overlay so a starred card can get a small filled-star
    // badge layered on top in the corner (now top-right, moved to make
    // room for the selection badge below) and, while multiselect is
    // active, every tile gets a checked or empty box badge in the
    // opposite corner (top-left) -- see TileSelectionBadge's own doc
    // comment for why unselected tiles get an explicit empty-box badge
    // rather than no badge at all.
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&card_face));

    if card.starred {
        // starred-symbolic rather than the earlier Pango-markup ★ glyph,
        // by request -- same symbolic-icon/CSS-coloring pattern as the
        // other badges now (star_widget_id, colored via the CSS rule
        // above), not inline markup. Confirmed, not a fresh guess --
        // matches star_button's own existing icon_name elsewhere in
        // this file exactly, the already-in-use per-card star toggle.
        // Should have checked that directly the first time, the same
        // way shoe-box-symbolic and user-trash-symbolic were both
        // pulled from this app's own already-proven icons rather than
        // guessed at fresh -- this one broke that pattern and got lucky
        // rather than actually being verified.
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.set_widget_name(&star_widget_id);
        star.set_pixel_size(20);
        star.set_halign(gtk::Align::End);
        star.set_valign(gtk::Align::Start);
        star.set_margin_end(8);
        star.set_margin_top(6);
        // Doesn't intercept clicks meant for the tile underneath it --
        // an overlay child covers its full allocation for input purposes
        // by default, which for a small corner badge would otherwise
        // eat taps landing anywhere near it.
        star.set_can_target(false);
        overlay.add_overlay(&star);
    }

    match selection_badge {
        TileSelectionBadge::None => {}
        TileSelectionBadge::Checked | TileSelectionBadge::Unchecked => {
            // Symbolic icons rather than the earlier Unicode glyphs
            // (☑/☐), by request. Color comes from the CSS rule already
            // added to this tile's own provider above (targeting
            // badge_widget_id), not Pango markup -- a symbolic icon's
            // color is a real CSS property, unlike the text glyph this
            // replaces, which could set its color inline via markup.
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

    // Bottom-right "Archived" badge, by request -- otherwise the only
    // way to tell an archived card from a normal one in the grid is to
    // open it, or toggle Hide Archived Cards on and off to see what
    // disappears. The small shoe-box-symbolic icon (clarified by
    // request, replacing an earlier text-label version) -- same icon
    // already used for the archive action elsewhere in this file, and
    // the same symbolic-icon/CSS-coloring approach as the selection
    // badges above, just its own distinct widget id
    // (archived_badge_widget_id) since it's a separate element. Smaller
    // than the selection badges' own 24px -- this is a lower-priority
    // informational indicator, not an interactive selection state, so
    // it doesn't need the same bumped-up visibility.
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
    brightness_button: &gtk::ToggleButton,
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
    // The card's name is the header's own title now, colored to match
    // the card (update_header_style below) -- not a separate label in
    // the body the way it used to be.
    content_header.set_title_widget(Some(&adw::WindowTitle::new(&card.name, "")));
    let text_color = contrasting_text_color(&card.color);
    *name_label_colors.borrow_mut() = (card.color.clone(), text_color.to_string());
    update_header_style(name_label_css, &card.color, text_color);

    edit_button.set_visible(true);
    bottom_bar.set_visible(true);
    // Reset rather than carry over: switching to a (possibly different)
    // card starts a fresh brightness session rather than silently
    // inheriting whatever the previous card left active. `set_active`
    // is a no-op if it's already off, and if it was on, this restores
    // the brightness it saved via the toggle's own handler.
    brightness_button.set_active(false);
    // Hidden entirely (not just left showing but non-functional) when
    // there's nothing to actually control -- get_brightness() returning
    // None means every mechanism brightness.rs knows how to try
    // (backlight/leds device detection, the login1/gnome-settings-daemon
    // D-Bus fallbacks) came up empty on this device, so showing a button
    // that would silently do nothing on tap isn't useful.
    brightness_button.set_visible(brightness::get_brightness().is_some());

    // Link: same disconnect-before-sync reasoning as star/archive
    // below -- a persistent header button now, reconnected per-card
    // (the URL itself, and whether there's one at all, changes).
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
    // No set_margin_* here -- GTK margin is space *outside* a widget's
    // own painted background (the parent's background shows through
    // instead), which is exactly why the gradient below was only
    // covering an inset box instead of vbox's full area. The same
    // visual inset is done as CSS padding instead, in
    // update_header_style's #barkeep-card-body rule -- padding *is*
    // included in the area a widget's own background paints.
    vbox.set_vexpand(true);
    // Targeted by update_header_style's gradient rule -- set fresh here
    // each time since vbox itself is a brand new widget every call,
    // unlike content_header which only needs this once at construction.
    vbox.set_widget_name("barkeep-card-body");

    // Groups and expiry are both shown down near the code text now (see
    // below) -- computed here since both need it, not because they're
    // displayed together right at this point in the function anymore.
    // Plain string comparison against today's ISO date works correctly
    // for expiry since both it and today are "YYYY-MM-DD" (see Card's
    // own doc comment on valid_from/expiry for why that's true).
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
        // and width rather than preserving the rendered PNG's native
        // (tall, per barcode.rs's target_dimensions) aspect ratio --
        // which was the actual cause of them going very narrow on a
        // short window, since Contain has to shrink width to keep that
        // tall aspect intact within limited height. Safe specifically
        // for 1D formats -- see BarcodeType::is_linear's own doc comment
        // for why. 2D formats keep Contain, since stretching those would
        // distort square modules into rectangles.
        picture.set_content_fit(if card.barcode_type.is_linear() {
            gtk::ContentFit::Fill
        } else {
            gtk::ContentFit::Contain
        });
        picture.set_vexpand(true);
        picture.set_hexpand(true);
        // No height_request floor here anymore -- one was tried and
        // confirmed not to actually fix the reported fuzziness (it
        // wasn't the real cause). The actual fix is upstream now, in
        // barcode.rs: 1D barcodes render at 1px native height, so this
        // Picture is always *upscaling* a source with no vertical detail
        // to lose, never downscaling a tall one.
        vbox.append(&picture);

        if !card.code_value.is_empty() {
            let code_label = gtk::Label::builder()
                .selectable(true)
                // can_focus(false): selectable(true) makes this label
                // focusable, and GTK grabs focus on the first focusable
                // descendant when a window's content is shown for the
                // first time if nothing already has focus -- which is
                // what was landing here on a fresh run and showing the
                // code pre-selected. Deliberate mouse click-and-drag
                // selection should still largely work as a direct user
                // action; this only stops it from being an *automatic*
                // focus target. The dedicated "Copy barcode code" menu
                // action covers the primary copy use case regardless.
                .can_focus(false)
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .justify(gtk::Justification::Center)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .max_width_chars(40)
                .css_classes(["monospace".to_string(), "dim-label".to_string()])
                .build();
            // Pango markup rather than a CSS "bold" class -- Adwaita
            // doesn't ship a bare utility class named that (unlike
            // e.g. "heading"), so markup is the reliable way to get
            // bold text here. escape_text since code_value is card
            // data, not a literal -- needed if it ever contains
            // characters like & or < that would otherwise break the
            // markup.
            code_label.set_markup(&format!(
                "<b>{}</b>",
                glib::markup_escape_text(&card.code_value)
            ));
            vbox.append(&code_label);
        }

        // Groups, then expiry -- both moved down here below the code
        // text; share/copy are both permanent bottom-bar buttons now
        // (see their own construction site for why), not built or
        // reparented in this function at all anymore.
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

    // Notes surface via the info button in the headerbar rather than a
    // label under the barcode -- a fresh Popover per card since its
    // content (the note text) changes with the selection; MenuButton
    // owns showing/positioning/dismissing it, including clamping itself
    // to fit narrow phone-width windows, so there's nothing extra to do
    // for small screens here.
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
        // Pango markup with <a href="..."> rather than plain text --
        // GtkLabel renders that as an actual clickable, keyboard-
        // navigable link on its own (a real, native feature; clicking
        // opens it with the system's default handler with no extra
        // wiring needed, the same as this app's own external-link
        // button elsewhere). linkify_urls escapes everything else via
        // markup_escape_text, since this is card notes going into
        // markup, not a literal.
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
        let brightness_button = brightness_button.clone();
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
            // Re-shows the (now-updated) card's own detail view after
            // saving, instead of leaving refresh()'s "no card selected"
            // placeholder up for a card that still exists and is still
            // the one you were just looking at.
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
                let brightness_button = brightness_button.clone();
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
                    // Bypasses refresh() by design (to keep the detail
                    // view open on the just-saved card instead of
                    // resetting to "no card selected"), so it needs its
                    // own explicit call here -- refresh() itself now
                    // calls rebuild_group_tabs too, but this path never
                    // reaches it.
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
                        &brightness_button,
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

    // Star: disconnect the previous card's handler *before* syncing
    // set_active to this card's actual starred state -- ToggleButton
    // fires "toggled" on a programmatic set_active() just as it would a
    // click, so without disconnecting first, syncing the button here
    // would silently re-toggle (and re-save) whichever card was
    // previously shown.
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
            // rebuild_grid, not refresh: refresh also resets the detail
            // pane back to "no card selected", which would close the
            // card you just starred. Starring only needs the grid
            // re-sorted/rebuilt (starred cards sort first -- see
            // CardStore::search), not the detail pane touched at all.
            // No manual icon-swap or Cell-tracked state anymore --
            // toggle_star just flips the DB unconditionally, and the
            // button's own pressed/depressed appearance *is* the state
            // now, driven natively by ToggleButton's active property
            // rather than something this code has to keep in sync by
            // hand.
            let result = store.borrow_mut().toggle_star(card_id);
            if result.is_err() {
                // Per mutate_then_save's own semantics, a failed save
                // means the mutation never actually applied at all --
                // but GTK already set this button's own active state
                // optimistically before this handler ran (a
                // ToggleButton fires "toggled" on the click before the
                // handler gets any chance to veto it), so it's now
                // showing the wrong state. Reverting to !is_active()
                // is exactly the button's own state from just before
                // this click, since it correctly matched card.starred
                // up until this one click happened. Signal-blocked
                // around the revert -- required, not optional, since
                // set_active() fires "toggled" again on a programmatic
                // change the same as a real click would, and without
                // blocking, this revert would recursively re-enter
                // this same closure.
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

    // Archive: same disconnect-before-sync reasoning as star above.
    // Deliberately does *not* navigate back to the grid on archiving,
    // even though the card may now be filtered out of it (if
    // show_archived is off) -- an earlier version did that, but it was
    // reported as jarring to hit the button and immediately get forced
    // out of the card you were just looking at. Staying open until the
    // person actually closes/navigates away themselves is the point;
    // the grid catches up (loses the now-archived card) whenever that
    // happens, same as it always does.
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
                // Same reasoning as star_button's own revert-on-failure
                // just above: per mutate_then_save's own semantics, a
                // failed save means the mutation never actually applied
                // at all, but GTK already set this button's own active
                // state optimistically before this handler ran, so
                // without reverting it here, it would keep showing the
                // wrong state. Signal-blocked around the revert for the
                // same reason as star_button's own -- required, not
                // optional, since set_active() fires "toggled" again on
                // a programmatic change same as a real click would.
                if let Some(id) = archive_handler.borrow().as_ref() {
                    btn.block_signal(id);
                    btn.set_active(!btn.is_active());
                    btn.unblock_signal(id);
                }
            }
            report_if_save_failed(&banner, result);
            // is_active() now reflects the final, correct state --
            // either the click's own new state (save succeeded) or the
            // reverted original state (save failed) -- not just
            // whatever GTK set optimistically before this handler ran,
            // now that a failure's own revert (just above) has already
            // happened by this point.
            let now_archived = btn.is_active();
            btn.set_tooltip_text(Some(if now_archived {
                "Unarchive"
            } else {
                "Archive"
            }));
            (rebuild_grid.borrow())();
            // Archiving/unarchiving changes whether this card counts as
            // visible under the current show_archived filter, which is
            // exactly what rebuild_group_tabs' own filtering depends on
            // -- if this card is the last visible member of some group
            // (under Include Archived Cards being off), archiving it
            // needs that group's tab to disappear, and unarchiving it
            // back needs the reverse.
            (rebuild_group_tabs.borrow())();
        })
    };
    *archive_handler.borrow_mut() = Some(new_archive_id);

    // Copy barcode code / copy barcode image, combined into one share
    // menu button instead of two separate ActionBar buttons -- see
    // share_menu_button's own construction site for the reasoning.
    // set_visible on the button itself is per-card (once); each action
    // gets its own disconnect-before-reconnect, same reasoning as
    // star/archive above, since connect_activate returns a real
    // SignalHandlerId the same way a button's connect_clicked does.
    share_menu_button.set_visible(!card.code_value.is_empty());

    disconnect_if_present(copy_code_action, &copy_code_handler);
    let new_copy_code_id = {
        let code_value = card.code_value.clone();
        let toast_overlay_for_copy = toast_overlay.clone();
        let share_menu_button = share_menu_button.clone();
        copy_code_action.connect_activate(move |_action, _param| {
            // Widget::clipboard() works from any realized widget on the
            // relevant display -- share_menu_button stands in for
            // whatever widget would normally supply this in a button's
            // own click handler (an action has no widget of its own to
            // call it from).
            share_menu_button.clipboard().set_text(&code_value);
            // set_text has no meaningful local failure mode to check (it
            // just stages content into GDK's own clipboard store) --
            // unconditional success message is accurate here, unlike
            // copy-image below which has a real failure path worth
            // distinguishing.
            toast_overlay_for_copy.add_toast(adw::Toast::new("Copied to clipboard"));
        })
    };
    *copy_code_handler.borrow_mut() = Some(new_copy_code_id);

    // Copies the card's rendered barcode straight to the clipboard as
    // an image, standing in for a platform "share" action -- no
    // universal share-sheet API exists on Linux desktop, and a
    // byte-compatible clone of Catima's own URL-based card sharing
    // isn't achievable either (that format lives on a GitHub wiki page
    // that isn't accessible, and fundamentally depends on catima.app
    // hosting a matching renderer anyway, which isn't something this
    // app can lean on). Pasting an image is broadly supported by
    // messaging apps, email clients, etc, so it's a genuinely useful
    // desktop-native equivalent rather than a compromise.
    disconnect_if_present(copy_image_action, &copy_image_handler);
    let new_copy_image_id = {
        let card_for_share = card.clone();
        let toast_overlay_for_share = toast_overlay.clone();
        let share_menu_button = share_menu_button.clone();
        copy_image_action.connect_activate(move |_action, _param| {
            // set_texture: a real, documented gdk::Clipboard convenience
            // method parallel to set_text (used for copy-code above,
            // already confirmed working) -- not compiled/verified
            // against this exact gtk4-rs version, same confidence
            // caveat as the rest of this app's less-common API calls.
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

/// Shared add/edit form. `is_new` controls whether Save does an add or an
/// update -- pass `true` for both a blank manual-entry card and a
/// scan-prefilled one (neither exists in the store yet), `false` only
/// when editing a card that's already saved.
/// Which tab open_edit_dialog should start on -- Card by default for
/// every existing entry point (new card, editing an existing one via
/// the detail view's own edit button), Options specifically when
/// re-opening after Manage Groups, since that's the tab the Groups
/// section actually lives on and the user was just there.
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
    // Rc-wrapped rather than a bare Box<dyn Fn(Card)> -- both the Save
    // button's own handler and the Manage Groups button's handler
    // (below) need their own access to this, and a boxed closure alone
    // can only ever be moved into one place, not cloned into two. Every
    // call site now wraps its own argument in Rc::new(...); cloning an
    // Rc for each closure that needs it is cheap and always possible,
    // unlike cloning the boxed closure inside it.
    on_saved: Rc<Option<Box<dyn Fn(Card)>>>,
    // These three are needed to pass along to open_manage_groups_dialog
    // when the Manage Groups button (below) is clicked -- that
    // function's own signature wants them directly, not some general
    // "refresh everything" callback that happens to wrap them
    // internally the way refresh (above) already does.
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
    // Multi-line, not the single-line adw::EntryRow style the other
    // fields use -- Enter needs to insert an actual line break here,
    // which EntryRow (built on GtkEntry) doesn't support at all
    // (Enter activates/submits it instead). gtk::TextView is GTK's
    // standard multi-line text editor and handles Enter-as-newline as
    // its own default behavior, no extra wiring needed for that part.
    // Not a ListBox row like the fields above -- appended to form_box
    // directly instead (same pattern the Groups section below already
    // uses), since a tall multi-line editor doesn't really fit
    // alongside single-line boxed-list rows the way EntryRow did.
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
    // "view"/"frame" classes for a bordered look roughly matching the
    // boxed-list rows' visual weight -- not compiled/verified against
    // this exact libadwaita version, same confidence caveat as this
    // app's other less-common style choices.
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
    // single-string category field. A plain (non-flat) ToggleButton
    // already gets a visually distinct checked/highlighted appearance
    // from Adwaita's own stylesheet by default -- exactly the "gray
    // when off, highlighted when on" look wanted here, no custom CSS
    // needed beyond the "pill" class (a real, standard libadwaita style
    // class for rounded chip-shaped buttons) for the shape. Reuses
    // gtk::FlowBox, the same widget class this app already uses for the
    // whole card grid, just wrapping small toggle pills instead of
    // card tiles -- proven working in this exact codebase already.
    let selected_group_ids: Rc<RefCell<Vec<Uuid>>> =
        Rc::new(RefCell::new(card.borrow().group_ids.clone()));
    // Reduces the pills' own internal padding (both axes) below the
    // stock .pill class's default -- that default is generous enough
    // that even short labels like "Group A" were reading as almost the
    // same width as much longer ones, which is what made the FlowBox's
    // already-correct wrapping (homogeneous(false) below sizes each
    // pill to its own natural width, not a uniform grid cell) look like
    // a fixed grid instead. No background-color override for the
    // unchecked state anymore (reconsidered by request, same reasoning
    // as the multiselect dialog's own version of this pill): an earlier
    // card-color attempt here (#ffffff) read as nearly identical to
    // the dialog's own window-background and was replaced
    // with a translucent shade-color overlay to fix that -- but the
    // actual problem was that specific color choice blending into its
    // surroundings, not that GTK's own stock .pill styling (border/
    // shadow, not just a flat color) can't provide that contrast
    // natively on its own, in both light and dark mode, without this
    // needing to pick any color at all. Selected uses the real theme
    // accent (var(--accent-bg-color)/var(--accent-fg-color) -- libadwaita's
    // own real, current named-color syntax, confirmed directly against
    // the official libadwaita CSS variables docs, replacing an earlier
    // @accent_bg_color/@accent_fg_color reference that was never
    // actually verified), matching how the rest of this app already
    // treats accent color elsewhere, not an invented one. A fresh
    // CssProvider is registered every time this dialog opens rather
    // than once globally -- simpler, and a realistic session isn't
    // opening this dialog often enough for the redundant
    // re-registration to matter.
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
    // Always shown now, by request -- previously this whole section
    // (title, pills, everything) was hidden outright whenever no groups
    // existed yet, which meant there was no way to discover or reach
    // group management from here at all. "No groups yet" matches this
    // app's own existing empty-state voice elsewhere (see empty_status's
    // "No Cards Yet" title in build_window) rather than inventing new
    // wording for the same kind of moment.
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
    // Always present, regardless of has_groups -- by request, so
    // there's always a direct path to group management from right here,
    // not just from the main window's own menu. Its click handler is
    // attached later, alongside save_button's own (see that comment) --
    // several of the fields it needs to snapshot (valid_from_value,
    // expiry_value, on_expiration_row, color_button, type_row) aren't
    // constructed yet at this point in the function.
    let manage_groups_button = gtk::Button::builder()
        .label("Manage Groups")
        .halign(gtk::Align::Center)
        .margin_top(20)
        .build();
    groups_box.append(&manage_groups_button);

    // Date picker: an ActionRow (tap to open a popover with a
    // gtk::Calendar) instead of free-text entry. `current_value` is
    // created by the *caller* and passed in (rather than created and
    // returned by this function) specifically so `on_change` can close
    // over the real, single Rc directly -- an earlier version had this
    // function own current_value internally, which meant on_change
    // (built before that internal value existed) couldn't reference it
    // and needed an awkward manually-resynced duplicate instead. This
    // is simpler and has one source of truth. `on_change` fires after
    // every value mutation (a day picked, or Clear Date); wrapped in an
    // Rc internally since it needs to be shared between two separate
    // signal handlers (day-selected and the clear button), not because
    // callers need to worry about that. Not compiled/verified against
    // this exact gtk4-rs version -- gtk::Calendar hasn't been used
    // anywhere else in this app before.
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

    // Built after on_expiration_row (rather than before, like
    // valid_from_row) specifically so its on_change closure can
    // directly capture on_expiration_row and keep its sensitivity
    // live-synced with expiry_value -- disabled whenever expiry is
    // cleared, enabled whenever a date is actually picked.
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
            // Same reasoning as the main detail view's picture: 1D
            // barcodes now render at 1px native height (see barcode.rs),
            // so Contain -- which this was hardcoded to regardless of
            // type -- was scaling by the *width* constraint on such an
            // extreme (700:1) aspect ratio, shrinking the already-1px
            // source down to a fraction of a pixel instead of
            // stretching it to fill. Fill for 1D, Contain for 2D, set
            // fresh each time since barcode_type can change as the
            // dropdown selection changes, unlike the main detail view
            // where it's fixed per already-saved card.
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

    // Delete, on the Card tab -- only shown when editing an existing
    // card (is_new means there's nothing in the store yet to delete).
    // Reuses confirm_delete, the same confirmation dialog the detail
    // view's own delete action already used before this moved here.
    // Icon + "Delete" text (misread as icon-only in a previous pass --
    // "small" meant "not full window width," not "no text") via
    // AdwButtonContent, the libadwaita widget built for exactly that
    // combination -- gtk::Button's own icon_name/label properties don't
    // actually combine into one. Bottom-right now, in a bottom
    // ActionBar rather than the header (also corrected from a previous
    // pass) -- ActionBar doesn't stretch its children to fill by
    // default, so sitting in one is what actually keeps this compact
    // instead of full-width, not anything about the button itself.
    // user-trash-symbolic already proven working elsewhere in this app.
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

    // AdwViewStack/AdwViewSwitcher rather than a single long scrolling
    // form -- the dialog had grown tall enough (preview, six rows,
    // notes, groups, all stacked) to be reported as a real problem.
    // ViewStack/ViewSwitcher is libadwaita's own purpose-built pattern
    // for exactly this (a small, fixed set of named pages switched from
    // the header) -- the same shape GNOME's own Preferences-style
    // dialogs use, not something built from scratch here.
    let view_stack = adw::ViewStack::new();
    view_stack.add_titled(&card_page, Some("card"), "Card");
    view_stack.add_titled(&options_page, Some("options"), "Options");

    // unit(Px): same sp-by-default issue as split_view's sidebar width -- AdwClamp's maximum_size
    // also defaults to "sp" units, so this 480 was being interpreted as
    // 480sp (wider than intended on any system with text scaling above
    // 100%) until this was set explicitly.
    let clamp = adw::Clamp::builder()
        .child(&view_stack)
        .maximum_size(480)
        .unit(adw::LengthUnit::Px)
        .build();
    clamp.set_margin_top(16);
    clamp.set_margin_bottom(16);
    clamp.set_margin_start(16);
    clamp.set_margin_end(16);

    // propagate_natural_height: without this, a ScrolledWindow's own
    // reported natural height defaults to something minimal regardless
    // of how tall its content actually wants to be -- it's *designed*
    // to scroll rather than grow. This makes it instead report its
    // child's real natural height (while still actually scrolling/
    // clipping if the available space ends up smaller, e.g. a small
    // phone screen), which is what lets content_height below act as a
    // generous cap rather than a fixed size the dialog always claims
    // regardless of whether the form actually needs it. One shared
    // scroller wrapping the whole view_stack, not one per tab -- each
    // tab's content scrolls within the same dialog chrome, and
    // switching tabs doesn't need to rebuild or resize it.
    let scroller = gtk::ScrolledWindow::builder()
        .child(&clamp)
        .vexpand(true)
        .propagate_natural_height(true)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    // Suppresses the desktop environment's own window-control buttons
    // (typically just a close [X]) on this dialog's header -- our own
    // explicit back/save buttons already cover those actions, so the
    // DE's own window-control buttons (typically just a close [X]) are
    // no longer suppressed here -- an "X" cancel button and an icon-only
    // save button were both tried in the header instead, but reportedly
    // weren't working out stylistically, so back to the platform
    // default plus delete_button (left)/save_button (right) below,
    // rather than continuing to try to replace it with something custom.
    // Custom tab strip instead of AdwViewSwitcher -- the icons
    // ViewSwitcher was choosing to render (this app never set any
    // explicit page icons; whatever it fell back to on its own)
    // reportedly didn't render on-device at all. Styled the same way as
    // the main view's group-filter tabs (.group-tab, already registered
    // globally via style_context_add_provider_for_display back in
    // build_window, so it's already active here with no need to
    // register it again) -- no scroll/paging buttons needed here
    // unlike that strip, since this one only ever has exactly two tabs
    // and comfortably fits the dialog's width on its own.
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
    // view_stack's own visible child needs to agree with whichever tab
    // button starts active above -- its default (whatever page was
    // add_titled first, "card") only happens to already match the Card
    // case; the Options case needs setting explicitly or the tab
    // button and the actual visible content would disagree on startup.
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

    // Save (bottom-left) and Delete (bottom-right), in a bottom
    // ActionBar rather than the header -- matches the detail view's own
    // bottom_bar pattern already used elsewhere in this app for the
    // same "actions along the bottom" shape.
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
        // Generous cap, not a target -- propagate_natural_height above
        // means the dialog only actually grows this tall if the form's
        // real content needs it and the screen has the room; 560 was
        // being hit as a hard ceiling even on screens with plenty of
        // space to spare, which is what was reported. Should matter far
        // less now that content is split across two tabs anyway.
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
                // In the (permanently) collapsed single-pane layout,
                // this is what actually switches back to the sidebar's
                // card grid -- refresh() alone only repopulates the
                // content pane with its own "no card selected"
                // placeholder, it doesn't switch which pane is showing.
                // This call existed on the old detail-view delete button
                // before delete moved into this dialog, and got lost in
                // that move -- restoring it here.
                split_view.set_show_content(false);
                dialog.close();
            });
        });
    }

    {
        // Owned clone of the &adw::ApplicationWindow reference -- needed
        // since this closure has to outlive the current call to
        // open_edit_dialog, which the borrowed reference itself
        // wouldn't.
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
        // Each of these also needs its own clone here -- confirmed via
        // real compiler errors, not assumed: without this, this
        // closure's own move |_| moves each widget out of the
        // surrounding scope entirely, leaving nothing left for
        // save_button's own handler (right below this one) to use, even
        // though GTK/libadwaita widgets are cheap, reference-counted
        // clones under the hood, not a deep copy.
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
            // Same field-by-field snapshot save_button's own handler
            // builds, deliberately duplicated rather than shared --
            // this one is never saved to the store (no store.borrow_mut
            // ().add/update call at all, unlike save_button's), just
            // carried across to the re-opened dialog below, so keeping
            // it a separate, self-contained block avoids any risk of a
            // shared-closure refactor accidentally changing what
            // save_button itself does.
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

            // window_for_reopen: a genuinely separate binding from
            // `window` below, not just another name for the same one --
            // confirmed via a real E0505 compiler error, not assumed.
            // open_manage_groups_dialog's own first argument borrows
            // `window`, and that borrow is considered active for the
            // whole call expression, including while building its later
            // arguments -- so moving the *same* variable into the
            // on_closed closure a few arguments later, within that same
            // call, conflicts with the still-active borrow. A distinct
            // clone sidesteps the conflict entirely rather than fighting
            // the borrow checker over ordering.
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
                    // Re-opens with the snapshot preserved and whatever
                    // groups now exist -- rebuilding the whole dialog
                    // from scratch rather than trying to patch the
                    // still-open one's own Groups section in place,
                    // reusing all of open_edit_dialog's existing,
                    // already-working construction logic (including the
                    // Groups section itself, which will naturally see
                    // the store's current group list) instead of new,
                    // narrower "just refresh this one part" logic.
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
                // Explicit deref (&*on_saved, not &on_saved) -- on_saved
                // is Rc<Option<...>> now, and Rc doesn't get the same
                // automatic match-ergonomics deref a plain reference
                // does, so matching against it directly wouldn't
                // type-check against the Some/None patterns below.
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

//! Builds a single card's row for the lock-screen list.
//!
//! Deliberately not a custom GObject subclass the way an earlier draft
//! of this file was (subclassing AdwActionRow) -- that approach doesn't
//! carry over cleanly to AdwExpanderRow, since unlike AdwActionRow/
//! AdwPreferencesRow (both confirmed subclassable, via their own real
//! ActionRowImpl/PreferencesRowImpl traits in adw::subclass::prelude,
//! used by the widget.rs's own PrefsDialog and this crate's own earlier
//! draft of this file), there's no similarly-confirmed
//! ExpanderRowImpl-equivalent trait findable for AdwExpanderRow itself
//! -- and with no compile check available for the next several hours,
//! betting on an unconfirmed subclassing capability isn't worth it. Not
//! needed anyway: the only reason to subclass was to attach a Card to
//! each row instance, and a plain closure capturing the card directly
//! does that without needing a custom type at all -- a stock
//! AdwExpanderRow instance, configured and wired up by this one
//! function, is both simpler and lower-risk.
//!
//! The barcode/code-value content below each row is built lazily, on
//! that row's own first expansion, rather than upfront for every row
//! when the list first populates -- render_png does real work (rxing
//! encode, PNG write to disk), and a permissive filter could plausibly
//! leave many rows in the list, most of which the user may never
//! expand at all in a given lock-screen session.
//!
//! Exclusive expansion (only one row open at a time) isn't something
//! AdwExpanderRow provides on its own -- confirmed by its own full
//! property/method list carrying no group/exclusivity concept at all,
//! unlike e.g. GtkCheckButton's own "group" property for radio-button-
//! style behavior. build_row takes the row's own parent GtkListBox
//! directly so it can enforce this itself: on this row's own expansion,
//! walk every sibling in that same list and collapse any other one
//! that's still open.

use adw::prelude::*;
use cardigan_core::barcode;
use cardigan_core::card::Card;
use std::cell::Cell;
use std::rc::Rc;

pub fn build_row(card: &Card, list_box: &gtk::ListBox) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::new();
    row.set_title(&glib::markup_escape_text(&card.name));

    // Built once, on this row's own first expansion -- see this file's
    // own doc comment above for why lazily rather than upfront.
    let content_built = Rc::new(Cell::new(false));
    let card = card.clone();
    let list_box = list_box.clone();

    row.connect_expanded_notify(move |row| {
        if !row.is_expanded() {
            return;
        }

        // Exclusive expansion: collapse every other AdwExpanderRow
        // sibling in the same list. Setting an already-collapsed row's
        // own "expanded" back to false is a no-op that GObject won't
        // re-notify on (property notifications only fire on an actual
        // value change), so this can't loop back into this same
        // handler for any row other than the one that just expanded.
        let mut child = list_box.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            if let Some(sibling) = widget.downcast_ref::<adw::ExpanderRow>() {
                if sibling != row {
                    sibling.set_expanded(false);
                }
            }
            child = next;
        }

        if content_built.get() {
            return;
        }
        content_built.set(true);

        let content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content_box.set_margin_start(12);
        content_box.set_margin_end(12);
        content_box.set_margin_top(12);
        content_box.set_margin_bottom(12);

        if let Some(path) = barcode::render_png(&card) {
            // Same content-fit distinction as window.rs's own,
            // already-working show_card_detail (confirmed directly
            // from that real, existing code, not guessed at): Fill for
            // 1D barcodes (which would otherwise go very narrow, since
            // Contain has to shrink width to preserve their own tall
            // native aspect ratio within limited height), Contain for
            // 2D formats (where stretching would distort square
            // modules into rectangles).
            let picture = gtk::Picture::for_filename(&path);
            picture.set_content_fit(if card.barcode_type.is_linear() {
                gtk::ContentFit::Fill
            } else {
                gtk::ContentFit::Contain
            });
            picture.set_height_request(120);
            content_box.append(&picture);

            let code_label = gtk::Label::builder()
                .label(&card.code_value)
                .selectable(true)
                .css_classes(["dim-label"])
                .wrap(true)
                .build();
            content_box.append(&code_label);
        } else {
            content_box.append(&gtk::Label::new(Some("No code set for this card.")));
        }

        row.add_row(&content_box);
    });

    row
}

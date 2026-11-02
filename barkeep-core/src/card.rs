use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// The 13 barcode/QR formats Catima supports. Kept name-for-name so the
/// Catima importer can map directly onto this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BarcodeType {
    Aztec,
    Codabar,
    Code39,
    Code93,
    Code128,
    DataMatrix,
    Ean8,
    Ean13,
    Itf,
    Pdf417,
    QrCode,
    UpcA,
    UpcE,
    /// Not a Catima format. Barkeep's fallback for cards with no
    /// machine-readable code, or a decoded symbology we can't yet
    /// re-render.
    NoCode,
}

impl BarcodeType {
    pub fn label(&self) -> &'static str {
        match self {
            BarcodeType::Aztec => "Aztec",
            BarcodeType::Codabar => "Codabar",
            BarcodeType::Code39 => "Code 39",
            BarcodeType::Code93 => "Code 93",
            BarcodeType::Code128 => "Code 128",
            BarcodeType::DataMatrix => "Data Matrix",
            BarcodeType::Ean8 => "EAN-8",
            BarcodeType::Ean13 => "EAN-13",
            BarcodeType::Itf => "ITF (Interleaved 2 of 5)",
            BarcodeType::Pdf417 => "PDF417",
            BarcodeType::QrCode => "QR Code",
            BarcodeType::UpcA => "UPC-A",
            BarcodeType::UpcE => "UPC-E",
            BarcodeType::NoCode => "No code",
        }
    }

    pub fn all() -> &'static [BarcodeType] {
        use BarcodeType::*;
        &[
            QrCode, Code128, Code39, Code93, Codabar, Ean8, Ean13, Itf, Pdf417, UpcA, UpcE,
            Aztec, DataMatrix, NoCode,
        ]
    }

    /// True for 1D/linear barcode formats, as opposed to 2D matrix
    /// formats (QR, Aztec, DataMatrix) or PDF417 (technically 2D
    /// "stacked linear", kept in the non-linear bucket since its error
    /// correction relies on 2D structure). Used by the detail view to
    /// decide whether stretching the rendered barcode to fill
    /// available space is safe: stretching a 1D barcode multiplies
    /// every bar's width by the same factor, preserving the relative
    /// ratios the encoding depends on. Stretching a 2D grid distorts
    /// square modules into rectangles.
    pub fn is_linear(self) -> bool {
        use BarcodeType::*;
        matches!(
            self,
            Code128 | Code39 | Code93 | Codabar | Ean8 | Ean13 | Itf | UpcA | UpcE
        )
    }

    /// Maps to rxing's format enum for encode/decode. `None` for
    /// `NoCode`, which has no barcode representation.
    pub fn to_rxing(self) -> Option<rxing::BarcodeFormat> {
        use rxing::BarcodeFormat as R;
        match self {
            BarcodeType::Aztec => Some(R::AZTEC),
            BarcodeType::Codabar => Some(R::CODABAR),
            BarcodeType::Code39 => Some(R::CODE_39),
            BarcodeType::Code93 => Some(R::CODE_93),
            BarcodeType::Code128 => Some(R::CODE_128),
            BarcodeType::DataMatrix => Some(R::DATA_MATRIX),
            BarcodeType::Ean8 => Some(R::EAN_8),
            BarcodeType::Ean13 => Some(R::EAN_13),
            BarcodeType::Itf => Some(R::ITF),
            BarcodeType::Pdf417 => Some(R::PDF_417),
            BarcodeType::QrCode => Some(R::QR_CODE),
            BarcodeType::UpcA => Some(R::UPC_A),
            BarcodeType::UpcE => Some(R::UPC_E),
            BarcodeType::NoCode => None,
        }
    }

    pub fn from_rxing(fmt: rxing::BarcodeFormat) -> BarcodeType {
        use rxing::BarcodeFormat as R;
        match fmt {
            R::AZTEC => BarcodeType::Aztec,
            R::CODABAR => BarcodeType::Codabar,
            R::CODE_39 => BarcodeType::Code39,
            R::CODE_93 => BarcodeType::Code93,
            R::CODE_128 => BarcodeType::Code128,
            R::DATA_MATRIX => BarcodeType::DataMatrix,
            R::EAN_8 => BarcodeType::Ean8,
            R::EAN_13 => BarcodeType::Ean13,
            R::ITF => BarcodeType::Itf,
            R::PDF_417 => BarcodeType::Pdf417,
            R::QR_CODE => BarcodeType::QrCode,
            R::UPC_A => BarcodeType::UpcA,
            R::UPC_E => BarcodeType::UpcE,
            // Anything else decodable but not cleanly categorized
            // above (e.g. RSS/MaxiCode): keep the decoded text, drop
            // the symbology.
            _ => BarcodeType::NoCode,
        }
    }

    /// Reverse of from_catima_name. NoCode has no Catima equivalent,
    /// so it writes an empty barcodetype field, matching how Catima
    /// represents a card with no code.
    fn to_catima_name(self) -> &'static str {
        use BarcodeType::*;
        match self {
            Aztec => "AZTEC",
            Codabar => "CODABAR",
            Code39 => "CODE_39",
            Code93 => "CODE_93",
            Code128 => "CODE_128",
            DataMatrix => "DATA_MATRIX",
            Ean8 => "EAN_8",
            Ean13 => "EAN_13",
            Itf => "ITF",
            Pdf417 => "PDF_417",
            QrCode => "QR_CODE",
            UpcA => "UPC_A",
            UpcE => "UPC_E",
            NoCode => "",
        }
    }

    /// Catima's CSV `barcodetype` column uses these exact strings.
    fn from_catima_name(name: &str) -> Option<BarcodeType> {
        use BarcodeType::*;
        Some(match name.trim() {
            "AZTEC" => Aztec,
            "CODABAR" => Codabar,
            "CODE_39" => Code39,
            "CODE_93" => Code93,
            "CODE_128" => Code128,
            "DATA_MATRIX" => DataMatrix,
            "EAN_8" => Ean8,
            "EAN_13" => Ean13,
            "ITF" => Itf,
            "PDF_417" => Pdf417,
            "QR_CODE" => QrCode,
            "UPC_A" => UpcA,
            "UPC_E" => UpcE,
            _ => return None,
        })
    }
}

/// Matches a free-text barcode-type label (e.g. from a column named
/// "type"/"format"/"barcodeformat") against a BarcodeType. Separate
/// from BarcodeType::from_catima_name, which matches Catima's exact
/// fixed strings; this normalizes first and matches a broader set of
/// synonyms per type. An unrecognized value falls through to None,
/// letting the caller fall back to guess_barcode_type_from_value.
fn guess_barcode_type_from_label(label: &str) -> Option<BarcodeType> {
    let normalized = normalize_label(label);
    Some(match normalized.as_str() {
        "qr" | "qrcode" => BarcodeType::QrCode,
        "code128" => BarcodeType::Code128,
        "code39" => BarcodeType::Code39,
        "code93" => BarcodeType::Code93,
        "codabar" => BarcodeType::Codabar,
        "ean8" => BarcodeType::Ean8,
        "ean13" => BarcodeType::Ean13,
        "itf" | "itf14" | "interleaved2of5" => BarcodeType::Itf,
        "pdf417" => BarcodeType::Pdf417,
        "upca" => BarcodeType::UpcA,
        "upce" => BarcodeType::UpcE,
        "aztec" => BarcodeType::Aztec,
        "datamatrix" => BarcodeType::DataMatrix,
        _ => return None,
    })
}


/// What to do once a card's expiry passes, applied by
/// CardStore::apply_expiration_actions. Not enforced live; acted on
/// the next time that runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OnExpiration {
    #[default]
    DoNothing,
    Archive,
    Delete,
}

impl OnExpiration {
    pub fn all() -> &'static [OnExpiration] {
        &[
            OnExpiration::DoNothing,
            OnExpiration::Archive,
            OnExpiration::Delete,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            OnExpiration::DoNothing => "Do Nothing",
            OnExpiration::Archive => "Archive",
            OnExpiration::Delete => "Delete",
        }
    }
}

/// A named tag cards can be sorted into. Has its own stable id so
/// renaming or reordering a group never touches card data; cards
/// reference groups by id (see Card::group_ids), not name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
}

/// A single loyalty / membership card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub id: Uuid,
    pub name: String,
    pub barcode_type: BarcodeType,
    /// The raw value encoded in the barcode/QR (ignored for NoCode).
    pub code_value: String,
    /// Hex color used for the card's swatch/accent, e.g. "#3584e4".
    pub color: String,
    pub notes: String,
    /// Starred cards sort first (see search). serde(default) so a
    /// cards.json saved before this field existed still deserializes.
    #[serde(default)]
    pub starred: bool,
    /// Shown as an external-link icon next to the card's name in the
    /// detail view. serde(default) for the same reason as `starred`.
    #[serde(default)]
    pub link: Option<String>,
    /// ISO 8601 "YYYY-MM-DD". Lexicographic string comparison against
    /// today's ISO date is a correct expiry check, no date parsing
    /// needed. Not validated on save; an unparseable value just won't
    /// participate correctly in the expired/upcoming logic.
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub expiry: Option<String>,
    /// Only meaningful when `expiry` is set. Kept even if `expiry` is
    /// later cleared rather than reset; the edit dialog just shows it
    /// disabled.
    #[serde(default)]
    pub on_expiration: OnExpiration,
    /// Referenced by id, not name, so renaming/reordering a Group
    /// doesn't touch this. A stale id (group deleted) is ignored
    /// wherever this is read; CardStore::remove_group also sweeps
    /// deleted ids out of every card.
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
    /// Hidden from the main grid by default without being deleted.
    /// Opposite of starring, which pins a card to the top instead.
    #[serde(default)]
    pub archived: bool,
}

impl Card {
    pub fn new(name: impl Into<String>) -> Self {
        Card {
            id: Uuid::new_v4(),
            name: name.into(),
            barcode_type: BarcodeType::QrCode,
            code_value: String::new(),
            color: "#3584e4".to_string(),
            notes: String::new(),
            starred: false,
            link: None,
            valid_from: None,
            expiry: None,
            on_expiration: OnExpiration::DoNothing,
            group_ids: Vec::new(),
            archived: false,
        }
    }

    /// `today` must be non-empty and ISO-formatted. An empty string
    /// would incorrectly compare as "before" every set `valid_from`,
    /// since any non-empty date string sorts greater than an empty
    /// one; callers with an empty/unknown `today` need to check for
    /// that themselves (see search's use of this).
    pub fn is_valid(&self, today: &str) -> bool {
        let not_yet_valid = self.valid_from.as_deref().map(|d| d > today).unwrap_or(false);
        let already_expired = self.expiry.as_deref().map(|d| d < today).unwrap_or(false);
        !not_yet_valid && !already_expired
    }
}

/// Outcome of an import: how many rows became new cards, and how many
/// were recognized as duplicates (of an existing card, or of an earlier
/// row in the same import) and folded into the existing card instead of
/// added as a second copy. See `CardStore::append_imported`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportSummary {
    pub imported: usize,
    pub duplicates: usize,
}

/// Imported: a confidently-recognized format, already saved.
/// NeedsFallbackConfirmation: candidate cards from try_fallback_parse,
/// not yet saved. The caller (window.rs) shows a confirmation dialog
/// and only calls CardStore::append_imported if the user confirms.
pub enum ImportOutcome {
    Imported(ImportSummary),
    NeedsFallbackConfirmation(Vec<Card>),
}

/// Small bits of UI state persisted across runs (last toggle states,
/// last group tab). Separate file from cards.json, at
/// dirs::state_dir() ($XDG_STATE_HOME, falls back to ~/.local/state)
/// rather than data_dir(), per the XDG distinction between state and
/// real user data.
#[derive(Serialize, Deserialize, Default)]
pub struct UiState {
    pub hide_archived: bool,
    pub hide_invalid: bool,
    pub search_bar_visible: bool,
    pub selected_group: Option<Uuid>,
}

impl UiState {
    /// `$XDG_STATE_HOME/barkeep/ui-state.json` (falls back to
    /// `~/.local/state`). Missing or unreadable falls back to Default
    /// rather than erroring.
    pub fn open_default() -> Self {
        let Some(mut dir) = dirs::state_dir() else {
            return Self::default();
        };
        dir.push("barkeep");
        if fs::create_dir_all(&dir).is_err() {
            return Self::default();
        }
        dir.push("ui-state.json");
        let Ok(data) = fs::read_to_string(&dir) else {
            return Self::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    /// Same location open_default() reads from. Errors silently
    /// ignored.
    pub fn save(&self) {
        let Some(mut dir) = dirs::state_dir() else { return };
        dir.push("barkeep");
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        dir.push("ui-state.json");
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = fs::write(&dir, data);
        }
    }
}

/// The on-disk shape of cards.json, kept separate from CardStore
/// itself since CardStore also carries `path`, which doesn't belong in
/// the saved JSON.
#[derive(Serialize, Deserialize, Default)]
struct CardStoreData {
    #[serde(default)]
    cards: Vec<Card>,
    #[serde(default)]
    groups: Vec<Group>,
}

/// Same shape as CardStoreData, but borrowing rather than owning, so
/// save() doesn't need to clone the collections just to hand them to
/// serde_json.
#[derive(Serialize)]
struct CardStoreDataRef<'a> {
    cards: &'a [Card],
    groups: &'a [Group],
}

/// Loads, holds, and persists the full card collection plus groups
/// together as one JSON file. One file rather than two lets
/// remove_group persist its group removal and per-card group_ids
/// sweep as a single atomic write.
///
/// Clone is needed for mutate_then_save below, which clones the whole
/// store, mutates the clone, and only swaps it into `self` once the
/// clone's save() succeeds.
#[derive(Clone)]
pub struct CardStore {
    path: PathBuf,
    pub cards: Vec<Card>,
    pub groups: Vec<Group>,
}

impl CardStore {
    /// `$XDG_DATA_HOME/barkeep/cards.json` (falls back to
    /// `~/.local/share`). Shared by open_default below and
    /// load_read_only's callers (the lock-screen plugin, a separate
    /// process resolving the same path independently), so the two
    /// consumers can't drift onto different ideas of where the file
    /// lives.
    pub fn default_path() -> io::Result<PathBuf> {
        let mut dir = dirs::data_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no data directory available")
        })?;
        dir.push("barkeep");
        fs::create_dir_all(&dir)?;
        dir.push("cards.json");
        Ok(dir)
    }

    /// Opens (and creates if needed) the store at the default XDG data location:
    /// `$XDG_DATA_HOME/barkeep/cards.json` (falls back to `~/.local/share`).
    pub fn open_default() -> io::Result<(Self, Vec<String>)> {
        Self::open(Self::default_path()?)
    }

    /// Defaults to `T::default()` if the file doesn't exist. A genuine
    /// I/O failure still propagates via `?`; this only changes what
    /// happens on a parse failure. Previously `.unwrap_or_default()`,
    /// which silently started the app with an empty collection and let
    /// the next save() overwrite the still-recoverable original file.
    /// Now the original is copied to `<path>.bak` first, and a warning
    /// is pushed onto `warnings` so the caller can tell the user their
    /// data needed recovering.
    fn read_json_with_backup_on_corruption<T: Default + serde::de::DeserializeOwned>(
        path: &Path,
        warnings: &mut Vec<String>,
    ) -> io::Result<T> {
        if !path.exists() {
            return Ok(T::default());
        }
        let data = fs::read_to_string(path)?;
        match serde_json::from_str(&data) {
            Ok(value) => Ok(value),
            Err(parse_error) => {
                let backup_path = path.with_extension("json.bak");
                let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("this file");
                match fs::write(&backup_path, &data) {
                    Ok(()) => {
                        warnings.push(format!(
                            "{filename} appeared damaged ({parse_error}) and has been backed up to {} before starting fresh.",
                            backup_path.file_name().and_then(|f| f.to_str()).unwrap_or("a backup file")
                        ));
                    }
                    Err(backup_error) => {
                        warnings.push(format!(
                            "{filename} appeared damaged ({parse_error}) and starting fresh, but backing it up first also failed ({backup_error}) -- the damaged file is still at its original location, untouched, in case it's recoverable by other means."
                        ));
                    }
                }
                Ok(T::default())
            }
        }
    }

    pub fn open(path: PathBuf) -> io::Result<(Self, Vec<String>)> {
        let mut warnings = Vec::new();
        let data: CardStoreData = Self::read_json_with_backup_on_corruption(&path, &mut warnings)?;
        Ok((
            CardStore {
                path,
                cards: data.cards,
                groups: data.groups,
            },
            warnings,
        ))
    }

    /// No backup-on-corruption/repair attempt, for the lock-screen
    /// plugin, which reads the same file the main app writes to but
    /// shouldn't try to repair it as a separate process. `None` means a
    /// genuine read/parse failure on a file that exists; the plugin
    /// treats that as "couldn't load cards" rather than attempting
    /// repair. A missing file returns `Some` with empty collections.
    pub fn load_read_only(path: &Path) -> Option<Self> {
        if !path.exists() {
            return Some(CardStore {
                path: path.to_path_buf(),
                cards: Vec::new(),
                groups: Vec::new(),
            });
        }
        let raw = fs::read_to_string(path).ok()?;
        let data: CardStoreData = serde_json::from_str(&raw).ok()?;
        Some(CardStore {
            path: path.to_path_buf(),
            cards: data.cards,
            groups: data.groups,
        })
    }

    pub fn save(&self) -> io::Result<()> {
        let data = CardStoreDataRef {
            cards: &self.cards,
            groups: &self.groups,
        };
        let json = serde_json::to_string_pretty(&data)?;
        fs::write(&self.path, json)
    }

    /// Runs `mutate` against a clone, saves the clone, and only swaps
    /// it into `self` if the save succeeds. On failure `self` is never
    /// touched, so in-memory and on-disk state can't disagree the way
    /// they could when mutation and save were separate steps. Every
    /// mutation method below goes through this.
    fn mutate_then_save<T>(&mut self, mutate: impl FnOnce(&mut Self) -> T) -> io::Result<T> {
        let mut clone = self.clone();
        let result = mutate(&mut clone);
        clone.save()?;
        *self = clone;
        Ok(result)
    }

    /// Creates a new group, appended at the end of the current order.
    /// Returns its freshly-assigned id.
    pub fn add_group(&mut self, name: impl Into<String>) -> io::Result<Uuid> {
        let name = name.into();
        self.mutate_then_save(move |store| {
            let id = Uuid::new_v4();
            store.groups.push(Group { id, name });
            id
        })
    }

    pub fn rename_group(&mut self, id: Uuid, new_name: impl Into<String>) -> io::Result<()> {
        let new_name = new_name.into();
        self.mutate_then_save(move |store| {
            if let Some(group) = store.groups.iter_mut().find(|g| g.id == id) {
                group.name = new_name;
            }
        })
    }

    /// Sweeps the removed group's id out of every card that referenced
    /// it. Both changes persist together in mutate_then_save's single
    /// save() call.
    pub fn remove_group(&mut self, id: Uuid) -> io::Result<()> {
        self.mutate_then_save(move |store| {
            store.groups.retain(|g| g.id != id);
            for card in store.cards.iter_mut() {
                card.group_ids.retain(|g| *g != id);
            }
        })
    }

    /// Used by the Manage Groups dialog's drag-to-reorder. Any id in
    /// `ordered_ids` not matching a real group is skipped; any real
    /// group not mentioned keeps its existing relative position,
    /// appended after everything explicitly ordered.
    pub fn set_group_order(&mut self, ordered_ids: &[Uuid]) -> io::Result<()> {
        self.mutate_then_save(|store| {
            let mut reordered: Vec<Group> = Vec::with_capacity(store.groups.len());
            for id in ordered_ids {
                if let Some(pos) = store.groups.iter().position(|g| g.id == *id) {
                    reordered.push(store.groups.remove(pos));
                }
            }
            reordered.extend(store.groups.drain(..));
            store.groups = reordered;
        })
    }

    pub fn add(&mut self, card: Card) -> io::Result<()> {
        self.mutate_then_save(move |store| {
            store.cards.push(card);
        })
    }

    pub fn update(&mut self, card: Card) -> io::Result<()> {
        self.mutate_then_save(move |store| {
            if let Some(existing) = store.cards.iter_mut().find(|c| c.id == card.id) {
                *existing = card;
            }
        })
    }

    pub fn remove(&mut self, id: Uuid) -> io::Result<()> {
        self.mutate_then_save(move |store| {
            store.cards.retain(|c| c.id != id);
        })
    }

    /// Same as remove, but for multiple ids: saves once, not once per
    /// card.
    pub fn remove_bulk(&mut self, ids: &[Uuid]) -> io::Result<()> {
        self.mutate_then_save(|store| {
            store.cards.retain(|c| !ids.contains(&c.id));
        })
    }

    /// For each `(group_id, should_be_member)` pair, adds or removes
    /// that group from every card in `card_ids`. A group not mentioned
    /// in `changes` is left untouched, matching the bulk "Change
    /// Groups" dialog's tri-state semantics.
    pub fn apply_group_changes_bulk(&mut self, card_ids: &[Uuid], changes: &[(Uuid, bool)]) -> io::Result<()> {
        self.mutate_then_save(|store| {
            for card in store.cards.iter_mut() {
                if !card_ids.contains(&card.id) {
                    continue;
                }
                for &(group_id, should_be_member) in changes {
                    if should_be_member {
                        if !card.group_ids.contains(&group_id) {
                            card.group_ids.push(group_id);
                        }
                    } else {
                        card.group_ids.retain(|id| *id != group_id);
                    }
                }
            }
        })
    }

    /// Flips a card's starred state and persists it. A no-op (still
    /// saves) if `id` isn't found, matching `update`'s existing
    /// not-found handling elsewhere in this file.
    pub fn toggle_star(&mut self, id: Uuid) -> io::Result<()> {
        self.mutate_then_save(move |store| {
            if let Some(card) = store.cards.iter_mut().find(|c| c.id == id) {
                card.starred = !card.starred;
            }
        })
    }

    pub fn toggle_archived(&mut self, id: Uuid) -> io::Result<()> {
        self.mutate_then_save(move |store| {
            if let Some(card) = store.cards.iter_mut().find(|c| c.id == id) {
                card.archived = !card.archived;
            }
        })
    }

    /// Explicit state, not a toggle, for bulk archive/unarchive: every
    /// selected card ends up in the same state regardless of its
    /// starting state.
    pub fn set_archived_bulk(&mut self, ids: &[Uuid], archived: bool) -> io::Result<()> {
        self.mutate_then_save(|store| {
            for card in store.cards.iter_mut() {
                if ids.contains(&card.id) {
                    card.archived = archived;
                }
            }
        })
    }

    /// Same as set_archived_bulk, for starring.
    pub fn set_starred_bulk(&mut self, ids: &[Uuid], starred: bool) -> io::Result<()> {
        self.mutate_then_save(|store| {
            for card in store.cards.iter_mut() {
                if ids.contains(&card.id) {
                    card.starred = starred;
                }
            }
        })
    }

    /// Called once at startup and again at every local midnight (see
    /// build_window's scheduling). `today` is caller-supplied so this
    /// module stays decoupled from glib for the clock. Returns whether
    /// anything changed, so callers only rebuild the grid when needed.
    ///
    /// Not routed through mutate_then_save, since that always attempts
    /// a save; this needs to skip saving entirely when nothing
    /// expired, or a persistent disk problem would surface a "Save
    /// Failed" notification every midnight for nothing.
    pub fn apply_expiration_actions(&mut self, today: &str) -> io::Result<bool> {
        if today.is_empty() {
            return Ok(false);
        }

        let mut clone = self.clone();
        let mut changed = false;
        let mut to_delete = Vec::new();
        for card in clone.cards.iter_mut() {
            let Some(expiry) = &card.expiry else { continue };
            if expiry.as_str() >= today {
                continue;
            }
            match card.on_expiration {
                OnExpiration::DoNothing => {}
                OnExpiration::Archive => {
                    if !card.archived {
                        card.archived = true;
                        changed = true;
                    }
                }
                OnExpiration::Delete => {
                    to_delete.push(card.id);
                    changed = true;
                }
            }
        }
        if !to_delete.is_empty() {
            clone.cards.retain(|c| !to_delete.contains(&c.id));
        }
        if !changed {
            return Ok(false);
        }
        clone.save()?;
        *self = clone;
        Ok(true)
    }

    /// `group_filter` is `None` for "All", or a specific group's id.
    /// All filters combine with AND semantics alongside the text
    /// query. Search text matches name, notes, or (by resolving
    /// group_ids) any group the card belongs to.
    pub fn search(
        &self,
        query: &str,
        show_archived: bool,
        valid_only: bool,
        today: &str,
        group_filter: Option<Uuid>,
    ) -> Vec<&Card> {
        let q = query.to_lowercase();
        let mut results: Vec<&Card> = self
            .cards
            .iter()
            .filter(|c| show_archived || !c.archived)
            .filter(|c| !valid_only || today.is_empty() || c.is_valid(today))
            .filter(|c| match group_filter {
                None => true,
                Some(id) => c.group_ids.contains(&id),
            })
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || c.notes.to_lowercase().contains(&q)
                    || c.group_ids.iter().any(|gid| {
                        self.groups
                            .iter()
                            .find(|g| g.id == *gid)
                            .map(|g| g.name.to_lowercase().contains(&q))
                            .unwrap_or(false)
                    })
            })
            .collect();
        // Starred cards first, alphabetical within each group. `!starred`
        // sorts `true` (was starred) before `false` since Rust's bool
        // ordering is false < true, so negating it puts starred cards
        // first without a custom Ordering match.
        results.sort_by_key(|c| (!c.starred, c.name.to_lowercase()));
        results
    }

    /// Exports the store as a bare CSV, in Catima's card-table column
    /// layout, without the zip wrapper or `catima.csv` internal
    /// filename Catima uses. `balance`/`balancetype` (Catima features
    /// Barkeep has no equivalent for) are left blank. `group_ids`/
    /// `archived` have no matching Catima columns, so they only
    /// round-trip through Barkeep's own JSON export.
    pub fn export_to(&self, dest: &Path) -> io::Result<()> {
        let mut writer = csv::Writer::from_path(dest)?;
        writer
            .write_record([
                "_id",
                "store",
                "note",
                "validfrom",
                "expiry",
                "balance",
                "balancetype",
                "cardid",
                "barcodeid",
                "barcodetype",
                "headercolor",
            ])
            .map_err(csv_io_err)?;
        for (i, card) in self.cards.iter().enumerate() {
            writer
                .write_record([
                    i.to_string(),
                    card.name.clone(),
                    card.notes.clone(),
                    card.valid_from.clone().unwrap_or_default(),
                    card.expiry.clone().unwrap_or_default(),
                    String::new(),
                    String::new(),
                    card.code_value.clone(),
                    card.code_value.clone(),
                    card.barcode_type.to_catima_name().to_string(),
                    hex_to_signed_argb(&card.color).to_string(),
                ])
                .map_err(csv_io_err)?;
        }
        writer.flush()?;
        Ok(())
    }

    /// Imports cards from a file of unknown provenance, auto-detecting
    /// one of four formats: a Barkeep export, a Catima backup (.zip or
    /// bare .csv), a LoyaltyCardKeychain CSV export, or a UBcards
    /// wallet.ini.
    ///
    /// A bare .csv doesn't say which CSV format it is; detected by
    /// sniffing the header row.
    ///
    /// Read as raw bytes and lossily decoded rather than
    /// fs::read_to_string, since wallet.ini can contain invalid UTF-8.
    ///
    /// Password-protected Catima exports aren't supported.
    ///
    /// Extension checks (pkpass/zip) are a fast path; otherwise magic
    /// bytes are checked for a zip regardless of extension, since some
    /// apps export a zip with the extension renamed or dropped.
    pub fn import_any(&mut self, path: &Path) -> Result<ImportOutcome, String> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        // .pkpass is a zip (Apple Wallet's container format) with a
        // different internal layout, so it needs its own check before
        // the generic zip branch.
        if extension.as_deref() == Some("pkpass") {
            return self.import_pkpass(path).map(ImportOutcome::Imported);
        }
        if extension.as_deref() == Some("zip") {
            return self.import_zip(path).map(ImportOutcome::Imported);
        }

        let bytes = fs::read(path).map_err(|e| e.to_string())?;

        if bytes.starts_with(b"PK\x03\x04") {
            return self.try_import_from_archive(&bytes);
        }

        self.import_bytes(&bytes)
    }

    /// Extracted out of import_any so the same logic runs against each
    /// file found inside an archive (see try_import_from_archive), not
    /// just a standalone file.
    ///
    /// Matches by each card's exact id, since this is Barkeep's own
    /// export format re-imported into itself. Returns how many cards
    /// were processed; this format doesn't distinguish matched from
    /// newly added.
    fn merge_barkeep_export(&mut self, imported: Vec<Card>) -> io::Result<usize> {
        self.mutate_then_save(move |store| {
            let count = imported.len();
            for card in imported {
                match store.cards.iter_mut().find(|c| c.id == card.id) {
                    Some(existing) => *existing = card,
                    None => store.cards.push(card),
                }
            }
            count
        })
    }

    fn import_bytes(&mut self, bytes: &[u8]) -> Result<ImportOutcome, String> {
        let text = String::from_utf8_lossy(bytes);

        // Checked before the Barkeep-JSON attempt: both are bare JSON
        // arrays starting with '[', so a Voucher Vault export would
        // otherwise fail with a confusing error instead of reaching
        // its own parser. codeType is Voucher Vault's field name, not
        // Card's, so this doesn't misroute a genuine Barkeep export.
        if text.trim_start().starts_with('[') && text.contains("codeType") {
            return self.append_imported(parse_vouchervault_json(&text)?).map(ImportOutcome::Imported);
        }

        if text.trim_start().starts_with('[') && !text.contains("[General]") {
            let imported: Vec<Card> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let count = self.merge_barkeep_export(imported).map_err(|e| e.to_string())?;
            // Matched and merged by id above already, so there's nothing
            // left over to count as a duplicate the way the CSV formats
            // below can have.
            return Ok(ImportOutcome::Imported(ImportSummary { imported: count, duplicates: 0 }));
        }

        if text.contains(CATIMA_CARD_HEADER_PREFIX) {
            return self.append_imported(parse_catima_csv(&text)?).map(ImportOutcome::Imported);
        }

        if text.contains(LOYALTYCARDKEYCHAIN_HEADER) {
            return self.append_imported(parse_loyaltycardkeychain_csv(&text)?).map(ImportOutcome::Imported);
        }

        if text.contains("[General]") && text.contains("all=") {
            return self.append_imported(parse_ubcards_wallet_ini(&text)).map(ImportOutcome::Imported);
        }

        // Last resort before giving up; see
        // try_fallback_parse's own doc comment for the shape-detection
        // order. Deliberately not auto-applied: the caller is expected
        // to confirm with the user first (see ImportOutcome).
        if let Some(cards) = try_fallback_parse(&text) {
            return Ok(ImportOutcome::NeedsFallbackConfirmation(cards));
        }

        Err("Unrecognized file format -- Barkeep can import its own export (.json), a Catima backup (.zip or .csv), a LoyaltyCardKeychain export (.csv), a UBcards wallet.ini, a Voucher Vault export (.json), or an Apple Wallet pass (.pkpass).".to_string())
    }

    /// Imports a single Apple Wallet pass. `.pkpass` is a zip containing
    /// (among other things) a `pass.json` and a cryptographic signature
    /// over the whole package. Signature verification is skipped;
    /// only extracting card data, not authenticating the issuer.
    ///
    /// The barcode lives in `barcodes` (current passes) or the older
    /// singular `barcode` object (pre-iOS 9), checked in that order.
    /// `backgroundColor` is CSS `rgb(r, g, b)`, not hex, and needs
    /// converting. `expirationDate` maps to `expiry`, truncated to the
    /// date.
    ///
    /// Exactly one of the five style dictionaries
    /// (boardingPass/coupon/eventTicket/storeCard/generic) holds the
    /// display fields, nested inside it rather than at the top level.
    /// That structured data, plus `voided`/`logoText`/`relevantDate`,
    /// has no dedicated Card field, so it's summarized into `notes`.
    ///
    /// `relevantDate` is NOT mapped to `valid_from` despite the
    /// surface similarity: it's a notification-surfacing hint (when
    /// iOS should show the pass), not a validity constraint. A
    /// boarding pass's relevantDate is often set shortly before
    /// departure while the pass itself is valid from check-in;
    /// mapping it to valid_from would make "Hide Presently Invalid
    /// Cards" hide a usable pass.
    fn import_pkpass(&mut self, path: &Path) -> Result<ImportSummary, String> {
        #[derive(serde::Deserialize)]
        struct PkPassBarcode {
            message: Option<String>,
            format: Option<String>,
        }

        #[derive(serde::Deserialize)]
        struct PkPassField {
            key: Option<String>,
            label: Option<String>,
            value: Option<serde_json::Value>,
        }

        #[derive(serde::Deserialize, Default)]
        struct PkPassStyleFields {
            #[serde(rename = "headerFields")]
            header_fields: Option<Vec<PkPassField>>,
            #[serde(rename = "primaryFields")]
            primary_fields: Option<Vec<PkPassField>>,
            #[serde(rename = "secondaryFields")]
            secondary_fields: Option<Vec<PkPassField>>,
            #[serde(rename = "auxiliaryFields")]
            auxiliary_fields: Option<Vec<PkPassField>>,
            #[serde(rename = "backFields")]
            back_fields: Option<Vec<PkPassField>>,
        }

        #[derive(serde::Deserialize)]
        struct PkPassJson {
            description: Option<String>,
            #[serde(rename = "organizationName")]
            organization_name: Option<String>,
            #[serde(rename = "backgroundColor")]
            background_color: Option<String>,
            barcodes: Option<Vec<PkPassBarcode>>,
            barcode: Option<PkPassBarcode>,
            #[serde(rename = "expirationDate")]
            expiration_date: Option<String>,
            voided: Option<bool>,
            #[serde(rename = "logoText")]
            logo_text: Option<String>,
            #[serde(rename = "relevantDate")]
            relevant_date: Option<String>,
            #[serde(rename = "boardingPass")]
            boarding_pass: Option<PkPassStyleFields>,
            coupon: Option<PkPassStyleFields>,
            #[serde(rename = "eventTicket")]
            event_ticket: Option<PkPassStyleFields>,
            #[serde(rename = "storeCard")]
            store_card: Option<PkPassStyleFields>,
            generic: Option<PkPassStyleFields>,
        }

        /// Formats a field's "value" (string, number, or bool) as
        /// display text. None for anything else.
        fn pkpass_scalar_to_string(value: &serde_json::Value) -> Option<String> {
            match value {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                serde_json::Value::Bool(b) => Some(b.to_string()),
                _ => None,
            }
        }

        /// Joins a field array (e.g. primaryFields) into one
        /// "label: value, label: value" line, falling back to `key`
        /// when `label` is absent.
        fn format_pkpass_fields(fields: &Option<Vec<PkPassField>>) -> Option<String> {
            let parts: Vec<String> = fields
                .as_ref()?
                .iter()
                .filter_map(|f| {
                    let value = f.value.as_ref().and_then(pkpass_scalar_to_string)?;
                    match f.label.as_deref().or(f.key.as_deref()) {
                        Some(label) if !label.is_empty() => Some(format!("{label}: {value}")),
                        _ => Some(value),
                    }
                })
                .collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }

        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let mut pass_json = String::new();
        archive
            .by_name("pass.json")
            .map_err(|_| "this .pkpass has no pass.json inside it".to_string())?
            .read_to_string(&mut pass_json)
            .map_err(|e| e.to_string())?;

        let pass: PkPassJson = serde_json::from_str(&pass_json).map_err(|e| e.to_string())?;

        let name = pass
            .description
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| pass.organization_name.clone())
            .unwrap_or_else(|| "Imported card".to_string());

        let barcode = pass
            .barcodes
            .as_ref()
            .and_then(|list| list.first())
            .or(pass.barcode.as_ref());
        let (code_value, barcode_type) = match barcode {
            Some(b) => (
                b.message.clone().unwrap_or_default(),
                b.format
                    .as_deref()
                    .and_then(pkpass_barcode_format_to_barcode_type)
                    .unwrap_or(BarcodeType::NoCode),
            ),
            None => (String::new(), BarcodeType::NoCode),
        };

        let color = pass
            .background_color
            .as_deref()
            .and_then(parse_css_rgb_color)
            .unwrap_or_else(|| stable_color_for(&name));

        // Exactly one of these five should be present, per spec;
        // whichever it is names the pass's style and holds its
        // display fields.
        let style: Option<(&str, &PkPassStyleFields)> = pass
            .boarding_pass
            .as_ref()
            .map(|f| ("Boarding Pass", f))
            .or_else(|| pass.coupon.as_ref().map(|f| ("Coupon", f)))
            .or_else(|| pass.event_ticket.as_ref().map(|f| ("Event Ticket", f)))
            .or_else(|| pass.store_card.as_ref().map(|f| ("Store Card", f)))
            .or_else(|| pass.generic.as_ref().map(|f| ("Generic", f)));

        let mut extra_notes: Vec<String> = Vec::new();
        if let Some(voided) = pass.voided {
            extra_notes.push(format!("Voided: {voided}"));
        }
        if let Some((style_name, style_fields)) = style {
            extra_notes.push(format!("Pass Style: {style_name}"));
            if let Some(s) = format_pkpass_fields(&style_fields.header_fields) {
                extra_notes.push(format!("Header: {s}"));
            }
            if let Some(s) = format_pkpass_fields(&style_fields.primary_fields) {
                extra_notes.push(format!("Primary: {s}"));
            }
            if let Some(s) = format_pkpass_fields(&style_fields.secondary_fields) {
                extra_notes.push(format!("Secondary: {s}"));
            }
            if let Some(s) = format_pkpass_fields(&style_fields.auxiliary_fields) {
                extra_notes.push(format!("Auxiliary: {s}"));
            }
            if let Some(s) = format_pkpass_fields(&style_fields.back_fields) {
                extra_notes.push(format!("Back: {s}"));
            }
        }
        if let Some(logo_text) = pass.logo_text.filter(|s| !s.is_empty()) {
            extra_notes.push(format!("Logo Text: {logo_text}"));
        }
        if let Some(relevant_date) = pass.relevant_date {
            extra_notes.push(format!("Relevant Date: {relevant_date}"));
        }

        let mut card = Card::new(name);
        card.code_value = code_value;
        card.barcode_type = barcode_type;
        card.color = color;
        // expiry doesn't carry time-of-day.
        card.expiry = pass.expiration_date.map(|e| e.chars().take(10).collect());
        card.notes = extra_notes.join("\n");

        self.append_imported(vec![card])
    }

    /// Extracts every file from a zip detected by its magic bytes and
    /// tries each one.
    ///
    /// catima.csv and loyalty_programs.csv are checked by filename
    /// first: FidMe's CSV has no recognizable header text the way
    /// Catima's does, so it's only detectable by this known filename.
    ///
    /// Beyond those two, a confidently-recognized format in any file
    /// wins over a low-confidence fallback match in an earlier one
    /// (e.g. a README sitting before the real data file). Known
    /// limitation: if an unrelated file false-positive matches a known
    /// format before the real data file is reached, that false match
    /// wins outright, since import_bytes already saves as part of
    /// matching.
    fn try_import_from_archive(&mut self, bytes: &[u8]) -> Result<ImportOutcome, String> {
        let files = extract_zip_files(bytes)?;

        if let Some((_, catima_bytes)) = files.iter().find(|(name, _)| name == "catima.csv") {
            let text = String::from_utf8_lossy(catima_bytes).into_owned();
            return self.append_imported(parse_catima_csv(&text)?).map(ImportOutcome::Imported);
        }
        if let Some((_, fidme_bytes)) = files.iter().find(|(name, _)| name == "loyalty_programs.csv") {
            let text = String::from_utf8_lossy(fidme_bytes).into_owned();
            return self.append_imported(parse_fidme_csv(&text)).map(ImportOutcome::Imported);
        }

        let mut fallback_result: Option<Vec<Card>> = None;
        let mut last_error = "Archive contained no recognizable files.".to_string();
        for (_, file_bytes) in &files {
            match self.import_bytes(file_bytes) {
                Ok(ImportOutcome::Imported(summary)) => return Ok(ImportOutcome::Imported(summary)),
                Ok(ImportOutcome::NeedsFallbackConfirmation(cards)) => {
                    if fallback_result.is_none() {
                        fallback_result = Some(cards);
                    }
                }
                Err(e) => last_error = e,
            }
        }

        match fallback_result {
            Some(cards) => Ok(ImportOutcome::NeedsFallbackConfirmation(cards)),
            None => Err(last_error),
        }
    }

    /// Catima and FidMe both export a plain `.zip`, so the extension
    /// alone doesn't say which; opens the archive and checks which of
    /// the two internal filenames is present.
    fn import_zip(&mut self, path: &Path) -> Result<ImportSummary, String> {
        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

        if archive.by_name("catima.csv").is_ok() {
            let mut csv_text = String::new();
            archive
                .by_name("catima.csv")
                .map_err(|e| e.to_string())?
                .read_to_string(&mut csv_text)
                .map_err(|e| e.to_string())?;
            return self.append_imported(parse_catima_csv(&csv_text)?);
        }

        if archive.by_name("loyalty_programs.csv").is_ok() {
            let mut csv_text = String::new();
            archive
                .by_name("loyalty_programs.csv")
                .map_err(|e| e.to_string())?
                .read_to_string(&mut csv_text)
                .map_err(|e| e.to_string())?;
            return self.append_imported(parse_fidme_csv(&csv_text));
        }

        Err("couldn't find catima.csv or loyalty_programs.csv inside the zip -- password-protected exports aren't supported, and this doesn't look like a Catima or FidMe backup".to_string())
    }

    /// Unlike the JSON import path, these formats have no id to merge
    /// on, so duplicates are matched by (barcode type, code value)
    /// instead, against both the existing collection and earlier rows
    /// from this same import (Catima/LoyaltyCardKeychain exports can
    /// contain duplicate rows, e.g. a card exported once per group).
    /// A match is folded into the existing card via `merge_into`
    /// rather than added again.
    ///
    /// Cards with no code value have nothing reliable to match on, so
    /// they're always added as new.
    ///
    /// Public so window.rs can call this directly once the user
    /// confirms a fallback-parsed import.
    pub fn append_imported(&mut self, cards: Vec<Card>) -> Result<ImportSummary, String> {
        self.mutate_then_save(move |store| {
            let mut summary = ImportSummary::default();
            for card in cards {
                let existing_index = if card.code_value.is_empty() {
                    None
                } else {
                    store
                        .cards
                        .iter()
                        .position(|c| c.barcode_type == card.barcode_type && c.code_value == card.code_value)
                };
                match existing_index {
                    Some(i) => {
                        merge_into(&mut store.cards[i], &card);
                        summary.duplicates += 1;
                    }
                    None => {
                        store.cards.push(card);
                        summary.imported += 1;
                    }
                }
            }
            summary
        })
        .map_err(|e| e.to_string())
    }
}

/// Fills in whatever the existing card is missing from a duplicate,
/// without overwriting what it already has. Currently just notes.
fn merge_into(existing: &mut Card, imported: &Card) {
    if existing.notes.is_empty() && !imported.notes.is_empty() {
        existing.notes = imported.notes.clone();
    }
}

fn csv_io_err(e: csv::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e)
}

/// Inverse of parse_signed_argb_color: packs a "#RRGGBB" hex string
/// back into Catima's signed-32-bit-ARGB representation (opaque,
/// alpha=FF).
fn hex_to_signed_argb(hex: &str) -> i64 {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    let argb = 0xFF00_0000u32 | rgb;
    argb as i32 as i64
}

/// A prefix of Catima's card table header, used to locate where card
/// rows start in the combined groups/cards/links CSV. Just a prefix,
/// not the full column list: real exports have grown an extra
/// `barcodeencoding` column since this was first written, and matching
/// only the stable leading columns avoids breaking again next time.
/// See: https://github.com/CatimaLoyalty/Android/wiki/Export-format
const CATIMA_CARD_HEADER_PREFIX: &str =
    "_id,store,note,validfrom,expiry,balance,balancetype,cardid,barcodeid,barcodetype";

/// LoyaltyCardKeychain's CSV export header, in full (its export is just
/// the card table, no groups/links sections to skip past). Shares
/// Catima's column naming for every field Barkeep reads, minus
/// `barcodeid`; `parse_card_table_csv` falls back to `cardid` when
/// `barcodeid` isn't present.
const LOYALTYCARDKEYCHAIN_HEADER: &str =
    "_id,store,note,cardid,headercolor,headertextcolor,barcodetype";

/// Extracts every regular file's name and raw contents out of a zip.
/// Directory entries are skipped; nothing to import there.
fn extract_zip_files(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).map_err(|e| e.to_string())?;
        files.push((name, contents));
    }
    Ok(files)
}

fn parse_catima_csv(text: &str) -> Result<Vec<Card>, String> {
    parse_card_table_csv(text, CATIMA_CARD_HEADER_PREFIX, "Catima")
}

fn parse_loyaltycardkeychain_csv(text: &str) -> Result<Vec<Card>, String> {
    parse_card_table_csv(text, LOYALTYCARDKEYCHAIN_HEADER, "LoyaltyCardKeychain")
}

/// Guesses a barcode format from the encoded value's shape, for
/// sources (currently just FidMe) with no type information at all.
/// EAN/UPC/ITF are strictly digit-only with fixed lengths; ITF
/// specifically requires an even digit count, since it encodes digits
/// in pairs. QR/Aztec/DataMatrix/PDF417 are excluded: there's no
/// comparable content-shape signal for those, so guessing among them
/// would be a coin flip.
fn guess_barcode_type_from_value(value: &str) -> BarcodeType {
    let value = value.trim();
    if value.is_empty() {
        return BarcodeType::NoCode;
    }

    let all_digits = value.chars().all(|c| c.is_ascii_digit());
    if all_digits {
        match value.len() {
            13 => BarcodeType::Ean13,
            12 => BarcodeType::UpcA,
            // EAN-8 and UPC-E can both be 8 digits; EAN-8 is the more
            // standard of the two internationally.
            8 => BarcodeType::Ean8,
            // UPC-E's compressed representations.
            6 | 7 => BarcodeType::UpcE,
            n if n % 2 == 0 => BarcodeType::Itf,
            // Odd-length digit-only can't be ITF or any fixed-length
            // format above. Code128 handles arbitrary-length numeric
            // content and is common for loyalty account numbers.
            _ => BarcodeType::Code128,
        }
    } else {
        // Code128 is a strict superset of what Code39/Code93 can
        // encode, so it covers this whole bucket.
        BarcodeType::Code128
    }
}

/// Semicolon-delimited (not comma), columns Retailer/Program/Added
/// At/Reference/Firstname/Lastname. No barcode-type column at all;
/// inferred from the Reference value's shape instead (see
/// guess_barcode_type_from_value). No dedicated notes field either;
/// Program/Added At/name are combined into notes.
///
/// Skips any row with an empty Reference: FidMe blanks this out for
/// expired cards (see CatimaLoyalty/Android#1005), and a card with no
/// ID has nothing usable to import.
fn parse_fidme_csv(text: &str) -> Vec<Card> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .has_headers(true)
        .flexible(true)
        .from_reader(text.as_bytes());

    let Ok(headers) = reader.headers().cloned() else {
        return Vec::new();
    };
    let col = |name: &str| headers.iter().position(|h| h == name);
    let retailer_idx = col("Retailer");
    let program_idx = col("Program");
    let added_at_idx = col("Added At");
    let reference_idx = col("Reference");
    let firstname_idx = col("Firstname");
    let lastname_idx = col("Lastname");

    let get = |record: &csv::StringRecord, idx: Option<usize>| -> String {
        idx.and_then(|i| record.get(i)).unwrap_or("").trim().to_string()
    };

    let mut cards = Vec::new();
    for record in reader.records().filter_map(|r| r.ok()) {
        let store = get(&record, retailer_idx);
        if store.is_empty() {
            continue;
        }
        let reference = get(&record, reference_idx);
        if reference.is_empty() {
            // FidMe blanks this on expired cards; a card with no ID
            // isn't usable.
            continue;
        }

        let program = get(&record, program_idx);
        let added_at = get(&record, added_at_idx);
        let first_name = get(&record, firstname_idx);
        let last_name = get(&record, lastname_idx);
        let combined_name = format!("{first_name} {last_name}").trim().to_string();

        let mut note_lines = Vec::new();
        if !program.is_empty() {
            note_lines.push(program);
        }
        if !added_at.is_empty() {
            note_lines.push(added_at);
        }
        if !combined_name.is_empty() {
            note_lines.push(combined_name);
        }

        let color = stable_color_for(&store);
        let barcode_type = guess_barcode_type_from_value(&reference);
        let mut card = Card::new(store);
        card.code_value = reference;
        card.barcode_type = barcode_type;
        card.color = color;
        card.notes = note_lines.join("\n");
        cards.push(card);
    }
    cards
}

/// Bare JSON array (not wrapped in an object), one object per voucher.
/// `description` (store name), `code`, `codeType`
/// (CODE128/CODE39/EAN13/PDF417/QR/TEXT), `color`
/// (GREY/BLUE/GREEN/ORANGE/PURPLE/RED/YELLOW), and `expires` (optional,
/// "yyyy-MM-ddTHH:mm:ss.SSS" UTC, truncated to the date). Catima's own
/// importer errors out on an unrecognized codeType or color; this is
/// more lenient, falling back to NoCode / stable_color_for instead of
/// failing the whole import over one bad row.
fn parse_vouchervault_json(text: &str) -> Result<Vec<Card>, String> {
    #[derive(serde::Deserialize)]
    struct VoucherVaultEntry {
        description: String,
        code: String,
        #[serde(rename = "codeType")]
        code_type: String,
        color: String,
        expires: Option<String>,
    }

    let entries: Vec<VoucherVaultEntry> =
        serde_json::from_str(text).map_err(|e| e.to_string())?;

    let mut cards = Vec::new();
    for entry in entries {
        let barcode_type = match entry.code_type.as_str() {
            "CODE128" => BarcodeType::Code128,
            "CODE39" => BarcodeType::Code39,
            "EAN13" => BarcodeType::Ean13,
            "PDF417" => BarcodeType::Pdf417,
            "QR" => BarcodeType::QrCode,
            _ => BarcodeType::NoCode, // includes "TEXT" and anything unrecognized
        };
        let color = match entry.color.as_str() {
            "GREY" => "#808080".to_string(),
            "BLUE" => "#0000ff".to_string(),
            "GREEN" => "#008000".to_string(),
            "ORANGE" => "#ffa500".to_string(),
            "PURPLE" => "#800080".to_string(),
            "RED" => "#ff0000".to_string(),
            "YELLOW" => "#ffff00".to_string(),
            _ => stable_color_for(&entry.description),
        };

        let mut card = Card::new(entry.description);
        card.code_value = entry.code;
        card.barcode_type = barcode_type;
        card.color = color;
        // expiry doesn't carry time-of-day.
        card.expiry = entry.expires.map(|e| e.chars().take(10).collect());
        cards.push(card);
    }
    Ok(cards)
}

/// Which Card field a recognized label maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardField {
    Name,
    CodeValue,
    BarcodeType,
    Notes,
    Color,
    Expiry,
    ValidFrom,
}

/// Normalizes a label for matching: lowercase, strip
/// spaces/underscores/hyphens. "Expiration Date", "expiration_date",
/// and "Expiration-Date" all collapse to "expirationdate".
fn normalize_label(label: &str) -> String {
    label
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Synonym lists, one per field, ordered most-canonical-first. This
/// order settles it when two labels in the same record match the same
/// field (see match_field_synonym): the earlier synonym wins. Generic
/// terms like "value" or "id" sit at the end since they could mean
/// many things in a file not designed with Barkeep in mind.
///
/// "card" (bare) is in NAME_SYNONYMS because a real user-provided
/// example file used exactly that as its name label ("Card1: Murphy's
/// USA").
const NAME_SYNONYMS: &[&str] = &[
    "name", "cardname", "storename", "merchantname", "programname",
    "card", "store", "merchant", "retailer", "business", "company",
    "program", "title", "label",
];
const CODE_VALUE_SYNONYMS: &[&str] = &[
    "code", "codevalue", "barcode", "cardnumber", "accountnumber",
    "membernumber", "membershipnumber", "cardid", "number", "id",
    "reference", "value",
];
const BARCODE_TYPE_SYNONYMS: &[&str] = &[
    "barcodetype", "codetype", "symbology", "barcodeformat",
    "codeformat", "type", "format",
];
const NOTES_SYNONYMS: &[&str] = &[
    "notes", "note", "description", "comment", "comments", "memo", "details",
];
const COLOR_SYNONYMS: &[&str] = &[
    "color", "colour", "cardcolor", "backgroundcolor", "hexcolor", "hex",
];
const EXPIRY_SYNONYMS: &[&str] = &[
    "expiry", "expires", "expiration", "expirationdate", "expirydate",
    "validuntil", "expdate",
];
const VALID_FROM_SYNONYMS: &[&str] = &[
    "validfrom", "activefrom", "issuedate", "startdate", "issued",
];

/// Exact match against each field's synonym list first; if nothing
/// matches, strips trailing ASCII digits and tries again, since a
/// file may number every label under a card with that card's own
/// number ("Code1", "Card1" both belong to record 1, not two fields
/// named "Code" and "Card"). Returns the matched field and how far
/// down its synonym list the match was found (0 = most canonical),
/// used by parse_label_value_blocks to resolve collisions.
fn match_field_synonym(label: &str) -> Option<(CardField, usize)> {
    let lists: &[(CardField, &[&str])] = &[
        (CardField::Name, NAME_SYNONYMS),
        (CardField::CodeValue, CODE_VALUE_SYNONYMS),
        (CardField::BarcodeType, BARCODE_TYPE_SYNONYMS),
        (CardField::Notes, NOTES_SYNONYMS),
        (CardField::Color, COLOR_SYNONYMS),
        (CardField::Expiry, EXPIRY_SYNONYMS),
        (CardField::ValidFrom, VALID_FROM_SYNONYMS),
    ];
    let try_match = |normalized: &str| {
        lists.iter().find_map(|(field, synonyms)| {
            synonyms.iter().position(|s| *s == normalized).map(|i| (*field, i))
        })
    };
    let normalized = normalize_label(label);
    try_match(&normalized).or_else(|| {
        let trimmed = normalized.trim_end_matches(|c: char| c.is_ascii_digit());
        // Only retry if trimming actually removed something.
        (trimmed.len() != normalized.len() && !trimmed.is_empty()).then(|| try_match(trimmed)).flatten()
    })
}

/// Which edge of a grid a header row/column was found at, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderLocation {
    TopRow,
    BottomRow,
    FirstColumn,
    LastColumn,
}

/// Counts how many cells in a row/column match a known field synonym,
/// any field. Just a "does this look like a header" signal, not for
/// resolving which cell maps to which field.
fn count_synonym_matches<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.filter(|c| match_field_synonym(c).is_some()).count()
}

/// Converts raw CSV or TSV text into a grid, for feeding into
/// detect_header/parse_grid below. Delimiter (comma vs tab) is guessed
/// by counting each in the first few non-empty lines. Reads with
/// headers disabled deliberately: detect_header needs to see every
/// row, including a possible header, to check all four edges.
fn parse_delimited_text_to_grid(text: &str) -> Option<Vec<Vec<String>>> {
    let sample: String = text.lines().filter(|l| !l.trim().is_empty()).take(5).collect::<Vec<_>>().join("\n");
    let comma_count = sample.matches(',').count();
    let tab_count = sample.matches('\t').count();
    if comma_count == 0 && tab_count == 0 {
        return None;
    }
    let delimiter = if tab_count > comma_count { b'\t' } else { b',' };

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());

    let mut grid = Vec::new();
    for result in reader.records() {
        let Ok(record) = result else { continue };
        let row: Vec<String> = record.iter().map(|cell| cell.to_string()).collect();
        // Skips fully-empty rows so a stray blank line doesn't become a
        // row of empty strings for detect_header/parse_grid to reason
        // about.
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        grid.push(row);
    }

    if grid.is_empty() {
        None
    } else {
        Some(grid)
    }
}

/// Checks all four edges of a headerless grid for header values before
/// falling back to content-based heuristics: explicit labels, wherever
/// positioned, are a more reliable signal than guessing from shape.
///
/// Top row gets a fast path: 3+ matching cells accepts it immediately
/// without checking the other edges. Otherwise all four edges are
/// tallied and the strictly highest count wins, even below the
/// top row's threshold of 3 (a 2-column file can only ever have 2
/// matches). A tie (including a tie at zero) reports no header (None)
/// rather than guessing among equally-plausible candidates. A common
/// tie case: the shared top-left corner cell is the only match
/// anywhere, contributing equally to both the top-row and
/// first-column tallies.
///
/// Only meaningful for a real grid: requires at least 2 rows and 2
/// columns.
fn detect_header(grid: &[Vec<String>]) -> Option<HeaderLocation> {
    if grid.len() < 2 || grid.first().map_or(true, |row| row.len() < 2) {
        return None;
    }

    let top_row = grid.first()?;
    let bottom_row = grid.last()?;
    // .get(0)/.last() per row rather than assuming every row is as
    // wide as the first, so ragged rows stay safe (at the cost of
    // "last column" not necessarily being one consistent index).
    let first_column: Vec<&str> = grid.iter().filter_map(|row| row.first()).map(String::as_str).collect();
    let last_column: Vec<&str> = grid.iter().filter_map(|row| row.last()).map(String::as_str).collect();

    let top_count = count_synonym_matches(top_row.iter().map(String::as_str));
    if top_count >= 3 {
        return Some(HeaderLocation::TopRow);
    }

    let candidates = [
        (HeaderLocation::TopRow, top_count),
        (HeaderLocation::FirstColumn, count_synonym_matches(first_column.into_iter())),
        (HeaderLocation::BottomRow, count_synonym_matches(bottom_row.iter().map(String::as_str))),
        (HeaderLocation::LastColumn, count_synonym_matches(last_column.into_iter())),
    ];
    let max_count = candidates.iter().map(|(_, count)| *count).max().unwrap_or(0);
    if max_count == 0 {
        return None;
    }
    let mut winners = candidates.iter().filter(|(_, count)| *count == max_count);
    let winner = winners.next()?;
    // More than one edge tied for the lead: no clear winner.
    if winners.next().is_some() {
        return None;
    }
    Some(winner.0)
}

/// Resolves collisions the same priority-based way
/// parse_label_value_blocks does per-record, but once at the header
/// level rather than per row. The column matching the most canonical
/// synonym wins that field; every other column matching the same
/// field, or nothing, gets None (folded into notes by parse_grid).
fn map_header_columns(header: &[String]) -> Vec<Option<CardField>> {
    let matches: Vec<Option<(CardField, usize)>> = header.iter().map(|h| match_field_synonym(h)).collect();
    let mut result: Vec<Option<CardField>> = matches.iter().map(|m| m.map(|(field, _)| field)).collect();
    for i in 0..matches.len() {
        let Some((field_i, priority_i)) = matches[i] else { continue };
        let beaten = matches.iter().enumerate().any(|(j, m)| {
            j != i && m.map_or(false, |(field_j, priority_j)| field_j == field_i && priority_j < priority_i)
        });
        if beaten {
            result[i] = None;
        }
    }
    result
}

/// Used when detect_header finds the header in a column rather than a
/// row, so parse_grid can assume "header is a row" uniformly. Safe
/// against ragged rows.
fn transpose_grid(grid: &[Vec<String>]) -> Vec<Vec<String>> {
    let num_columns = grid.iter().map(Vec::len).max().unwrap_or(0);
    let mut result = vec![Vec::with_capacity(grid.len()); num_columns];
    for row in grid {
        for (col_index, cell) in row.iter().enumerate() {
            result[col_index].push(cell.clone());
        }
    }
    result
}

/// Turns a headerless grid into cards, using detect_header to find the
/// header and map_header_columns to resolve which column means what.
/// Returns None if detect_header can't find a header;
/// guess_headerless_grid_columns handles that case separately.
///
/// Column-to-field mapping happens once, at the header level, not per
/// row the way parse_label_value_blocks needs, since a real header's
/// meaning doesn't change row to row.
fn parse_grid(grid: Vec<Vec<String>>) -> Option<Vec<Card>> {
    let location = detect_header(&grid)?;

    let (header, data_rows) = match location {
        HeaderLocation::TopRow => {
            let mut rows = grid;
            let header = rows.remove(0);
            (header, rows)
        }
        HeaderLocation::BottomRow => {
            let mut rows = grid;
            let header = rows.pop().unwrap_or_default();
            (header, rows)
        }
        HeaderLocation::FirstColumn => {
            let mut columns = transpose_grid(&grid);
            let header = columns.remove(0);
            (header, columns)
        }
        HeaderLocation::LastColumn => {
            let mut columns = transpose_grid(&grid);
            let header = columns.pop().unwrap_or_default();
            (header, columns)
        }
    };

    let column_fields = map_header_columns(&header);
    Some(build_cards_from_grid_columns(&data_rows, &header, &column_fields))
}

/// Shared between parse_grid's header-found path and
/// guess_headerless_grid_columns below, so both build cards
/// identically. `header` is used only for labeling unrecognized
/// columns in notes; pass an empty header for a headerless grid.
fn build_cards_from_grid_columns(
    data_rows: &[Vec<String>],
    header: &[String],
    column_fields: &[Option<CardField>],
) -> Vec<Card> {
    let mut cards = Vec::with_capacity(data_rows.len());
    for (index, row) in data_rows.iter().enumerate() {
        let mut name: Option<String> = None;
        let mut code_value: Option<String> = None;
        let mut barcode_type_value: Option<String> = None;
        let mut color: Option<String> = None;
        let mut expiry: Option<String> = None;
        let mut valid_from: Option<String> = None;
        let mut extra_notes: Vec<String> = Vec::new();

        for (col_index, cell) in row.iter().enumerate() {
            if cell.trim().is_empty() {
                continue;
            }
            let field = column_fields.get(col_index).copied().flatten();
            let column_label = header.get(col_index).cloned();
            match field {
                Some(CardField::Name) => name = Some(cell.clone()),
                Some(CardField::CodeValue) => code_value = Some(cell.clone()),
                Some(CardField::BarcodeType) => barcode_type_value = Some(cell.clone()),
                Some(CardField::Color) => color = Some(cell.clone()),
                Some(CardField::Expiry) => expiry = Some(cell.clone()),
                Some(CardField::ValidFrom) => valid_from = Some(cell.clone()),
                Some(CardField::Notes) => extra_notes.push(cell.clone()),
                None => match column_label {
                    Some(label) if !label.trim().is_empty() => extra_notes.push(format!("{label}: {cell}")),
                    _ => extra_notes.push(cell.clone()),
                },
            }
        }

        let name = name.unwrap_or_else(|| format!("Imported Card {}", index + 1));
        let code_value = code_value.unwrap_or_default();
        let barcode_type = barcode_type_value
            .as_deref()
            .and_then(guess_barcode_type_from_label)
            .unwrap_or_else(|| guess_barcode_type_from_value(&code_value));
        let color = color.unwrap_or_else(|| stable_color_for(&name));

        let mut card = Card::new(name);
        card.code_value = code_value;
        card.barcode_type = barcode_type;
        card.color = color;
        card.expiry = expiry;
        card.valid_from = valid_from;
        card.notes = extra_notes.join("\n");
        cards.push(card);
    }
    cards
}

/// A cell looks like a code/barcode value if it has no whitespace, is
/// entirely alphanumeric, and contains at least one digit. The digit
/// requirement distinguishes a real code ("123456789", "ABC123XYZ")
/// from a single-word store name ("Costco").
fn cell_looks_code_like(cell: &str) -> bool {
    let trimmed = cell.trim();
    !trimmed.is_empty()
        && !trimmed.contains(char::is_whitespace)
        && trimmed.chars().all(|c| c.is_ascii_alphanumeric())
        && trimmed.chars().any(|c| c.is_ascii_digit())
}

/// A cell looks like a name if it contains at least one letter and
/// isn't code-like, excluding a value like "ABC123" from counting as
/// name-like even though it contains letters.
fn cell_looks_name_like(cell: &str) -> bool {
    let trimmed = cell.trim();
    !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphabetic()) && !cell_looks_code_like(trimmed)
}

/// For a grid with no detectable header, guesses which column is the
/// code value and which is the name by what the data looks like,
/// rather than by fixed position.
///
/// A column only gets a guessed role if it's the unique column meeting
/// an 80%-of-non-empty-cells bar for that role; two candidates or zero
/// means no guess, independently for code and name. No column is ever
/// guessed as barcode type, color, expiry, or valid_from here; those
/// only come from an explicit header, which a headerless grid doesn't
/// have. Unassigned columns fold into notes.
fn guess_headerless_grid_columns(grid: &[Vec<String>]) -> Vec<Card> {
    let column_count = grid.iter().map(|row| row.len()).max().unwrap_or(0);

    let mut code_column: Option<usize> = None;
    let mut code_candidates = 0;
    for col in 0..column_count {
        let cells: Vec<&str> = grid
            .iter()
            .filter_map(|row| row.get(col))
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .collect();
        if cells.is_empty() {
            continue;
        }
        let code_like_count = cells.iter().filter(|c| cell_looks_code_like(c)).count();
        if code_like_count as f64 / cells.len() as f64 >= 0.8 {
            code_candidates += 1;
            code_column = Some(col);
        }
    }
    let code_column = if code_candidates == 1 { code_column } else { None };

    let mut name_column: Option<usize> = None;
    let mut name_candidates = 0;
    for col in 0..column_count {
        if Some(col) == code_column {
            continue;
        }
        let cells: Vec<&str> = grid
            .iter()
            .filter_map(|row| row.get(col))
            .map(|s| s.as_str())
            .filter(|s| !s.trim().is_empty())
            .collect();
        if cells.is_empty() {
            continue;
        }
        let name_like_count = cells.iter().filter(|c| cell_looks_name_like(c)).count();
        if name_like_count as f64 / cells.len() as f64 >= 0.8 {
            name_candidates += 1;
            name_column = Some(col);
        }
    }
    let name_column = if name_candidates == 1 { name_column } else { None };

    let column_fields: Vec<Option<CardField>> = (0..column_count)
        .map(|col| {
            if Some(col) == code_column {
                Some(CardField::CodeValue)
            } else if Some(col) == name_column {
                Some(CardField::Name)
            } else {
                None
            }
        })
        .collect();

    build_cards_from_grid_columns(grid, &[], &column_fields)
}

/// The most permissive fallback: a plain list of bare values, one per
/// line, no labels or grid structure. Every non-blank line becomes its
/// own card, named generically, since a single-column file has nothing
/// to distinguish a name from a code.
fn parse_bare_list(text: &str) -> Vec<Card> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, line)| {
            let name = format!("Imported Card {}", index + 1);
            let code_value = line.to_string();
            let barcode_type = guess_barcode_type_from_value(&code_value);
            let color = stable_color_for(&name);
            let mut card = Card::new(name);
            card.code_value = code_value;
            card.barcode_type = barcode_type;
            card.color = color;
            card
        })
        .collect()
}

/// Tries "label: value" blocks, then a CSV/TSV grid, then a plain bare
/// list as the final catch-all. Returns None only if the bare-list
/// fallback also found nothing (empty or whitespace-only file).
///
/// Label:value is gated by an upfront shape check (80%+ of non-blank
/// lines contain a colon), unlike the other two: parse_label_value_blocks
/// always returns something for any input, so an emptiness check after
/// the fact wouldn't catch a bad match (e.g. a CSV's comma-separated
/// lines have no colons, so they'd mostly become notes-only cards).
fn try_fallback_parse(text: &str) -> Option<Vec<Card>> {
    let non_blank_lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if !non_blank_lines.is_empty() {
        let colon_lines = non_blank_lines.iter().filter(|l| l.contains(':')).count();
        if colon_lines as f64 / non_blank_lines.len() as f64 >= 0.8 {
            let cards = parse_label_value_blocks(text);
            if !cards.is_empty() {
                return Some(cards);
            }
        }
    }

    if let Some(grid) = parse_delimited_text_to_grid(text) {
        if let Some(cards) = parse_grid(grid.clone()) {
            if !cards.is_empty() {
                return Some(cards);
            }
        }
        let cards = guess_headerless_grid_columns(&grid);
        if !cards.is_empty() {
            return Some(cards);
        }
    }

    let cards = parse_bare_list(text);
    if cards.is_empty() {
        None
    } else {
        Some(cards)
    }
}

/// Parses "Label: value" lines grouped into records by blank-line
/// separation, e.g.:
/// ```text
/// Card1: Murphy's USA
/// Code1: 123456
///
/// Card2: Buc-ee's
/// Code2: 245263456
/// Expiration Date2: 2031-06-11
/// ```
///
/// Blank-line separation is the only signal for where one record ends
/// and the next begins, not trailing digits, since a file might only
/// number the first label in a block and leave the rest bare.
///
/// A label that doesn't match any known field folds into notes as
/// "Label: value" text rather than being dropped. Same for a second
/// label matching a field already filled by a higher-priority match in
/// the same record.
///
/// Missing fields fall back to: no name-like label -> generic
/// "Imported Card N"; no type-like label -> guess_barcode_type_from_value
/// against whatever code value was found; no color-like label ->
/// stable_color_for.
fn parse_label_value_blocks(text: &str) -> Vec<Card> {
    // Groups consecutive non-blank lines together; blank lines start a
    // new group. Starts with one empty group so there's always
    // somewhere to push into; multiple consecutive blank lines don't
    // produce extra empty groups, and the trailing filter below cleans
    // up whatever's left.
    let records: Vec<Vec<&str>> = text
        .lines()
        .fold(vec![Vec::new()], |mut groups, line| {
            if line.trim().is_empty() {
                let Some(last) = groups.last() else {
                    unreachable!("groups starts as vec![Vec::new()] and this fold never pops, only pushes -- always at least one element")
                };
                if !last.is_empty() {
                    groups.push(Vec::new());
                }
            } else {
                let Some(last) = groups.last_mut() else {
                    unreachable!("groups starts as vec![Vec::new()] and this fold never pops, only pushes -- always at least one element")
                };
                last.push(line);
            }
            groups
        })
        .into_iter()
        .filter(|group| !group.is_empty())
        .collect();

    let mut cards = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        // Each slot tracks (label, value, priority) as one unit, not
        // value/priority separately. A prior version only had value and
        // priority, so a displacement pushed the outgoing value to
        // notes labeled with whatever line was currently being
        // processed instead of its own original label.
        let mut name: Option<(String, String)> = None;
        let mut name_priority = usize::MAX;
        let mut code_value: Option<(String, String)> = None;
        let mut code_value_priority = usize::MAX;
        let mut barcode_type: Option<(String, String)> = None;
        let mut barcode_type_priority = usize::MAX;
        let mut color: Option<(String, String)> = None;
        let mut color_priority = usize::MAX;
        let mut expiry: Option<(String, String)> = None;
        let mut expiry_priority = usize::MAX;
        let mut valid_from: Option<(String, String)> = None;
        let mut valid_from_priority = usize::MAX;
        let mut extra_notes: Vec<String> = Vec::new();

        for line in record {
            // A line with no ':' doesn't fit this format; kept in
            // notes rather than dropped.
            let Some((label, value)) = line.split_once(':') else {
                extra_notes.push((*line).to_string());
                continue;
            };
            let label = label.trim().to_string();
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }

            macro_rules! claim {
                ($field_slot:expr, $priority_slot:expr, $new_priority:expr, $label:expr, $value:expr) => {
                    if $new_priority < $priority_slot {
                        // A higher-priority match displaces what was
                        // there; the displaced pair still goes to
                        // notes, attributed to its own original label.
                        if let Some((old_label, old_value)) = $field_slot.take() {
                            extra_notes.push(format!("{old_label}: {old_value}"));
                        }
                        $field_slot = Some(($label, $value));
                        $priority_slot = $new_priority;
                    } else {
                        extra_notes.push(format!("{}: {}", $label, $value));
                    }
                };
            }

            match match_field_synonym(&label) {
                Some((CardField::Name, priority)) => claim!(name, name_priority, priority, label, value),
                Some((CardField::CodeValue, priority)) => claim!(code_value, code_value_priority, priority, label, value),
                Some((CardField::BarcodeType, priority)) => claim!(barcode_type, barcode_type_priority, priority, label, value),
                Some((CardField::Color, priority)) => claim!(color, color_priority, priority, label, value),
                Some((CardField::Expiry, priority)) => claim!(expiry, expiry_priority, priority, label, value),
                Some((CardField::ValidFrom, priority)) => claim!(valid_from, valid_from_priority, priority, label, value),
                // Notes has no priority worth tracking; every
                // notes-bound line just accumulates.
                Some((CardField::Notes, _)) => extra_notes.push(value),
                None => extra_notes.push(format!("{label}: {value}")),
            }
        }

        // Used as-is, including a bare-digits value from something
        // like "Card: 1". An earlier version rejected bare-digits
        // names in favor of the generic placeholder; reconsidered,
        // since "1" is just as transparently incomplete and keeps the
        // source file's own numbering intact.
        let name = name
            .map(|(_, value)| value)
            .unwrap_or_else(|| format!("Imported Card {}", index + 1));
        let code_value = code_value.map(|(_, value)| value).unwrap_or_default();
        let barcode_type = barcode_type
            .and_then(|(_, value)| guess_barcode_type_from_label(&value))
            .unwrap_or_else(|| guess_barcode_type_from_value(&code_value));
        let color = color
            .map(|(_, value)| value)
            .unwrap_or_else(|| stable_color_for(&name));
        let expiry = expiry.map(|(_, value)| value);
        let valid_from = valid_from.map(|(_, value)| value);

        let mut card = Card::new(name);
        card.code_value = code_value;
        card.barcode_type = barcode_type;
        card.color = color;
        card.expiry = expiry;
        card.valid_from = valid_from;
        card.notes = extra_notes.join("\n");
        cards.push(card);
    }
    cards
}

/// Parses a UBcards `wallet.ini`,
/// `~/.local/share/ubcards/wallet.ini` on the device. Plain Qt
/// `QSettings` INI: one `[General]` section with an `all=` list of
/// card UUIDs, then one `[<uuid>]` section per card with `name`,
/// `category`, `text` (the barcode value, oddly not called "value" or
/// "code"), `type`, and `timestamp` keys.
///
/// Doesn't parse `all=`: every section other than `[General]` already
/// is a card, so scanning for `[...]` sections sidesteps `all=`'s
/// list-serialization quirks (UBcards' own UI and its migration script
/// don't even agree on the separator format there).
///
/// `timestamp` is a QDateTime, and Qt serializes it as
/// `@Type(<raw QDataStream bytes>)`, meaning wallet.ini is not
/// reliably valid UTF-8 as a whole file. import_any reads it as
/// lossily-decoded bytes for this reason. This parser never reads
/// `timestamp` and only recognizes `[...]` section headers and
/// `name=`/`text=`/`type=`/`category=` keys, so a stray `\n` byte
/// inside binary timestamp data just produces a garbage line that
/// doesn't match any pattern and gets skipped.
fn parse_ubcards_wallet_ini(text: &str) -> Vec<Card> {
    #[derive(Default)]
    struct Section {
        name: String,
        category: String,
        text: String,
        barcode_type: String,
    }

    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;

    for line in text.split('\n') {
        let line = line.trim();

        if let Some(inner) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            if inner != "General" {
                current = Some(Section::default());
            }
            continue;
        }

        let Some(section) = current.as_mut() else {
            continue;
        };

        if let Some(value) = line.strip_prefix("name=") {
            section.name = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("category=") {
            section.category = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("text=") {
            section.text = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("type=") {
            section.barcode_type = value.trim().to_string();
        }
    }
    if let Some(section) = current.take() {
        sections.push(section);
    }

    let mut cards = Vec::new();
    for section in sections {
        if section.text.is_empty() {
            continue;
        }

        let barcode_type = ubcards_type_to_barcode_type(&section.barcode_type);
        let color = stable_color_for(&section.name);

        let mut card = Card::new(if section.name.is_empty() {
            "Imported card".to_string()
        } else {
            section.name
        });
        card.code_value = section.text;
        card.barcode_type = barcode_type;
        card.color = color;
        // "generic" is UBcards' default category unless the person
        // picked something else, so it's only surfaced when set to
        // something else.
        if !section.category.is_empty() && section.category != "generic" {
            card.notes = format!("Category: {}", section.category);
        }
        cards.push(card);
    }
    cards
}

/// Maps a UBcards/ZBar symbology name to Barkeep's BarcodeType. Sourced
/// from the manual-entry dropdown in ubcards.qml (CODE-128, CODE-128-
/// mini, CODE-39, libre-CODE-39, EAN-13, EAN-8, I2/5) and, for a
/// camera-scanned card, ZBar's own `get_type_name()` via
/// qrcodereader.cpp, which adds at least QR-Code and other symbologies
/// outside the fixed dropdown. Case-insensitive match.
fn ubcards_type_to_barcode_type(type_str: &str) -> BarcodeType {
    let normalized = type_str.trim().to_lowercase();
    match normalized.as_str() {
        "qr-code" | "qrcode" | "qr" => BarcodeType::QrCode,
        "ean-13" | "ean13" => BarcodeType::Ean13,
        "ean-8" | "ean8" => BarcodeType::Ean8,
        "upc-a" | "upca" => BarcodeType::UpcA,
        "upc-e" | "upce" => BarcodeType::UpcE,
        "code-39" | "libre-code-39" | "code39" => BarcodeType::Code39,
        "code-93" | "code93" => BarcodeType::Code93,
        "code-128" | "code-128-mini" | "code128" => BarcodeType::Code128,
        "codabar" => BarcodeType::Codabar,
        "i2/5" | "i25" | "itf" => BarcodeType::Itf,
        "pdf417" => BarcodeType::Pdf417,
        _ => BarcodeType::NoCode,
    }
}

/// Apple's PassKit `PKBarcodeFormat*` constants. PassKit supports far
/// fewer symbologies than Barkeep/rxing; only these four exist.
fn pkpass_barcode_format_to_barcode_type(format: &str) -> Option<BarcodeType> {
    match format {
        "PKBarcodeFormatQR" => Some(BarcodeType::QrCode),
        "PKBarcodeFormatPDF417" => Some(BarcodeType::Pdf417),
        "PKBarcodeFormatAztec" => Some(BarcodeType::Aztec),
        "PKBarcodeFormatCode128" => Some(BarcodeType::Code128),
        _ => None,
    }
}

/// Parses PassKit's CSS-style "rgb(r, g, b)" color (used for
/// `backgroundColor` in pass.json) into Barkeep's "#RRGGBB" hex.
fn parse_css_rgb_color(value: &str) -> Option<String> {
    let inner = value
        .trim()
        .strip_prefix("rgb(")?
        .strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
    if parts.len() != 3 {
        return None;
    }
    let r: u8 = parts[0].parse().ok()?;
    let g: u8 = parts[1].parse().ok()?;
    let b: u8 = parts[2].parse().ok()?;
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// Shared parser behind `parse_catima_csv` and
/// `parse_loyaltycardkeychain_csv`. Columns are looked up by name
/// rather than fixed position, so a format-specific extra column (like
/// Catima's `barcodeencoding`) can't silently shift every field after
/// it.
fn parse_card_table_csv(text: &str, header_marker: &str, format_label: &str) -> Result<Vec<Card>, String> {
    let header_pos = text.find(header_marker).ok_or_else(|| {
        format!("this doesn't look like a {format_label} export (card table header not found)")
    })?;
    let card_section = &text[header_pos..];

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(card_section.as_bytes());

    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    let col = |name: &str| headers.iter().position(|h| h == name);
    let store_idx = col("store");
    let note_idx = col("note");
    let cardid_idx = col("cardid");
    let barcodeid_idx = col("barcodeid"); // None for formats without it (e.g. LoyaltyCardKeychain); falls back to cardid below
    let barcodetype_idx = col("barcodetype");
    let headercolor_idx = col("headercolor");
    // None for LoyaltyCardKeychain, which has no validfrom/expiry
    // columns; get() handles a missing column gracefully.
    let validfrom_idx = col("validfrom");
    let expiry_idx = col("expiry");
    let expected_len = headers.len();

    let get = |record: &csv::StringRecord, idx: Option<usize>| {
        idx.and_then(|i| record.get(i)).unwrap_or("").to_string()
    };

    let mut cards = Vec::new();
    for result in reader.records() {
        let record = result.map_err(|e| e.to_string())?;
        // A card row has as many columns as the header we just parsed.
        // Once we hit the link table ("cardId,groupId", 2 columns) or a
        // blank line, we're done.
        if record.len() != expected_len {
            break;
        }

        let store = get(&record, store_idx);
        let note = get(&record, note_idx);
        let card_id_field = get(&record, cardid_idx);
        let barcode_id_field = get(&record, barcodeid_idx);
        let barcode_type_field = get(&record, barcodetype_idx);
        let header_color_field = get(&record, headercolor_idx);
        let valid_from_field = get(&record, validfrom_idx);
        let expiry_field = get(&record, expiry_idx);

        // barcodeid holds the encoded value when Catima has one, but
        // it's often empty with the real value sitting in cardid
        // instead.
        let code_value = if !barcode_id_field.is_empty() {
            barcode_id_field
        } else {
            card_id_field
        };

        let barcode_type = if barcode_type_field.trim().is_empty() {
            BarcodeType::NoCode
        } else {
            BarcodeType::from_catima_name(&barcode_type_field).unwrap_or(BarcodeType::NoCode)
        };

        let color = parse_catima_color(&header_color_field, &store);

        let mut card = Card::new(if store.is_empty() { "Imported card".to_string() } else { store });
        card.notes = note;
        card.code_value = code_value;
        card.barcode_type = barcode_type;
        card.color = color;
        if !valid_from_field.trim().is_empty() {
            card.valid_from = Some(valid_from_field);
        }
        if !expiry_field.trim().is_empty() {
            card.expiry = Some(expiry_field);
        }
        cards.push(card);
    }

    Ok(cards)
}

/// Catima stores header color as a signed 32-bit ARGB int (Android's
/// `Color.parseColor` representation). Some cards have no custom color
/// set (an empty field, not a parse failure); for those, derive a
/// stable color from the store name instead of one flat fallback.
fn parse_catima_color(field: &str, store_name: &str) -> String {
    match field.trim().parse::<i64>() {
        Ok(value) => format!("#{:06X}", (value as u32) & 0x00FF_FFFF),
        Err(_) => stable_color_for(store_name),
    }
}

/// Deterministic (same name always gives the same color) but
/// arbitrary, derived from a hash of the name. Fixed
/// saturation/lightness keep every generated color readable as a card
/// background.
fn stable_color_for(name: &str) -> String {
    let mut hash: u32 = 5381;
    for b in name.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u32);
    }
    let hue = (hash % 360) as f64;
    hsl_to_hex(hue, 0.55, 0.45)
}

fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let r = ((r1 + m) * 255.0).round() as u8;
    let g = ((g1 + m) * 255.0).round() as u8;
    let b = ((b1 + m) * 255.0).round() as u8;
    format!("#{r:02X}{g:02X}{b:02X}")
}

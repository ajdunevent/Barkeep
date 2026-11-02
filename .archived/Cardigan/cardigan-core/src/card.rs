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
    /// Not a Catima format -- Cardigan's own fallback for cards with no
    /// machine-readable code, or a decoded symbology we can't yet re-render.
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

    /// True for 1D/linear barcode formats (a sequence of variable-width
    /// bars) as opposed to 2D matrix formats (QR, Aztec, DataMatrix) or
    /// PDF417 (technically 2D/"stacked linear" -- kept in the non-linear
    /// bucket here since its error correction relies on 2D structure,
    /// unlike pure 1D formats). Used by the detail view to decide
    /// whether stretching the rendered barcode to fill available space
    /// (rather than preserving its native aspect ratio) is safe:
    /// non-uniform scaling of an already-rendered 1D barcode image
    /// multiplies every bar's width by the same factor, which preserves
    /// the *relative* bar-width ratios the encoding actually depends on
    /// -- so it stays scannable. That reasoning doesn't hold for a 2D
    /// grid of modules, where stretching one axis distorts square
    /// modules into rectangles.
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
            // Anything else we can decode but not cleanly categorize above
            // (e.g. RSS/MaxiCode) -- keep the decoded text, drop the symbology.
            _ => BarcodeType::NoCode,
        }
    }

    /// Reverse of from_catima_name, for exporting -- NoCode has no
    /// Catima equivalent, so it just writes an empty barcodetype field,
    /// matching how Catima itself represents a card with no code.
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

/// Tries to match a free-text barcode-type label (e.g. from a column
/// explicitly named "type"/"format"/"barcodeformat") against a real
/// BarcodeType. Deliberately separate from BarcodeType::from_catima_name
/// just above -- that one matches Catima's own exact, fixed strings
/// ("CODE_39", "QR_CODE"); this one is for arbitrary, free-text values a
/// user's own file might use, so it normalizes first (same
/// lowercase/strip-separators approach as normalize_label, since a
/// label-matching problem is a label-matching problem either way) and
/// matches against a broader set of reasonable synonyms per type. Only
/// covers the formats this app can actually render (see BarcodeType's
/// own variants) -- an unrecognized value falls through to None, letting
/// the caller fall back to guess_barcode_type_from_value instead of
/// forcing a wrong match.
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


/// by CardStore::apply_expiration_actions, not enforced live (nothing
/// stops `expiry` from being in the past between checks; it's just
/// acted on the next time that runs).
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

/// A named, independently-managed tag cards can be sorted into. Has its
/// own stable id (like Card itself) specifically so renaming or
/// reordering a group never needs to touch any card's own data --
/// cards reference groups by id (see Card::group_ids), not by name.
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
    /// Starred cards sort first in `search` (see there). `serde(default)`
    /// so a cards.json saved before this field existed still deserializes
    /// -- every pre-existing card just comes back unstarred instead of
    /// failing to load.
    #[serde(default)]
    pub starred: bool,
    /// Optional URL shown as an external-link icon next to the card's
    /// name in the detail view. `serde(default)` for the same
    /// backward-compatibility reason as `starred`.
    #[serde(default)]
    pub link: Option<String>,
    /// ISO 8601 "YYYY-MM-DD" strings, chosen over a real date type so no
    /// new dependency is needed -- lexicographic string comparison
    /// against today's ISO date is already a correct "is this expired"
    /// check in this format, no date-parsing required. Free-text
    /// entry, not validated against the format on save (matching the
    /// rest of this app's edit dialog, which doesn't validate
    /// code_value's shape either) -- an unparseable value just won't
    /// participate correctly in the expired/upcoming display logic,
    /// not a hard error.
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub expiry: Option<String>,
    /// Only meaningful when `expiry` is actually set -- see
    /// CardStore::apply_expiration_actions. Kept even if `expiry` is
    /// later cleared, rather than reset, since it's harmless dead state
    /// at that point and the edit dialog will just show it disabled.
    #[serde(default)]
    pub on_expiration: OnExpiration,
    /// A card can belong to any number of groups (unlike the earlier
    /// single-string `category` this replaced) -- referenced by id, not
    /// name, so renaming/reordering a Group (see CardStore::groups)
    /// never needs to touch this. Any id in here that no longer matches
    /// a real Group (e.g. that group was deleted) is just silently
    /// ignored wherever this is displayed/filtered on, rather than an
    /// error -- CardStore::remove_group also proactively sweeps deleted
    /// ids out of every card, so this is a defensive fallback, not the
    /// primary mechanism.
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
    /// Archived cards are hidden from the main grid by default (see
    /// CardStore::search's show_archived parameter) without being
    /// deleted -- for a card you're not currently using but don't want
    /// to lose, distinct from starring (which does the opposite: pins
    /// a card to the top instead of hiding it).
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

    /// Whether this card is currently valid, given `today` as an ISO
    /// 8601 "YYYY-MM-DD" string (lexicographic string comparison
    /// against it is a correct "before/after" check in this format, no
    /// date parsing needed -- see `valid_from`'s own doc comment).
    /// `today` must be non-empty and actually ISO-formatted for this to
    /// mean anything -- an empty string would incorrectly compare as
    /// "before" every set `valid_from`, since any non-empty date string
    /// sorts lexicographically greater than an empty one; callers that
    /// might have an empty/unknown `today` should check for that
    /// themselves before calling this (see search's own use of this,
    /// which does exactly that) rather than relying on this method to
    /// handle it.
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

/// What import_any actually found -- either a confidently-recognized
/// format, already imported and saved, or a set of candidate cards from
/// this importer's own last-resort fallback parsing (see
/// try_fallback_parse), not yet saved at all. The fallback case is
/// deliberately never auto-applied -- the caller (window.rs) is
/// expected to show a confirmation dialog first ("some cards may be
/// incomplete or have misidentified fields") and only call
/// CardStore::append_imported with these candidates if the user
/// actually confirms.
pub enum ImportOutcome {
    Imported(ImportSummary),
    NeedsFallbackConfirmation(Vec<Card>),
}

/// Small bits of UI state persisted across runs -- which toggles were
/// last set and which group tab was last active, not any real user
/// data (that's CardStore's own job, and stays in $XDG_DATA_HOME as it
/// always has). Deliberately a separate
/// file in a separate location, using dirs::state_dir() ($XDG_STATE_HOME,
/// falling back to ~/.local/state) rather than data_dir() -- this is
/// exactly the distinction the XDG base directory spec itself draws:
/// state data that should persist between restarts but isn't important
/// or portable enough to belong in $XDG_DATA_HOME alongside someone's
/// actual card collection.
#[derive(Serialize, Deserialize, Default)]
pub struct UiState {
    pub hide_archived: bool,
    pub hide_invalid: bool,
    pub search_bar_visible: bool,
    pub selected_group: Option<Uuid>,
}

impl UiState {
    /// Opens (and creates if needed) at the default XDG state location:
    /// `$XDG_STATE_HOME/cardigan/ui-state.json` (falls back to
    /// `~/.local/state`). A missing or unreadable file quietly falls
    /// back to Default (everything false) rather than erroring -- this
    /// is convenience state to restore, not something the app should
    /// refuse to start over.
    pub fn open_default() -> Self {
        let Some(mut dir) = dirs::state_dir() else {
            return Self::default();
        };
        dir.push("cardigan");
        if fs::create_dir_all(&dir).is_err() {
            return Self::default();
        }
        dir.push("ui-state.json");
        let Ok(data) = fs::read_to_string(&dir) else {
            return Self::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    /// Saves to the same location open_default() reads from. Errors are
    /// silently ignored -- same reasoning as open_default()'s own
    /// fallback: losing a UI-toggle preference for next launch isn't
    /// worth surfacing as a real error to the user.
    pub fn save(&self) {
        let Some(mut dir) = dirs::state_dir() else { return };
        dir.push("cardigan");
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        dir.push("ui-state.json");
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = fs::write(&dir, data);
        }
    }
}

/// The on-disk shape of cards.json -- a plain, explicit serde-only
/// struct kept separate from CardStore itself, which also carries
/// `path` (not something that belongs in the saved JSON). `#[serde(default)]`
/// on both fields, same reasoning as every other field addition in this
/// app: a hand-edited or older file missing one of these still parses
/// rather than hard-failing.
#[derive(Serialize, Deserialize, Default)]
struct CardStoreData {
    #[serde(default)]
    cards: Vec<Card>,
    #[serde(default)]
    groups: Vec<Group>,
}

/// Same shape as CardStoreData, but borrowing rather than owning --
/// used only for save(), so writing to disk doesn't need to clone the
/// entire cards/groups collections just to hand them to serde_json.
#[derive(Serialize)]
struct CardStoreDataRef<'a> {
    cards: &'a [Card],
    groups: &'a [Group],
}

/// Loads, holds, and persists the full card collection plus groups (see
/// Group) together as one JSON file. These used to be two separate
/// sibling files -- cards.json predated groups as a feature, and
/// merging them into one `{cards: [...], groups: [...]}` object would
/// have meant an existing user's real cards.json (a bare array at the
/// time) suddenly failing to parse as the new shape at all, silently
/// resetting their whole collection on the first launch after that
/// update. That risk doesn't apply here (no released version, no real
/// user data to protect across an upgrade), so the two files are
/// combined into one, which also lets remove_group below persist its
/// group-removal and its per-card group_ids sweep as a single atomic
/// write, rather than two independent ones that could disagree with
/// each other if the second failed after the first succeeded.
///
/// Clone -- needed for mutate_then_save below, which clones the whole
/// store, mutates the clone, and only swaps it into `self` once the
/// clone's own save() has actually succeeded. Cheap enough not to
/// matter: this app's realistic card counts are small, and every
/// save() already re-serializes the whole collection to JSON regardless,
/// which costs far more than one extra clone of the same data.
#[derive(Clone)]
pub struct CardStore {
    path: PathBuf,
    pub cards: Vec<Card>,
    pub groups: Vec<Group>,
}

impl CardStore {
    /// The default XDG data location cards.json lives at:
    /// `$XDG_DATA_HOME/cardigan/cards.json` (falls back to
    /// `~/.local/share`). Shared by open_default below and by
    /// load_read_only's own callers (the lock-screen plugin, running as
    /// a separate process that needs to resolve the exact same path
    /// independently) -- kept in one place specifically so the two
    /// consumers can't drift onto two different ideas of where the
    /// file actually lives.
    pub fn default_path() -> io::Result<PathBuf> {
        let mut dir = dirs::data_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no data directory available")
        })?;
        dir.push("cardigan");
        fs::create_dir_all(&dir)?;
        dir.push("cards.json");
        Ok(dir)
    }

    /// Opens (and creates if needed) the store at the default XDG data location:
    /// `$XDG_DATA_HOME/cardigan/cards.json` (falls back to `~/.local/share`).
    pub fn open_default() -> io::Result<(Self, Vec<String>)> {
        Self::open(Self::default_path()?)
    }

    /// Reads and parses `path` into `T`, defaulting to `T::default()` if
    /// the file doesn't exist yet (same as before). On a genuine I/O
    /// failure (permissions, etc.), that error still propagates via `?`
    /// -- this only changes what happens on a *parse* failure. The old
    /// behavior there was `.unwrap_or_default()`, which silently started
    /// the app with an empty collection and left the actual, still-
    /// possibly-recoverable file sitting on disk to be silently
    /// overwritten by the very next save() call -- a crash at least
    /// signals something's wrong; this didn't, and the data loss it led
    /// to was irreversible once that next save happened. Now: the
    /// original file is copied to `<path>.bak` (overwriting any earlier
    /// `.bak` from a previous corruption -- only the most recent
    /// corrupted copy is worth keeping around) *before* falling back to
    /// an empty collection, and a human-readable warning is pushed onto
    /// `warnings` so the caller can actually tell the user their data
    /// needed recovering, rather than this happening invisibly. If even
    /// the backup copy fails (rare -- would mean the directory itself
    /// just became unwritable), the warning says so too rather than
    /// claiming a backup that doesn't actually exist.
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

    /// Reads cards.json without any backup-on-corruption/repair attempt
    /// -- for the lock-screen plugin specifically, which reads the same
    /// file the main app writes to but, running as a genuinely separate
    /// process, should never attempt to "fix" it itself: two
    /// independent processes both trying to repair the same shared
    /// file on disk is a good way to create a new, different bug, not
    /// fewer. `Some(CardStore)` with empty `cards`/`groups` for a
    /// missing file (nothing written yet -- not an error, matching
    /// open()'s own `T::default()` fallback for the same case). `None`
    /// for a genuine read or parse failure on a file that does exist --
    /// this is the one case the caller should actually treat as
    /// "couldn't load cards" and show accordingly, per our own earlier
    /// decision that the plugin degrades gracefully rather than
    /// attempting repair. If some future caller needs to distinguish
    /// "no cards yet" from "couldn't read the file" for some other
    /// reason, add a second function for that rather than overloading
    /// this one's own single, simple contract.
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

    /// Runs `mutate` against a clone of this store, attempts to save
    /// the clone's resulting state, and only replaces this store's own
    /// data with the clone's if that save actually succeeds. On
    /// failure, `self` is never touched at all -- the mutation closure
    /// only ever ran against the (now-discarded) clone -- so in-memory
    /// state and on-disk state can't end up disagreeing with each
    /// other the way they previously could: every mutation method used
    /// to apply directly to `self`, then attempt a save that could fail
    /// independently of whether the in-memory change had already
    /// "succeeded." Every mutation method below goes through this now,
    /// rather than mutating `self` and calling save() separately
    /// itself. `path` is included in the clone but never actually
    /// changes here, so overwriting it via the swap is harmless.
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

    /// Removes a group entirely, and sweeps its id out of every card
    /// that referenced it (rather than leaving a dangling reference --
    /// Card::group_ids's own doc comment says a stale id is tolerated
    /// defensively wherever it's read, but there's no reason to leave
    /// one behind when this is the one place that can clean it up
    /// properly at the source). Both changes -- the removal itself and
    /// the per-card sweep -- persist together in mutate_then_save's
    /// single save() call, so they can't disagree with each other the
    /// way two independent saves to two separate files once could have.
    pub fn remove_group(&mut self, id: Uuid) -> io::Result<()> {
        self.mutate_then_save(move |store| {
            store.groups.retain(|g| g.id != id);
            for card in store.cards.iter_mut() {
                card.group_ids.retain(|g| *g != id);
            }
        })
    }

    /// Reorders `groups` to match `ordered_ids` exactly -- used by the
    /// Manage Groups dialog's drag-to-reorder. Any id in `ordered_ids`
    /// that doesn't match a real group is silently skipped; any real
    /// group not mentioned in `ordered_ids` (shouldn't normally happen,
    /// since the dialog reorders the full list it was shown) keeps its
    /// existing relative position, appended after everything that was
    /// explicitly ordered.
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

    /// Same as remove just above, but for multiple ids at once -- saves
    /// once after removing every matching card, not once per card.
    pub fn remove_bulk(&mut self, ids: &[Uuid]) -> io::Result<()> {
        self.mutate_then_save(|store| {
            store.cards.retain(|c| !ids.contains(&c.id));
        })
    }

    /// Applies a set of group-membership changes to every card whose id
    /// is in `card_ids` -- for each `(group_id, should_be_member)` pair,
    /// adds or removes that group from every matching card's own
    /// group_ids. A group not mentioned in `changes` at all is left
    /// completely untouched for every card -- this is the bulk "Change
    /// Groups" dialog's own tri-state semantics (see that dialog's own
    /// doc comment for why a group can end up genuinely skipped rather
    /// than just "not added"), decided by the caller before this is
    /// ever called, not something this method itself needs to know
    /// about. Saves once after applying every change to every matching
    /// card.
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

    /// Sets an explicit archived state (not a toggle) for every card
    /// whose id is in `ids` -- used for bulk archive/unarchive, where
    /// every selected card needs to end up in the *same* state
    /// regardless of what each one's own starting state was, unlike
    /// toggle_archived's per-card flip. Saves once after applying to
    /// every matching card, not once per card.
    pub fn set_archived_bulk(&mut self, ids: &[Uuid], archived: bool) -> io::Result<()> {
        self.mutate_then_save(|store| {
            for card in store.cards.iter_mut() {
                if ids.contains(&card.id) {
                    card.archived = archived;
                }
            }
        })
    }

    /// Sets an explicit starred state (not a toggle) for every card
    /// whose id is in `ids` -- same reasoning as set_archived_bulk just
    /// above, mirrored exactly: bulk star/unstar needs every selected
    /// card to end up in the same state regardless of its own starting
    /// state, unlike toggle_star's per-card flip. Saves once after
    /// applying to every matching card, not once per card.
    pub fn set_starred_bulk(&mut self, ids: &[Uuid], starred: bool) -> io::Result<()> {
        self.mutate_then_save(|store| {
            for card in store.cards.iter_mut() {
                if ids.contains(&card.id) {
                    card.starred = starred;
                }
            }
        })
    }

    /// Applies each expired card's own `on_expiration` setting (archive
    /// it, delete it, or leave it alone) -- called once at startup and
    /// again at every local midnight while the app stays open (see
    /// build_window's scheduling of this). `today` is a caller-supplied
    /// "YYYY-MM-DD" string rather than computed in here, so this module
    /// doesn't need a glib/GTK dependency just for the clock -- window.rs
    /// already has glib::DateTime in scope for other things. Plain
    /// string comparison against expiry (also "YYYY-MM-DD") is a
    /// correct "has this passed" check in this format, no date-parsing
    /// needed. Returns whether anything actually changed, so callers
    /// only need to rebuild the grid when there's something to show
    /// differently.
    ///
    /// Deliberately not routed through mutate_then_save above, even
    /// though it follows the same clone-then-swap-on-success shape by
    /// hand here -- mutate_then_save always attempts a save, and this
    /// is the one mutation method that genuinely needs to skip it
    /// entirely when nothing expired. Since this runs unattended, once
    /// a day, for as long as the app stays open, attempting a save
    /// every single time regardless would mean a persistent, unrelated
    /// disk problem could surface a "Save Failed" notification every
    /// midnight indefinitely, for a save that had nothing to actually
    /// lose in the first place.
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

    /// `show_archived` controls whether archived cards are included at
    /// all -- false is the normal default (archived cards hidden from
    /// the main grid); true is for whatever "Show Archived" toggle
    /// surfaces them. `valid_only`, similarly, hides any card whose
    /// valid_from hasn't started yet or whose expiry has already passed
    /// -- a card with neither set is never affected by this (nothing to
    /// judge it against, so it's always "valid"). `today` is a
    /// caller-supplied "YYYY-MM-DD" string for the same reason
    /// apply_expiration_actions takes one -- this module stays
    /// decoupled from GTK/glib for the clock. `group_filter` is `None`
    /// for "All", or a specific group's id to only show cards that
    /// actually belong to it. All of these combine with AND semantics,
    /// alongside the text query -- a card has to pass every active
    /// filter, not just one. Search text matches name, notes, or (by
    /// resolving each of the card's group_ids to its current name) any
    /// group it belongs to.
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
    /// layout -- the same content `import_any` already reads back on
    /// the Catima side, just without the zip wrapper or the
    /// `catima.csv` internal filename Catima's own zip format uses (this
    /// is a bare file, so neither applies). `validfrom`/`expiry` are
    /// populated from Card's own fields now; `balance`/`balancetype`
    /// (Catima features Cardigan has no equivalent for) are left blank.
    /// `group_ids`/`archived` don't have matching Catima columns at all,
    /// so they only round-trip through Cardigan's own JSON export, not
    /// this CSV.
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
    /// which of Cardigan's four supported import formats it is:
    ///  - a Cardigan export (JSON array, from `export_to` / the Export
    ///    Cards menu item) -- merged by id, like `export_to`'s inverse
    ///  - a Catima backup -- either the `.zip` Catima 2.0+ produces, or a
    ///    bare `.csv` for pre-2.0 exports
    ///  - a LoyaltyCardKeychain CSV export
    ///  - a UBcards `wallet.ini` (see parse_ubcards_wallet_ini)
    ///
    /// `.zip` is taken as a strong enough signal on its own (only Catima
    /// exports that way here), but a bare `.csv` extension doesn't say
    /// which of the two CSV-shaped formats it is -- Catima's bare export
    /// and LoyaltyCardKeychain's export are both plain CSV with different
    /// column sets, so detection there is by sniffing the header row
    /// itself rather than trusting the extension.
    ///
    /// Read as raw bytes and lossily decoded, rather than
    /// fs::read_to_string, because wallet.ini specifically can contain
    /// genuinely invalid UTF-8 (see parse_ubcards_wallet_ini) -- a no-op
    /// for every other already-valid-UTF-8 format this also detects.
    ///
    /// For the CSV/ini formats, only the card table is read; groups,
    /// categories, and star/archive/balance metadata (which Cardigan has
    /// no equivalent for) are intentionally dropped. Duplicate rows --
    /// matched against both the existing collection and earlier rows in
    /// the same import, by (barcode type, code value) -- are folded into
    /// the existing card rather than added again; see `append_imported`.
    ///
    /// Note: password-protected Catima exports aren't supported -- export
    /// without a password first if you used one.
    /// Returns ImportOutcome rather than a bare ImportSummary -- see
    /// that enum's own doc comment. Every known-format branch below is
    /// unchanged from before except being wrapped in
    /// ImportOutcome::Imported; only the final fallback (previously a
    /// bare Err) now tries this importer's own last-resort parsing (see
    /// try_fallback_parse) before actually giving up, returning
    /// NeedsFallbackConfirmation if it found anything at all, or the
    /// same "unrecognized format" error as before if not.
    ///
    /// Extension checks (pkpass/zip) stay first, as a fast path -- if
    /// either matches, there's no need to even read the file's own
    /// magic bytes at all. Otherwise, reads the file once and checks
    /// its actual magic bytes for an archive format regardless of
    /// extension (see sniff_archive_format) -- some apps export a known
    /// compressed format but rename or drop the extension, and
    /// extension alone would silently misroute those to the plain-text
    /// dispatch below instead. An archive found this way has every file
    /// inside it tried against import_bytes (see that function and
    /// try_import_from_archive), the same shared dispatch a non-archive
    /// file goes through directly right after.
    pub fn import_any(&mut self, path: &Path) -> Result<ImportOutcome, String> {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        // Checked before the generic .zip branch below -- a .pkpass file
        // *is* a zip (Apple Wallet's own container format), just with a
        // completely different internal layout (pass.json, not
        // catima.csv), so it needs its own extension check first rather
        // than falling into import_catima_zip and failing there.
        if extension.as_deref() == Some("pkpass") {
            return self.import_pkpass(path).map(ImportOutcome::Imported);
        }
        if extension.as_deref() == Some("zip") {
            return self.import_zip(path).map(ImportOutcome::Imported);
        }

        let bytes = fs::read(path).map_err(|e| e.to_string())?;

        if let Some(format) = sniff_archive_format(&bytes) {
            return self.try_import_from_archive(format, &bytes);
        }

        self.import_bytes(&bytes)
    }

    /// The shared text-format dispatch, extracted out of import_any so
    /// the same logic can also run against each file found inside an
    /// archive (see try_import_from_archive), not just a standalone
    /// file passed to import_any directly. Behavior is otherwise
    /// unchanged from what used to live directly in import_any.
    /// Merges a re-imported Cardigan-JSON export back in, matching by
    /// each card's own exact id (not by barcode content the way
    /// append_imported does for cross-app imports) -- this is
    /// specifically for Cardigan's own export format re-imported into
    /// Cardigan itself, where the same card really is the same id.
    /// Returns how many cards were processed (all of them, whether
    /// matched or newly added -- this format doesn't distinguish, since
    /// re-importing your own export is expected to just restore
    /// everything as-is).
    fn merge_cardigan_export(&mut self, imported: Vec<Card>) -> io::Result<usize> {
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

        // Checked before the Cardigan-JSON attempt below: both are bare
        // JSON arrays starting with '[', so without this, a Voucher
        // Vault export would reach serde_json::from_str::<Vec<Card>>
        // first and fail there with a confusing "missing field id"-style
        // error instead of ever reaching its own parser. "codeType" is
        // Voucher Vault's own field name (verified against its real
        // source), not one of Card's own serde field names, so this
        // doesn't risk misrouting a genuine Cardigan export.
        if text.trim_start().starts_with('[') && text.contains("codeType") {
            return self.append_imported(parse_vouchervault_json(&text)?).map(ImportOutcome::Imported);
        }

        if text.trim_start().starts_with('[') && !text.contains("[General]") {
            let imported: Vec<Card> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let count = self.merge_cardigan_export(imported).map_err(|e| e.to_string())?;
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

        // Last resort, before actually giving up -- see
        // try_fallback_parse's own doc comment for the shape-detection
        // order. Deliberately not auto-applied: the caller is expected
        // to confirm with the user first (see ImportOutcome).
        if let Some(cards) = try_fallback_parse(&text) {
            return Ok(ImportOutcome::NeedsFallbackConfirmation(cards));
        }

        Err("Unrecognized file format -- Cardigan can import its own export (.json), a Catima backup (.zip or .csv), a LoyaltyCardKeychain export (.csv), a UBcards wallet.ini, a Voucher Vault export (.json), or an Apple Wallet pass (.pkpass).".to_string())
    }

    /// Imports a single Apple Wallet pass. `.pkpass` is a zip containing
    /// (among other things) a `pass.json` and a cryptographic signature
    /// over the whole package -- signature verification is deliberately
    /// skipped entirely here, since the goal is just extracting the
    /// card data, not authenticating the pass's issuer the way a real
    /// wallet app would before trusting/displaying it.
    ///
    /// `pass.json`'s shape is Apple's own published PassKit format,
    /// checked against real documentation, not guessed: `description`
    /// is the pass's required human-readable name; the barcode lives in
    /// `barcodes` (an array, current-format passes) or the older
    /// singular `barcode` object (legacy passes, pre-iOS 9) -- checked
    /// in that order, first one present wins, as `message` (the encoded
    /// value) + `format` (a "PKBarcodeFormatXxx" string).
    /// `backgroundColor`, if present, is CSS-style `rgb(r, g, b)`, not
    /// hex -- converted rather than passed through as-is.
    /// `expirationDate` maps to Card's own `expiry` (truncated to just
    /// the date, same as Voucher Vault's `expires` handling). Exactly
    /// one of the five style dictionaries (boardingPass/coupon/
    /// eventTicket/storeCard/generic) is present per pass, per spec --
    /// each holds headerFields/primaryFields/secondaryFields/
    /// auxiliaryFields/backFields *nested inside it*, not at the top
    /// level (confirmed directly against real PassKit documentation,
    /// which was wrong to assume otherwise). None of that structured
    /// display data, nor `voided`, `logoText`, or `relevantDate`, has a
    /// dedicated Card field to go in -- summarized into `notes` instead
    /// rather than silently dropped. relevantDate specifically is *not*
    /// mapped to valid_from despite the surface similarity: it's a
    /// notification-surfacing hint (when iOS should prominently show
    /// this pass), not a validity constraint -- a boarding pass's
    /// relevantDate is commonly set to shortly before departure for
    /// lock-screen purposes, while the pass itself is typically valid
    /// from check-in, well before that. Mapping it to valid_from would
    /// make "Hide Presently Invalid Cards" hide a pass that's actually
    /// usable.
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

        /// Formats one JSON scalar (a field's "value" -- string, number,
        /// or bool depending on the field's own dateStyle/numberStyle
        /// hints) as display text. None for anything else (an object,
        /// array, or null value would be unusual here and isn't worth
        /// guessing a representation for).
        fn pkpass_scalar_to_string(value: &serde_json::Value) -> Option<String> {
            match value {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                serde_json::Value::Bool(b) => Some(b.to_string()),
                _ => None,
            }
        }

        /// Joins one field array (e.g. primaryFields) into a single
        /// "label: value, label: value" summary line, falling back to
        /// `key` when `label` is absent, and to bare `value` when
        /// neither is present. None if the array is absent or empty, or
        /// every field in it lacked a usable value.
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

        // Exactly one of these five should be present, per spec --
        // whichever it is names the pass's own style and holds its
        // structured display fields.
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
        // Just the date portion (first 10 characters of an ISO 8601
        // timestamp) -- Card's own expiry field doesn't carry
        // time-of-day, same handling as Voucher Vault's expires field.
        card.expiry = pass.expiration_date.map(|e| e.chars().take(10).collect());
        card.notes = extra_notes.join("\n");

        self.append_imported(vec![card])
    }

    /// Handles a magic-byte-detected archive (see sniff_archive_format,
    /// called from import_any) -- extracts every file inside (see
    /// extract_archive_files) and tries each one.
    ///
    /// catima.csv and loyalty_programs.csv are checked by filename
    /// first, matching import_zip's own existing special-casing for
    /// those two specific Catima/FidMe backup files -- needed because
    /// FidMe's own CSV format (unlike Catima's, which carries its own
    /// recognizable header text) isn't otherwise detectable by content
    /// at all, only by this known filename.
    ///
    /// Beyond those two, this runs two passes rather than one: a
    /// confidently-recognized format (ImportOutcome::Imported) in *any*
    /// file wins over a low-confidence fallback match
    /// (NeedsFallbackConfirmation) in an *earlier* one -- e.g. a README
    /// or LICENSE file that happens to sit before the real data file in
    /// the archive shouldn't win just by being tried first, if some
    /// other file in the same archive is actually a recognized format.
    /// One known, accepted limitation: if an unrelated file in the
    /// archive happens to false-positive match a known format before
    /// the real data file is reached, that false match wins outright
    /// (import_bytes already saves as part of matching a known format,
    /// not as a separate, undo-able step) -- considered an acceptable,
    /// rare edge case rather than justifying a more invasive dry-run-
    /// then-commit restructuring of import_bytes itself.
    fn try_import_from_archive(&mut self, format: ArchiveFormat, bytes: &[u8]) -> Result<ImportOutcome, String> {
        let files = extract_archive_files(format, bytes)?;

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

    /// Catima-zip-specific half of `import_any`: unzips `catima.csv` out
    /// of a Catima 2.0+ backup and hands it to the shared CSV parser.
    /// Both Catima and FidMe export a plain `.zip`, so the extension
    /// alone (already handled the pkpass case, which has its own
    /// distinct extension) doesn't say which -- opens the archive once
    /// and checks which of the two internal filenames is actually
    /// present.
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

    /// Appends freshly-parsed cards from any CSV import source and saves.
    /// Unlike the JSON import path, these formats have no id of their own
    /// to merge on, so duplicates are matched by (barcode type, code
    /// value) instead -- against both the existing collection and
    /// earlier rows already folded in from this same import (Catima and
    /// LoyaltyCardKeychain exports can themselves contain duplicate rows,
    /// e.g. a card exported once per group it belongs to). A match is
    /// folded into the existing card via `merge_into` rather than added
    /// as a second copy; a genuine duplicate that adds nothing new (e.g.
    /// no notes the existing card is missing) is effectively just
    /// skipped.
    ///
    /// Cards with no code value (`NoCode`, or a row that just didn't
    /// have one) have nothing reliable to match on, so they're always
    /// added as new rather than risking folding unrelated cards
    /// together.
    ///
    /// Public (not just used internally by import_any's own known-
    /// format branches) so window.rs can call this directly once the
    /// user confirms a fallback-parsed import -- see ImportOutcome.
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

/// Fills in whatever the existing card is missing from a newly-imported
/// duplicate, without overwriting anything the existing card already
/// has. Currently just notes -- the field most likely to differ between
/// two exports of what's otherwise the same card -- but broken out as
/// its own function so it's an easy spot to extend later if another
/// such field comes up.
fn merge_into(existing: &mut Card, imported: &Card) {
    if existing.notes.is_empty() && !imported.notes.is_empty() {
        existing.notes = imported.notes.clone();
    }
}

/// A prefix of Catima's card table header, used to locate where the card
/// rows start in the combined groups/cards/links CSV. Deliberately just a
/// prefix, not the full column list -- real exports have grown an extra
/// `barcodeencoding` column since the version this was first written
/// against, and matching only the stable leading columns avoids breaking
/// again the next time Catima adds one. See:
/// https://github.com/CatimaLoyalty/Android/wiki/Export-format
fn csv_io_err(e: csv::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e)
}

/// Inverse of parse_signed_argb_color: pack a "#RRGGBB" hex string back
/// into Catima's signed-32-bit-ARGB representation (opaque, alpha=FF).
fn hex_to_signed_argb(hex: &str) -> i64 {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
    let argb = 0xFF00_0000u32 | rgb;
    argb as i32 as i64
}

const CATIMA_CARD_HEADER_PREFIX: &str =
    "_id,store,note,validfrom,expiry,balance,balancetype,cardid,barcodeid,barcodetype";

/// LoyaltyCardKeychain's CSV export header, in full (unlike Catima's it's
/// not a prefix of a longer row -- LoyaltyCardKeychain's export is just
/// the card table on its own, no groups/links sections to skip past).
/// It happens to share Catima's column-naming convention for every field
/// Cardigan actually reads, minus `barcodeid` -- LoyaltyCardKeychain only
/// has `cardid`, which `parse_card_table_csv` already falls back to when
/// `barcodeid` isn't present.
const LOYALTYCARDKEYCHAIN_HEADER: &str =
    "_id,store,note,cardid,headercolor,headertextcolor,barcodetype";

/// An archive format detected by its actual magic bytes (see
/// sniff_archive_format), independent of file extension.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
}

/// Detects an archive format from its actual magic bytes, regardless of
/// file extension -- some apps export a known compressed format but
/// rename or drop the extension, which extension-only detection would
/// silently miss. Gzip and bzip2 both have reliable magic bytes at the
/// very start of the file (0x1F 0x8B and "BZh" respectively); a zip's
/// own local-file-header signature ("PK\x03\x04") is checked the same
/// way. Plain, uncompressed tar doesn't have a magic marker at offset 0
/// the same way -- the USTAR format (the modern standard since
/// POSIX.1-1988, which is what essentially every current tar
/// implementation writes) has a fixed "ustar" string at byte offset
/// 257 instead, checked here rather than attempting to fully validate
/// an older, non-USTAR tar header's own checksum, which would be
/// considerably more involved for a case that's now rare in practice.
fn sniff_archive_format(bytes: &[u8]) -> Option<ArchiveFormat> {
    if bytes.starts_with(b"PK\x03\x04") {
        return Some(ArchiveFormat::Zip);
    }
    if bytes.starts_with(&[0x1F, 0x8B]) {
        return Some(ArchiveFormat::TarGz);
    }
    if bytes.starts_with(b"BZh") {
        return Some(ArchiveFormat::TarBz2);
    }
    if bytes.len() >= 262 && &bytes[257..262] == b"ustar" {
        return Some(ArchiveFormat::Tar);
    }
    None
}

/// Extracts every regular file's name and raw contents out of a
/// detected archive, decompressing first if needed (TarGz/TarBz2).
/// Directory entries are skipped -- nothing to import there.
fn extract_archive_files(format: ArchiveFormat, bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    match format {
        ArchiveFormat::Zip => {
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
        ArchiveFormat::Tar => extract_tar_files(bytes),
        ArchiveFormat::TarGz => {
            let mut decoder = flate2::read::GzDecoder::new(bytes);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed).map_err(|e| e.to_string())?;
            extract_tar_files(&decompressed)
        }
        ArchiveFormat::TarBz2 => {
            let mut decoder = bzip2::read::BzDecoder::new(bytes);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed).map_err(|e| e.to_string())?;
            extract_tar_files(&decompressed)
        }
    }
}

/// The actual tar-entry-walking logic shared by ArchiveFormat::Tar
/// (used directly) and TarGz/TarBz2 (used after decompressing to a
/// plain tar byte stream first) in extract_archive_files just above.
fn extract_tar_files(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut archive = tar::Archive::new(bytes);
    let mut files = Vec::new();
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
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

/// Guesses a barcode format from the encoded value's own shape, for
/// sources (currently just FidMe) that provide no type information at
/// all. Deliberately conservative -- based on real, verifiable
/// constraints each format actually has, not vibes: EAN/UPC/ITF are
/// strictly digit-only, so any letter or symbol rules all three out
/// immediately; EAN-13/UPC-A/EAN-8 have fixed, standard lengths; ITF
/// specifically requires an *even* digit count, since it encodes digits
/// in pairs -- an odd-length digit-only value literally cannot be ITF.
/// Two- and 2D formats (QR, Aztec, DataMatrix, PDF417) are deliberately
/// excluded from this -- unlike the length/character-class constraints
/// above, there's no comparable content-shape signal that would
/// distinguish "should be QR" from "should be a 1D format," so guessing
/// there would be closer to a coin flip than an inference. This is a
/// best-effort guess, not a determination -- still very possibly wrong,
/// just meaningfully better odds than a single fixed fallback for every
/// card regardless of what the value actually looks like.
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
            // EAN-8 and UPC-E can both be 8 digits -- EAN-8 is the more
            // standard, widely-used of the two internationally, so it's
            // the tiebreak default here rather than UPC-E.
            8 => BarcodeType::Ean8,
            // UPC-E's compressed representations.
            6 | 7 => BarcodeType::UpcE,
            n if n % 2 == 0 => BarcodeType::Itf,
            // An odd-length digit-only value can't be ITF, and doesn't
            // match any of the fixed-length formats above -- Code128
            // handles arbitrary-length numeric content without issue,
            // and is common enough for loyalty account numbers
            // specifically to be a reasonable default here.
            _ => BarcodeType::Code128,
        }
    } else {
        // Contains a letter or symbol -- rules out every strictly
        // digit-only format above. Code39/Code93 could represent some
        // of these (uppercase + digits + a limited symbol set), but
        // can't represent lowercase letters at all, and Code128 is a
        // strict superset of what either can encode -- so Code128
        // covers this whole bucket correctly rather than needing to
        // distinguish cases within it.
        BarcodeType::Code128
    }
}

/// Parses FidMe's `loyalty_programs.csv` (found inside its export zip).
/// Verified against Catima's own real importer source
/// (FidmeImporter.java), not guessed: semicolon-delimited (not comma),
/// with columns Retailer/Program/Added At/Reference/Firstname/Lastname.
/// There's no barcode-type column at all -- FidMe's own export genuinely
/// doesn't include one (Catima's importer has the same gap, with a
/// comment noting they'd need their own store-to-barcode-type database
/// to fill it in) -- so the barcode type is inferred from the Reference
/// value's own shape instead (see guess_barcode_type_from_value) rather
/// than defaulting every single card to NoCode regardless of what it
/// looks like. Still just a best-effort guess, and may need correcting
/// afterward. There's also no dedicated notes field; Program/Added
/// At/name are combined into notes instead, matching Catima's own
/// approach, since dropping them entirely would lose real information
/// the export does contain.
///
/// One correctness detail carried over deliberately: Catima's importer
/// skips any row with an empty Reference (FidMe itself blanks this out
/// for expired cards -- see CatimaLoyalty/Android#1005), since a card
/// with no ID has nothing usable to import. Same behavior here.
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
            // See doc comment above -- FidMe blanks this on expired
            // cards, and a card with no ID isn't usable.
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

/// Parses a Voucher Vault export -- a bare JSON array (not wrapped in an
/// object), one object per voucher. Verified against both Voucher
/// Vault's real source and Catima's own real importer
/// (VoucherVaultImporter.java), not guessed: `description` (store name,
/// required), `code` (the encoded value, required), `codeType` (one of
/// CODE128/CODE39/EAN13/PDF417/QR/TEXT -- TEXT meaning no real barcode),
/// `color` (one of GREY/BLUE/GREEN/ORANGE/PURPLE/RED/YELLOW), and
/// `expires` (optional, "yyyy-MM-ddTHH:mm:ss.SSS" UTC -- truncated to
/// just the date here, since Card's own expiry field is a plain
/// "YYYY-MM-DD" string). Catima's importer *errors out* entirely on an
/// unrecognized codeType or color value (both are meant to be closed
/// enums) -- this is more lenient: an unrecognized codeType falls back
/// to NoCode and an unrecognized color falls back to
/// stable_color_for, rather than failing the whole import over one bad
/// row.
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
        // Just the date portion (first 10 characters of
        // "yyyy-MM-ddTHH:mm:ss.SSS") -- Card's own expiry field doesn't
        // carry time-of-day.
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

/// Normalizes a label for matching: lowercase, strip spaces/underscores/
/// hyphens. "Expiration Date", "expiration_date", and "Expiration-Date"
/// all collapse to the same "expirationdate", so the synonym lists below
/// only need to cover genuinely different words, not case/spacing
/// variants of the same word.
fn normalize_label(label: &str) -> String {
    label
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Synonym lists, one per field, ordered most-canonical-first -- this
/// order is what settles it when two different labels in the same
/// record both match the same field (see match_field_synonym below): the
/// earlier a synonym appears in its own field's list, the more
/// unambiguous a signal it is that this label really means that field,
/// so it wins. Generic terms like "value" or "id" sit at the end of
/// their list specifically because they could plausibly mean all sorts
/// of things in a file that wasn't designed with Cardigan in mind,
/// unlike something like "barcode" or "cardnumber".
///
/// "card" (bare) is in NAME_SYNONYMS specifically because a real,
/// user-provided example file used exactly that as its name label
/// ("Card1: Murphy's USA") -- not a guess, an observed real case.
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

/// Tries to match a raw label to a Card field. Exact match (after
/// normalize_label above) against each field's own synonym list first;
/// if nothing matches, strips trailing ASCII digits and tries again --
/// handles a file where every label under a given card shares that
/// card's own number ("Code1", "Card1" both belong to record 1, not two
/// different fields named "Code" and "Card"), confirmed against the
/// same real example file. Returns the matched field and how far down
/// its own synonym list the match was found (0 = most canonical), used
/// by parse_label_value_blocks to resolve collisions when two labels in
/// the same record both match the same field.
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
        // Only retry if trimming actually removed something -- an
        // all-digit or already-trailing-digit-free label would
        // otherwise just repeat the identical, already-failed lookup.
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

/// Counts how many cells in a row/column match a known field synonym at
/// all (any field, not narrowed to one) -- used purely as a "does this
/// look like a header" signal, not for actually resolving which cell
/// maps to which field (match_field_synonym itself, called per matched
/// header cell once a location is chosen, handles that separately).
fn count_synonym_matches<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.filter(|c| match_field_synonym(c).is_some()).count()
}

/// Checks all four edges of a headerless grid for header values before
/// falling back to content-based heuristics, by request -- explicit
/// labels, wherever they happen to be positioned, are always a more
/// reliable signal than guessing from the data's own shape.
///
/// Top row gets a fast path: if at least 3 of its cells match a known
/// synonym, it's accepted immediately as the header without even
/// checking the other three edges -- top-row-as-header is overwhelmingly
/// the standard convention, and requiring several matches (not just one)
/// keeps a coincidental single match from being mistaken for a real
/// header row.
///
/// Otherwise, all four edges (top row, first column, bottom row, last
/// column, in that order) are tallied and compared -- whichever has the
/// strictly highest count wins, even if that count is below the top
/// row's own fast-path threshold of 3 (a 2-column file can only ever
/// have 2 matches at most, for instance, so requiring 3 there would mean
/// never detecting a header at all). If two or more edges tie for the
/// highest count -- including a tie at zero, meaning nothing matched
/// anywhere -- there's no reliable signal favoring one location over
/// another, so this reports no header at all (None) rather than
/// guessing among equally-plausible candidates. A real, common way this
/// happens: the shared top-left corner cell is the only match anywhere,
/// contributing equally to both the top-row and first-column tallies,
/// which is genuinely ambiguous with just that one shared clue.
///
/// Only meaningful for a real grid -- requires at least 2 rows and at
/// least 2 columns in the first row; a single-column file is the
/// separately-scoped "plain list of bare codes" case, not a grid with a
/// header/data distinction at all, and a single row can't distinguish
/// "top" from "bottom" in any meaningful sense.
/// Converts raw CSV or TSV text into a grid (`Vec<Vec<String>>`), for
/// feeding into detect_header/parse_grid below. Delimiter (comma vs
/// tab) is guessed by counting each in the first few non-empty lines --
/// whichever is more common wins; a plain-text file with neither present
/// at all returns None, since it isn't grid-shaped data to begin with.
/// Reads with headers disabled deliberately -- detect_header itself
/// needs to see every row, including a possible header, to check all
/// four edges (top/bottom/first-column/last-column), not just whichever
/// row the csv crate's own header-stripping would assume is the header
/// (always the first row).
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
        // Skips genuinely empty rows (every cell blank) -- a stray blank
        // line in the source file shouldn't become a row of empty
        // strings that detect_header/parse_grid then have to reason
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

fn detect_header(grid: &[Vec<String>]) -> Option<HeaderLocation> {
    if grid.len() < 2 || grid.first().map_or(true, |row| row.len() < 2) {
        return None;
    }

    let top_row = grid.first()?;
    let bottom_row = grid.last()?;
    // .get(0)/.last() per row rather than assuming every row is exactly
    // as wide as the first -- stays safe against ragged/malformed rows
    // in a real-world file instead of assuming perfectly rectangular
    // data, at the cost of "last column" not necessarily being one
    // single consistent column index if row lengths actually vary.
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
    // More than one edge tied for the lead -- no clear winner, per the
    // rule this was built against.
    if winners.next().is_some() {
        return None;
    }
    Some(winner.0)
}

/// Assigns each header column to at most one CardField, resolving
/// collisions the same priority-based way parse_label_value_blocks does
/// per-record -- except here it only needs to happen once, at the
/// header level, rather than per row, since a real header's column
/// meanings are fixed across every data row rather than re-decided line
/// by line. Whichever column matched the most canonical (lowest
/// priority-number) synonym for a given field wins that field; every
/// other column that also matched the same field, or matched nothing at
/// all, gets None -- parse_grid folds an unassigned column's per-row
/// values into notes instead, the same "don't discard real information"
/// treatment used everywhere else in this importer.
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

/// Rebuilds `grid` with rows and columns swapped -- used when
/// detect_header finds the header in a column rather than a row, so the
/// rest of parse_grid can assume "header is a row, data is subsequent
/// rows" uniformly regardless of which edge it actually came from.
/// Length-safe against ragged rows (a shorter row simply doesn't
/// contribute a cell to columns past its own end, rather than panicking
/// or assuming every row is exactly as wide as the widest one).
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

/// Turns a headerless grid (rows of cells, e.g. from CSV/TSV) into
/// cards, using detect_header to find the header wherever it actually
/// is and map_header_columns to resolve which column means what.
/// Returns None if detect_header can't find a header anywhere -- the
/// content-based "guess from what the data looks like" fallback for
/// that case is a deliberately separate, not-yet-built piece (see this
/// session's own earlier scoping), not something this function attempts
/// on its own.
///
/// Column-to-field mapping happens once, at the header level (see
/// map_header_columns), not per row the way parse_label_value_blocks'
/// per-line labels need -- a real header's meaning doesn't change from
/// one data row to the next.
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

/// Builds cards from data rows given a column-to-field mapping and the
/// header row those column labels came from (used only for labeling
/// unrecognized columns in notes -- pass an empty header, e.g. &[], for
/// a headerless grid, and those notes lines fall back to the bare cell
/// value with no label prefix instead). Shared between parse_grid's own
/// header-found path above and guess_headerless_grid_columns below, so
/// both paths build cards identically rather than risking two
/// independently-maintained copies of the same logic drifting apart.
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
/// entirely alphanumeric, and contains at least one digit -- the digit
/// requirement specifically distinguishes a real code ("123456789",
/// "ABC123XYZ") from a single-word store name ("Costco"), which would
/// otherwise also pass a plain no-whitespace/alphanumeric check.
fn cell_looks_code_like(cell: &str) -> bool {
    let trimmed = cell.trim();
    !trimmed.is_empty()
        && !trimmed.contains(char::is_whitespace)
        && trimmed.chars().all(|c| c.is_ascii_alphanumeric())
        && trimmed.chars().any(|c| c.is_ascii_digit())
}

/// A cell looks like a name if it contains at least one letter and
/// isn't itself code-like -- excludes a value like "ABC123" (which does
/// contain letters) from counting as name-like, since that reads more
/// as a code than a name; see cell_looks_code_like just above.
fn cell_looks_name_like(cell: &str) -> bool {
    let trimmed = cell.trim();
    !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphabetic()) && !cell_looks_code_like(trimmed)
}

/// For a grid with no detectable header (detect_header returned None),
/// guesses which column is the code value and which is the name by what
/// the data itself looks like, rather than by fixed position. Verified
/// via a Python simulation against several hand-crafted grids --
/// including one with a phone-number column that's also code-like
/// alongside the real code column, confirming the ambiguous-column
/// case below actually declines to guess, not just in theory -- before
/// porting.
///
/// Deliberately conservative, matching this importer's own "blank over
/// wrong" philosophy throughout: a column only gets a guessed role if
/// it's the *unique* column meeting an 80%-of-non-empty-cells bar for
/// that role -- two candidate code columns, or zero, means no code
/// guess at all rather than picking one arbitrarily; same independently
/// for name, so one ambiguous field doesn't discard a confident guess
/// for the other. No column is ever guessed as barcode type, color,
/// expiry, or valid_from here -- those fields are only ever mapped from
/// an explicit header name elsewhere in this file (see
/// map_header_columns), and a headerless grid has no header name to go
/// on at all, so those fields simply aren't attempted here; any column
/// not identified as code or name folds into notes instead, the same
/// fallback every unrecognized column already gets (bare cell value,
/// no label prefix, since there's no header text to label it with --
/// see build_cards_from_grid_columns's own handling of an empty header).
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

/// The simplest, most permissive fallback of this whole importer -- a
/// plain list of bare values, one per line, with no labels and no grid
/// structure at all. Every non-blank line becomes its own card, named
/// generically ("Imported Card N") with the line's own trimmed text as
/// the code value -- there's no name to guess here at all, unlike the
/// headerless-grid case above, since a single-column file has nothing
/// to distinguish a name column from a code column with in the first
/// place.
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

/// This importer's own top-level dispatch across its three supported
/// shapes, tried in order: "label: value" blocks, then a CSV/TSV grid
/// (with or without a detectable header), then a plain bare list as the
/// final catch-all. Returns None only if even the bare-list fallback
/// (which accepts nearly anything with at least one non-blank line)
/// found nothing at all -- an empty or whitespace-only file.
///
/// Label:value is gated by an upfront shape check (at least 80% of
/// non-blank lines contain a colon) before even attempting it, unlike
/// the grid and bare-list attempts below -- parse_label_value_blocks
/// itself always returns *something* for any input, even text that was
/// never actually label:value-shaped at all (a CSV file's own comma-
/// separated lines don't contain colons, so they'd mostly become
/// notes-only cards with no real fields recognized), so an emptiness
/// check after the fact wouldn't catch a bad match here the way it does
/// for the other two shapes below.
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
/// separation -- e.g.:
/// ```text
/// Card1: Murphy's USA
/// Code1: 123456
///
/// Card2: Buc-ee's
/// Code2: 245263456
/// Expiration Date2: 2031-06-11
/// ```
///
/// Confirmed against a real, user-provided example file, not
/// hypothetical -- including the specific quirk that every label under
/// one card shares that card's own trailing number (see
/// match_field_synonym's own doc comment). Blank-line separation is the
/// *only* signal used for where one record ends and the next begins --
/// deliberately not "does this line have a trailing digit," since a file
/// might only number the first label in each block and leave the rest
/// bare, which would make trailing-digit presence an unreliable way to
/// detect a new record starting.
///
/// A label that doesn't match any known field folds into notes as
/// "Label: value" text, preserving the original label for context,
/// rather than being silently dropped -- same "don't discard real
/// information" approach the other importers already take with their
/// own extra/unrecognized fields. Same treatment for a *second* label
/// that matches a field already filled by a higher-priority match in
/// this same record (see match_field_synonym): the lower-priority one's
/// raw text still lands in notes rather than being thrown away outright.
///
/// Missing fields get the same safe, low-confidence-friendly defaults
/// used elsewhere in this app: no name-like label anywhere in the record
/// -> a generic "Imported Card N"; no explicit type-like label -> the
/// existing guess_barcode_type_from_value heuristic against whatever
/// code value was found (or NoCode if there wasn't one either); no
/// color-like label -> stable_color_for, the same deterministic
/// name-based color assignment other importers already fall back to.
fn parse_label_value_blocks(text: &str) -> Vec<Card> {
    // Groups consecutive non-blank lines together; one or more blank
    // lines in between starts a new group. Starts with one empty group
    // so there's always somewhere to push into; only pushes a fresh
    // group on a blank line if the current one isn't already empty,
    // which keeps multiple consecutive blank lines (or leading ones)
    // from producing extra, empty groups -- the trailing filter below
    // cleans up whatever empty groups remain regardless (an entirely
    // blank file, or one ending in blank lines).
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
        // value/priority separately -- confirmed as a real, necessary
        // fix (not just theoretical) against a real example file: a
        // displacement was pushing the *outgoing* value to notes
        // labeled with whatever line is currently being processed
        // ("Name: 1" instead of the correct "Card: 1"), since the old
        // version only had the value and priority to work with, not the
        // label that actually produced that value.
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
            // A line with no ':' at all doesn't fit this format's own
            // shape -- kept, not dropped, since discarding it silently
            // would lose whatever real information it held.
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
                        // A higher-priority match displaces whatever was
                        // there before -- the displaced (label, value)
                        // pair still goes to notes just below, correctly
                        // attributed to its own original label, rather
                        // than being lost or misattributed.
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
                // Notes has no real "priority" of its own worth tracking
                // -- every matched or unmatched notes-bound line just
                // accumulates, none of them displacing another.
                Some((CardField::Notes, _)) => extra_notes.push(value),
                None => extra_notes.push(format!("{label}: {value}")),
            }
        }

        // Whatever the highest-priority matched label said, used as-is
        // -- including a bare-digits value from something like a "Card:
        // 1" record-index marker with no separate Name field to
        // displace it. An earlier version rejected bare-digits names
        // specifically, falling back to the generic placeholder instead
        // -- reconsidered: a name of "1" is just as obviously,
        // transparently incomplete as the generic placeholder would be
        // (not the kind of plausible-but-wrong guess this parser's
        // "blank over wrong" philosophy actually exists to guard
        // against), trivial to fix either way, and keeps whatever the
        // source file's own numbering meant intact rather than
        // overwriting it with this function's own unrelated counter.
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

/// Parses a UBcards `wallet.ini` (`~/.local/share/ubcards/wallet.ini` on
/// the device) -- verified against UBcards' actual source
/// (cardstoragemodel.cpp), not guessed: it's a plain Qt `QSettings` INI
/// file, one `[General]` section holding an `all=` list of card UUIDs,
/// then one `[<uuid>]` section per card with `name`, `category`, `text`
/// (the barcode's encoded value -- confusingly not called "value" or
/// "code", but that's the literal QSettings key UBcards itself uses),
/// `type`, and `timestamp` keys.
///
/// Doesn't parse `all=` at all, deliberately -- every section other than
/// `[General]` already *is* a card by construction (cardstoragemodel.cpp
/// only ever creates a section when adding a card), so scanning for
/// every `[...]` section directly sidesteps needing to worry about
/// `all=`'s own list-serialization quirks. There's already visible
/// evidence those are real: the shell-script-based migration tool in the
/// same UBcards repo appends to this list with `" ,$uuid"` (space before
/// the comma), while Qt's own QSettings list serialization writes
/// `", "` (space after) -- so `all=` isn't even internally consistent
/// between UBcards' own UI and its bundled migration script. Splitting
/// on `,` and trimming each resulting id would tolerate either, but
/// there's no need to when just scanning for section headers avoids the
/// question entirely.
///
/// One real correctness wrinkle this has to work around: `timestamp` is
/// a QDateTime, and Qt's INI writer serializes non-string QVariant types
/// as `@Type(<raw QDataStream bytes>)` -- meaning wallet.ini is not
/// reliably valid UTF-8 as a *whole file* (confirmed directly in
/// UBcards' own migration script, which hardcodes a dummy
/// `timestamp=@DateTime(\0\0\0\x10\0\0\0\0\0\0%\x00\x00\x00\x00\0\0)`
/// value containing literal NUL and control bytes). import_any already
/// reads this file as lossily-decoded bytes rather than a plain string
/// for exactly this reason. The remaining risk that decoding doesn't
/// fully remove: a real (non-dummy) timestamp's raw bytes could
/// themselves happen to contain a literal `\n` byte, which would make a
/// naive line-split treat part of that binary blob as a spurious extra
/// "line". This parser never reads `timestamp` at all (Card has no
/// created-date field to put it in) and only recognizes a small,
/// specific set of patterns (`[...]` section headers, and `name=`/
/// `text=`/`type=`/`category=` keys) -- so a spurious split inside a
/// timestamp's binary data just produces garbage lines that don't match
/// any of those patterns and are silently skipped, rather than
/// corrupting a real field. This isn't a full, spec-compliant Qt-INI
/// parser (no escape-sequence or quoted-value handling) -- a reasonable
/// gap given real loyalty card names/numbers essentially never contain
/// characters that would need either.
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
        // "generic" is the default category every card gets unless the
        // person picked something else in UBcards' own UI -- not worth
        // surfacing for the common case, only when it's actually
        // meaningful information.
        if !section.category.is_empty() && section.category != "generic" {
            card.notes = format!("Category: {}", section.category);
        }
        cards.push(card);
    }
    cards
}

/// Maps a UBcards/ZBar symbology name to Cardigan's BarcodeType.
/// Verified against two sources in UBcards' own source, not guessed:
/// the manual-entry type dropdown in ubcards.qml (CODE-128, CODE-128-
/// mini, CODE-39, libre-CODE-39, EAN-13, EAN-8, I2/5) and, for a
/// camera-scanned card's `type`, qrcodereader.cpp setting it directly
/// from ZBar's own `get_type_name()` -- a wider set than the manual
/// dropdown (adds at least QR-Code, and potentially other symbologies
/// ZBar supports that never appear in the fixed dropdown, like UPC-A/E,
/// Code93, Codabar, PDF417). Case-insensitive match, since ZBar's and
/// Qt's exact capitalization wasn't verified to always agree.
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

/// Apple's own PassKit `PKBarcodeFormat*` constants -- documented,
/// stable, not guessed. Apple's PassKit doesn't support anywhere near as
/// many symbologies as Cardigan/rxing does; only these four exist.
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
/// `backgroundColor` in pass.json) into Cardigan's "#RRGGBB" hex, since
/// the two formats aren't interchangeable as-is.
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

/// Shared parser behind both `parse_catima_csv` and
/// `parse_loyaltycardkeychain_csv`. Columns are looked up by name rather
/// than fixed position, so a format-specific extra column (like Catima's
/// `barcodeencoding`) can't silently shift every field after it, and so
/// the same code handles both formats' differing column sets.
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
    let barcodeid_idx = col("barcodeid"); // None for formats without it (e.g. LoyaltyCardKeychain) -- falls back to cardid below
    let barcodetype_idx = col("barcodetype");
    let headercolor_idx = col("headercolor");
    // None for LoyaltyCardKeychain, which has no validfrom/expiry columns
    // at all -- get() already handles a missing column gracefully.
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

        // barcodeid holds the encoded value when Catima has one on file,
        // but in practice it's very often empty with the real value
        // sitting in cardid instead -- fall back to that.
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
/// `Color.parseColor` representation, e.g. `-45147`). Some cards simply
/// have no custom color set in Catima (an empty field, not a parse
/// failure -- confirmed against a real export) -- for those, derive a
/// stable color from the store name instead of one flat fallback, so
/// imported cards without a custom color still end up visually distinct
/// rather than all defaulting to the same blue.
fn parse_catima_color(field: &str, store_name: &str) -> String {
    match field.trim().parse::<i64>() {
        Ok(value) => format!("#{:06X}", (value as u32) & 0x00FF_FFFF),
        Err(_) => stable_color_for(store_name),
    }
}

/// Deterministic (same name -> same color every time) but effectively
/// arbitrary color, derived from a simple hash of the name. Fixed
/// saturation/lightness keep every generated color readable as a card
/// background regardless of hue.
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

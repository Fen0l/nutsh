//! Static catalog of Nutanix v4 API kinds.
//!
//! `generated.rs` is produced by `cargo xtask gen-catalog` from the OpenAPI specs under
//! `specs/` merged with the curated overlay in `curated.toml`. Never edit `generated.rs`
//! by hand.

// One struct per line on purpose: rustfmt would triple the file. The skip attribute on an
// out-of-line module is honoured by stable rustfmt.
#[rustfmt::skip]
mod generated;

pub mod path;
pub mod secret;
pub mod words;

pub use generated::{CURATED_GROUPS, KINDS, NAMESPACES, NAV, PAGES};

/// Properties that name an entity, in preference order. Shared by the generator (which puts
/// them first among fallback columns) and the client (which uses them for `Entity::name`).
pub const NAME_KEYS: &[&str] = &[
    "name",
    "hostName",
    "templateName",
    "title",
    "displayName",
    "username",
    "operation",
    "key",
    "clusterName",
    "containerName",
    "serviceName",
    "clientName",
    "ownerName",
];

/// HTTP method of a catalog action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// What a 2xx answer to an action carries: a task to watch, a document to show, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionReturn {
    Task,
    Payload,
    None,
}

/// How much damage an action can do. Orders the menu when no explicit `order` is curated -
/// safe first, destructive last, so a mistyped `enter` lands on something harmless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Danger {
    None,
    Low,
    Medium,
    High,
}

/// What the user has to do before an action runs. Ordered: a rule may raise an action's floor,
/// never lower it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfirmKind {
    None,
    Yes,
    TypeName,
}

/// The tightest budget the spec declares for an operation: `count` requests per `per_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub count: u32,
    pub per_secs: u32,
}

impl RateLimit {
    /// The budget for an operation whose spec declares no `x-rate-limit`.
    pub const DEFAULT: RateLimit = RateLimit {
        count: 5,
        per_secs: 1,
    };

    /// Whether `self` allows fewer requests per second than `other`. Cross-multiplied, so a
    /// `60 per 60s` tier compares correctly against a `2 per 1s` one without floating point.
    pub fn tighter_than(self, other: RateLimit) -> bool {
        u64::from(self.count) * u64::from(other.per_secs)
            < u64::from(other.count) * u64::from(self.per_secs)
    }
}

impl Default for RateLimit {
    fn default() -> Self {
        RateLimit::DEFAULT
    }
}

/// What a form field writes, and how the TUI collects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Text,
    Enum(&'static [&'static str]),
    Bool,
    Integer,
    /// A `{extId}` reference; the picker lists this catalog kind when one was resolved.
    Reference(Option<&'static str>),
    /// An object or array, as a seeded JSON skeleton the user edits as text.
    Json(&'static str),
}

/// One field of an action's request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The body path this field writes: `seg(\[n\])?(\.seg(\[n\])?)*`, e.g. `name`,
    /// `spec.hostExtId`, `vmRecoveryPoints[0].vmExtId`. A write grammar, deliberately not
    /// `core::path::get`, which reads and fans out over `[]` with no index.
    pub name: &'static str,
    pub label: &'static str,
    pub ty: FieldType,
    pub required: bool,
    /// A seed, or a substitution expanded at submit time by `core::actions::build_body`:
    /// `$ext_id`, `$now+<n>d`.
    pub value: Option<&'static str>,
    /// Filled from `value`, never shown and never edited.
    pub hidden: bool,
}

/// One past the largest `[n]` index a `Field.name` may address: `a[15]` is the last legal
/// one. A form that grows an array past this is a request body a terminal has no business
/// building by hand.
pub const MAX_FIELD_INDEX: usize = 16;

/// How many columns a table shows before `w`.
///
/// In the catalog rather than in `crates/tui/src/table.rs` so the generator and the drawer
/// share one number: `ui::render_rows` computes `table::status_index` over the columns it
/// *drew*, and `table::columns` slices to this unless `w` is held, so a curated `Status` at
/// index 6 would tint nothing and be silently dead. The generator refuses one.
pub const DEFAULT_COLUMNS: usize = 6;

/// The widest a curated detail label may be, and the width the pane lays its label column out at.
///
/// Beside [`DEFAULT_COLUMNS`] for the reason that one is here: the generator checks a curated
/// label against it and `nutsh_tui::detail` lays out against it, and a second copy would let the
/// two disagree silently. It is a **curation** constraint and a layout *target*, not a runtime
/// clip: a fallback label is whatever a leaf segment sentence-cases to, and one longer than this
/// is drawn on a line of its own rather than truncated into a collision with its neighbour.
pub const DETAIL_LABEL: usize = 18;

/// The widest an action label may be, curated or generated.
///
/// Derived from the narrowest box one is drawn in rather than picked: the action menu is
/// `DIALOG_PERCENT` of the body but never under `POPUP_WIDTH` (46), whose interior is 42 cells
/// once the border and the always-reserved gutter are taken. `ui::share` then gives the widest
/// key tag - `[ctrl-d]`, eight cells - its eight and the label the remaining 33.
///
/// A **curation** constraint, like [`DETAIL_LABEL`]: the generator refuses a longer curated
/// label, and the longest generated name in the catalog is `resume-synchronous-replication` at
/// thirty. A greyed row is a different question - its reason takes the tag's place and can be
/// long enough to cut the label - and that is the reason's job, not the label's.
pub const ACTION_LABEL: usize = 33;

/// The row budget of a kind whose overlay names none: ten pages of 100.
///
/// A Prism Central holds collections it never trims - the recording holds 181 825 audits - and a
/// page of those costs 0.85 s, so the collection is 1819 pages and 26 minutes of walking. It
/// never got that far: `scheduler::MAX_PAGES` cut every walk at 200 pages, so an unbudgeted
/// audit table spent about 170 s a cycle fetching 20 000 rows nobody reads, and showed an
/// empty body for all of it, on a kind that polls every 30 s. The cap is a backstop against a
/// paging loop, not a budget - it is the same 200 pages for every kind and says nothing about
/// which rows are worth having. Ten pages is under ten seconds at that latency.
///
/// It lives here, in the catalog, rather than in the scheduler, because the generator writes
/// it into every kind that does not curate one: reading `Kind::max_rows` then tells a reader
/// what the walk will actually do. The alternative - leaving `max_rows: None` in the catalog
/// and having `Subscription::list` read `None` as "apply the default" - makes the catalog lie
/// about its own kinds, which is the trap this constant exists to avoid. `None` therefore
/// means one thing everywhere, in a `Kind` and in a `Subscription` alike: no budget, walk it
/// all.
///
/// A budget hides rows, so it must never make them unreachable. Two things keep them in
/// reach: [`Kind::orderby`], curated on the collections that grow without bound, so the rows
/// the budget keeps are the newest rather than an arbitrary tenth; and the header's
/// `shown/total`, which prints the server's own count beside the shown one (`500/181825`),
/// so a truncation is stated rather than silent. Older rows are reached today by narrowing
/// the collection - a page pane's `$filter`, or a subscription's own `orderby` - and will be
/// reached by the filter grammar when it lands.
pub const DEFAULT_MAX_ROWS: u32 = 1000;

/// Whether `name` is a valid [`Field::name`]: `seg(\[n\])?(\.seg(\[n\])?)*`, `n` a
/// non-negative integer below [`MAX_FIELD_INDEX`].
///
/// This is a **write** grammar, deliberately not `core::path::get`, which reads and fans out
/// over `[]` with no index; the two never share code. The generator validates curated names
/// with it and `core::actions::build_body` walks the same shape, so an overlay typo fails at
/// generation rather than being sent as a literal key.
pub fn valid_field_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.split('.').all(|segment| {
        let (key, index) = match segment.split_once('[') {
            None => (segment, None),
            Some((key, rest)) => match rest.strip_suffix(']') {
                None => return false,
                Some(digits) => (key, Some(digits)),
            },
        };
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
        match index {
            None => true,
            Some(digits) => digits
                .parse::<usize>()
                .is_ok_and(|n| n < MAX_FIELD_INDEX && digits == n.to_string()),
        }
    })
}

/// How a column value is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Enum,
    /// An enum whose word carries the row's colour: rendered exactly as `Enum`, coloured by
    /// `nutsh_core::status::role_in` at the table layer. The first `Status` column of a view
    /// tints its row.
    Status,
    Bytes,
    /// An integer of seconds rendered as `0s`, `15m`, `6h`, `2d`. The mirror of `Bytes`:
    /// `recoveryPointObjectiveTimeSeconds` is an RPO, and `21600` is not an RPO anybody reads.
    Duration,
    Timestamp,
    /// An IPv4 address. Renders exactly as `Text`; its whole job is the width. At 120 columns
    /// with the sidebar open the VM table's `IP` column is `Text`, therefore `Flex(1)`,
    /// therefore it loses every weight fight with the `Flex(6)` name column - the live    /// frame shows `192.0.2.135` where the entity has `192.0.2.135,192.0.2.102`, silently
    /// cut. An IPv4 is fifteen cells and never more, and a `Cap` says so.
    Ip,
    /// An integer percentage: `40` → `40%`. Both the Disaster Recovery page's `%` column and
    /// the Dashboard's `PROGRESS` column render one today as a bare integer.
    Percent,
    /// Epoch **microseconds**, rendered as an age exactly as `Timestamp` is. `curated.toml`'s
    /// Host block is the column it repairs - `UPTIME ← bootTimeUsecs`, which `Timestamp` read
    /// as seconds and cannot render.
    Micros,
    Bool,
    Count,
    Reference,
}

impl ColumnKind {
    /// The variant's own name, which is exactly how a column kind is spelled in `curated.toml`,
    /// in `pages.toml` and in the generated file. Exhaustive, so a new variant is a compile
    /// error here rather than a `gen-catalog` failure at run time.
    pub fn as_str(self) -> &'static str {
        match self {
            ColumnKind::Text => "Text",
            ColumnKind::Enum => "Enum",
            ColumnKind::Status => "Status",
            ColumnKind::Bytes => "Bytes",
            ColumnKind::Duration => "Duration",
            ColumnKind::Timestamp => "Timestamp",
            ColumnKind::Ip => "Ip",
            ColumnKind::Percent => "Percent",
            ColumnKind::Micros => "Micros",
            ColumnKind::Bool => "Bool",
            ColumnKind::Count => "Count",
            ColumnKind::Reference => "Reference",
        }
    }

    /// Every variant, in the order the enum declares them. What the generator validates a
    /// curated `kind = "..."` against, so the vocabulary lives beside the enum and not in a
    /// hand-kept copy per caller.
    pub const ALL: &'static [ColumnKind] = &[
        ColumnKind::Text,
        ColumnKind::Enum,
        ColumnKind::Status,
        ColumnKind::Bytes,
        ColumnKind::Duration,
        ColumnKind::Timestamp,
        ColumnKind::Ip,
        ColumnKind::Percent,
        ColumnKind::Micros,
        ColumnKind::Bool,
        ColumnKind::Count,
        ColumnKind::Reference,
    ];

    /// The overlay's spelling, exactly, like [`Role::from_name`]: the generator refuses anything
    /// else, so a typo in `curated.toml` fails generation rather than reaching a frame.
    pub fn from_name(name: &str) -> Option<ColumnKind> {
        ColumnKind::ALL.iter().copied().find(|k| k.as_str() == name)
    }
}

/// The colour role a status word carries.
///
/// It lives here rather than in `nutsh-core` because `curated.toml` overrides roles per kind
/// and the generated catalog must therefore name the enum; `catalog` cannot depend on `core`,
/// the dependency runs the other way. The colours themselves are the TUI's
/// (`theme::role_fg`, `theme::row_fg`); this is only the vocabulary's alphabet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Ok,
    Off,
    Pending,
    Error,
    Warn,
    Info,
    Ending,
    Muted,
    Neutral,
}

/// The spellings `curated.toml` accepts, in the order the design table lists them.
pub const ROLE_NAMES: &[&str] = &[
    "Ok", "Off", "Pending", "Error", "Warn", "Info", "Ending", "Muted", "Neutral",
];

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Ok => "Ok",
            Role::Off => "Off",
            Role::Pending => "Pending",
            Role::Error => "Error",
            Role::Warn => "Warn",
            Role::Info => "Info",
            Role::Ending => "Ending",
            Role::Muted => "Muted",
            Role::Neutral => "Neutral",
        }
    }

    /// The overlay's spelling, exactly: the generator refuses anything else, so a typo in
    /// `curated.toml` fails generation rather than silently colouring a row `Neutral`.
    pub fn from_name(name: &str) -> Option<Role> {
        match name {
            "Ok" => Some(Role::Ok),
            "Off" => Some(Role::Off),
            "Pending" => Some(Role::Pending),
            "Error" => Some(Role::Error),
            "Warn" => Some(Role::Warn),
            "Info" => Some(Role::Info),
            "Ending" => Some(Role::Ending),
            "Muted" => Some(Role::Muted),
            "Neutral" => Some(Role::Neutral),
            _ => None,
        }
    }
}

/// A status word in the shape both the generator and the runtime compare: trimmed, without a
/// leading `$`, `-` and space folded to `_`, uppercase.
///
/// The `$` matters: every enum definition in `specs/` carries `$UNKNOWN` and all but four also
/// carry `$REDACTED`, so every status renderer meets them.
pub fn normalize_status(word: &str) -> String {
    let trimmed = word.trim();
    trimmed
        .strip_prefix('$')
        .unwrap_or(trimmed)
        .chars()
        .map(|c| match c {
            '-' | ' ' => '_',
            c => c.to_ascii_uppercase(),
        })
        .collect()
}

/// Every key the TUI binds or reserves, in the spelling a curated action's `key` uses.
///
/// It lives in the catalog rather than in the TUI so the generator - which depends on this
/// crate and not on `nutsh-tui` - can refuse a curated action key that would shadow
/// navigation. `: / S w y J ? enter esc j k g G ctrl-r ctrl-c q`, the movement keys, and
/// `tab shift-tab ctrl-b ctrl-e O 0..9 h l left right` -
/// `h`/`l` collapse and expand a sidebar group, and `l` is also the Contexts screen's login;
/// the administration spec adds `a p P r s m ctrl-d space c`; the readable-views spec adds `Y`;
/// the visual curation spec adds `ctrl-o`, the mouse toggle.
///
/// `"c"` is the one entry with an exception: the administration generator allows it for an
/// action literally named `cancel`, so "cancel" means the same thing on every table.
pub const RESERVED_KEYS: &[&str] = &[
    "/",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    ":",
    "?",
    "G",
    "J",
    "O",
    "P",
    "S",
    "Y",
    "a",
    "backspace",
    "c",
    "ctrl-b",
    "ctrl-c",
    "ctrl-d",
    "ctrl-e",
    "ctrl-o",
    "ctrl-r",
    "ctrl-t",
    "ctrl-x",
    "down",
    "end",
    "enter",
    "esc",
    "g",
    "h",
    "home",
    "j",
    "k",
    "l",
    "left",
    "m",
    "p",
    "pagedown",
    "pageup",
    "q",
    "r",
    "right",
    "s",
    "shift-tab",
    "space",
    "tab",
    "up",
    "w",
    "y",
];

/// A table column: header text and the dotted path into the entity JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    pub header: &'static str,
    pub path: &'static str,
    pub kind: ColumnKind,
}

/// One labelled value of a detail view. The rendering vocabulary is [`Column`]'s, deliberately: a
/// `Bytes` field and a `Bytes` column go through the same `nutsh_core::cell::render_value`, so
/// `8 GiB` is `8 GiB` in both.
///
/// Not `Field` - that name is taken by the action form's field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailField {
    /// At most [`DETAIL_LABEL`] characters; the generator refuses a longer one.
    pub label: &'static str,
    pub path: &'static str,
    pub kind: ColumnKind,
    /// The condition under which the field is drawn at all. `None` is the common case: an absent
    /// value already renders one dim `-`, and a section of nothing but those already disappears.
    pub when: Option<&'static str>,
}

/// A titled group of fields. Order is declaration order; the layout fills left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailSection {
    pub title: &'static str,
    pub fields: &'static [DetailField],
    /// For a variant title, where an empty section would be indistinguishable from a wrong one:
    /// `bootConfig` is a `LegacyBoot` or a `UefiBoot`, and an empty "Boot" section would not say
    /// which.
    pub when: Option<&'static str>,
}

/// The property every polymorphic v4 object carries its variant tag in.
pub const OBJECT_TYPE: &str = "$objectType";

/// A parsed `when`: the three forms [`DetailSection::when`] and [`DetailField::when`] may take.
///
/// The grammar lives here, beside the types that carry it, so the generator's check and
/// `nutsh_core::detail::holds` cannot disagree about what `bootConfig.$objectType = UefiBoot`
/// means. There is no expression parser and there will not be one: a condition a section needs
/// that these three cannot say is a section that wants curating differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When<'a> {
    /// `path` - true when it resolves to a value that is not `null`.
    Present(&'a str),
    /// `!path` - true when it does not.
    Absent(&'a str),
    /// `path = VALUE` - true when the first value at `path` equals `VALUE`, case-insensitively so
    /// an enum may be written the way a person writes it; and, when `path`'s last segment is
    /// [`OBJECT_TYPE`], compared on the short name after the final `.`.
    Equals { path: &'a str, value: &'a str },
}

/// The `when` grammar. `None` for anything outside it, which the generator refuses, so a `when`
/// that reaches a frame is one of the three forms by construction.
pub fn parse_when(when: &str) -> Option<When<'_>> {
    let when = when.trim();
    if when.is_empty() {
        return None;
    }
    if let Some((path, value)) = when.split_once('=') {
        let (path, value) = (path.trim(), value.trim());
        // `!a = b` is neither form: a negation and a comparison in one condition is the
        // expression parser this grammar exists to avoid.
        if path.is_empty() || value.is_empty() || path.starts_with('!') {
            return None;
        }
        return Some(When::Equals { path, value });
    }
    match when.strip_prefix('!') {
        Some(rest) if !rest.trim().is_empty() => Some(When::Absent(rest.trim())),
        Some(_) => None,
        None => Some(When::Present(when)),
    }
}

/// The prefix of a path whose last segment is [`OBJECT_TYPE`], or `None` when it is not one:
/// `bootConfig.$objectType` → `Some("bootConfig")`, a bare `$objectType` → `Some("")`.
///
/// The generator resolves the prefix and matches the literal against that property's variant
/// names; `nutsh_core::detail::holds` uses it to know that the value it read is a tag and must be
/// shortened before it is compared.
pub fn object_type_prefix(path: &str) -> Option<&str> {
    match path.strip_suffix(OBJECT_TYPE)? {
        "" => Some(""),
        head => head.strip_suffix('.'),
    }
}

/// The short name of a `$objectType` value: everything after the last `.`. The only half of the
/// tag that is stable across Prism Central builds, and the same half `nutsh_core::detail`'s
/// fallback uses as a variant tag.
pub fn short_type(object_type: &str) -> &str {
    object_type.rsplit('.').next().unwrap_or(object_type)
}

/// A group of the sidebar's menu. Data, generated from `nav.toml`: the sidebar's model is a
/// flat list of groups whose items are leaves, because a subtree inside an item would need a
/// second collapse dimension the model does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavGroup {
    pub name: &'static str,
    pub items: &'static [NavItem],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavItem {
    pub label: &'static str,
    pub target: NavTarget,
    /// Why an item is greyed, for a feature the v4 API has no equivalent for.
    pub note: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTarget {
    Kind(&'static str),
    Page(&'static str),
    /// The Contexts screen; the sidebar expands this into one row per configured context.
    Contexts,
    /// The settings screen: what is set, what it is set to, and who set it.
    Settings,
    /// Nothing to open: drawn greyed with `note` as its reason.
    Missing,
}

/// A feature page: several kinds composed into one screen with a fixed grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageDef {
    pub id: &'static str,
    pub title: &'static str,
    pub panes: &'static [PaneDef],
    /// The height of each grid row: `0` shares what is left of the page, anything else is
    /// exact.
    pub row_heights: &'static [u16],
    /// Which summary box to compute, if any; the boxes themselves are code, since they
    /// compute rather than list.
    pub summary: Option<&'static str>,
    /// How many top rows the summary column spans.
    pub summary_rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneDef {
    pub title: &'static str,
    pub kind: &'static str,
    /// Empty means the kind's own default columns.
    pub columns: &'static [Column],
    /// An OData `$filter`, verbatim.
    pub filter: Option<&'static str>,
    /// An OData `$orderby`, verbatim.
    pub orderby: Option<&'static str>,
    pub row: u16,
    pub col: u16,
    pub weight: u16,
    /// What to say when the list comes back empty.
    pub empty: &'static str,
}

/// Which OData query parameters a list endpoint accepts, and whether any query parameter is required.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListParams {
    pub page: bool,
    pub limit: bool,
    pub filter: bool,
    pub orderby: bool,
    pub select: bool,
    pub expand: bool,
    /// At least one query parameter is `required: true`; a bare list call returns HTTP 400.
    pub required: bool,
}

/// A mutation on a kind: `create`, `update`, `patch`, `delete`, or a `$actions/<name>` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub name: &'static str,
    pub path: &'static str,
    pub method: Method,
    pub needs_etag: bool,
    /// Declares a `requestBody` with `required: true`.
    pub needs_body: bool,
    /// Declares a `requestBody` at all.
    pub takes_body: bool,
    pub returns: ActionReturn,
    /// Placeholder names left to right; the last is the entity when the action names one.
    pub scope: &'static [&'static str],
    pub roles: &'static [&'static str],
    // Curation (`curated.toml`); all defaulted for an uncurated action.
    /// The key that runs this action from a table: `"P"`, `"ctrl-d"`, `""` for none. A string
    /// rather than a `char` because `ctrl-d` is in the headline set and does not fit one, and
    /// because `RESERVED_KEYS` is already spelled this way.
    pub key: &'static str,
    pub label: &'static str,
    pub danger: Danger,
    pub confirm: ConfirmKind,
    pub order: u16,
    pub hidden: bool,
    /// A constant request body, as JSON text.
    pub body: Option<&'static str>,
    pub form: &'static [Field],
    pub rate: RateLimit,
}

impl Action {
    /// What a menu row reads: the curated label, or the catalog name when none is curated.
    pub fn title(&self) -> &'static str {
        if self.label.is_empty() {
            self.name
        } else {
            self.label
        }
    }

    /// Whether the path names an entity. The head of the path - everything before `/$actions/`
    /// - ends in a placeholder for every action but `create`, which POSTs to the list path.
    pub fn names_entity(&self) -> bool {
        let head = self
            .path
            .split_once("/$actions/")
            .map_or(self.path, |(head, _)| head);
        head.ends_with('}')
    }

    /// Whether this action is about one existing row rather than about the collection.
    ///
    /// [`Action::names_entity`] is the usual answer, and the URL is where it is usually written.
    /// The exception is a curated workflow that POSTs to *another* kind's list path naming this
    /// row in its body: the VM's `Create recovery point` is `RecoveryPoint:create` with the
    /// row's own id substituted into a hidden field, and it acts on this VM however its URL
    /// reads. A `$ext_id` substitution is exactly that claim, written down.
    ///
    /// What it separates out is `create`: a POST to the list path, which makes a *new* row and
    /// touches the one under the cursor not at all. A surface that says it is showing what can
    /// be done to `web-01` must not list it.
    pub fn acts_on_a_row(&self) -> bool {
        self.names_entity() || self.form.iter().any(|f| f.value == Some("$ext_id"))
    }

    /// Ids the caller must supply before the entity id: every placeholder but the entity's.
    pub fn parents_needed(&self) -> usize {
        if self.names_entity() {
            self.scope.len().saturating_sub(1)
        } else {
            self.scope.len()
        }
    }
}

/// An API namespace: the spec version its kinds were generated from, and every version a Prism
/// Central may serve it at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Namespace {
    pub name: &'static str,
    /// The version the kinds were generated from.
    pub version: &'static str,
    pub preview: bool,
    /// Every GA version present in `specs/`, newest first; `[version]` for a namespace that
    /// only has pre-release specs. Negotiation tries them in this order.
    pub versions: &'static [&'static str],
}

/// One listable API kind.
#[derive(Debug, PartialEq, Eq)]
pub struct Kind {
    /// Schema name with the version removed, e.g. `vmm.ahv.config.Vm`.
    pub id: &'static str,
    pub namespace: &'static str,
    pub version: &'static str,
    /// Oldest entry of the namespace's `versions` whose spec has this kind's list path: the
    /// kind exists on any Prism Central that serves that version or a newer one.
    pub since: &'static str,
    pub display: &'static str,
    /// Curated aliases first, then aliases derived from the path.
    pub aliases: &'static [&'static str],
    pub category: &'static str,
    pub poll_secs: u32,
    pub list_path: &'static str,
    pub get_path: Option<&'static str>,
    pub schema: &'static str,
    pub list_params: ListParams,
    /// The `$orderby` every list of this kind is sent with, curated in `curated.toml`; the
    /// client drops it when `list_params.orderby` says the endpoint does not take one, and a
    /// subscription that names its own order keeps it.
    ///
    /// Curated, not derived. The pinned specs do declare which properties are orderable, in
    /// the `x-odata-fields` vendor extension on the `$orderby` parameter - twelve fields for
    /// `listTasks` in `prism` v4.4, nine for `listAlerts` and seven for `listAudits` in
    /// `monitoring` v4.3 - and every order curated in `curated.toml` names a field from its
    /// own endpoint's list, checked by hand and cited there by line. The generator still does
    /// not check it: the extension is optional, so its absence would have to mean "allow
    /// anything", and an order carries a direction and may name several clauses, which is a
    /// parser the generator does not have. A wrong field is a 400 on the whole list, so the
    /// citation in the overlay is the guard until that check exists.
    pub orderby: Option<&'static str>,
    /// A monotone last-modified timestamp property this kind can be ordered by, curated in
    /// `curated.toml`. `Some` arms the change-probe: one `$limit=1` list ordered by this field
    /// descending, compared against the probe's **own** previous answer, in place of a walk.
    ///
    /// Usually *not* the field the table is ordered by. `Kind::orderby` is creation order -
    /// what a person scanning tasks or alerts wants - and a probe over a creation time cannot
    /// see a task go `RUNNING → SUCCEEDED`, which is the most important change on the busiest
    /// table in the program. The two orders name different rows almost always, which is why
    /// the probe keeps its own baseline and never compares against `rows[0]`.
    pub probe_by: Option<&'static str>,
    /// The row budget of a list walk over this kind: the walk stops once it has this many
    /// rows, whatever the server's total, and the header's `shown/total` says so.
    ///
    /// **Every generated kind carries one.** The generator writes the overlay's `max_rows`
    /// where `curated.toml` names one and [`DEFAULT_MAX_ROWS`] where it does not, so this
    /// field states the walk a reader will get instead of leaving the cap to be discovered in
    /// the scheduler. It exists for the kinds a real Prism Central fills without bound - the    /// recording's 181 825 audits are 1819 pages, walked to `scheduler::MAX_PAGES` at 200 of them
    /// and 170 s every 30 s cycle - where the newest few hundred rows, in `orderby` order,
    /// are the whole of what a table can show.
    ///
    /// `None` means no budget: walk the collection to its end. Only a hand-built `Kind` (a
    /// test fixture) can say it; see [`DEFAULT_MAX_ROWS`] for why the catalog never does.
    pub max_rows: Option<u32>,
    /// Id of the kind whose `get_path` prefixes this kind's `list_path`.
    pub parent: Option<&'static str>,
    pub actions: &'static [Action],
    /// The kind whose actions this kind borrows, when its own path cannot carry them:
    /// `clustermgmt.config.Host` borrows `clustermgmt.config.Host~hosts`, whose get path
    /// supplies `{clusterExtId}`.
    pub action_kind: Option<&'static str>,
    /// Dotted paths read out of a row to fill the leading placeholders the table cannot supply,
    /// left to right: `["cluster.uuid"]` for a flat Host.
    pub action_parents: &'static [&'static str],
    /// The list GET's rate tier.
    pub rate: RateLimit,
    /// The `$select` every ordinary list of this kind is sent with: the top-level properties a
    /// **table row** is read for, comma separated, generated as their union. `None` means send
    /// no `$select` and take the whole document.
    ///
    /// Set on the request by the scheduler, through `ListOptions::select`, and never by the
    /// client from this field. That distinction is the whole safety of it: `can_i` and the
    /// version probes reach `Client::list_page_at` with default options, and a `$select` applied
    /// there would drop `identities[].identityFilter` from the authorisation lists and grey
    /// every action on every kind as not permitted, silently.
    ///
    /// `None` on the thirteen kinds with no `get_path`. A composed detail is drawn over the
    /// document the store holds, and only a single-entity GET can hand it a whole one; a kind
    /// that cannot be fetched singly would lose everything the columns do not name, permanently
    /// and with nothing able to repair it. `None` too where the endpoint declares no `$select`,
    /// where the schema could not be found, where a property the table reads is not one the
    /// schema declares, and where the union names every property there is and narrows nothing.
    ///
    /// The union is what the code reads off a row, and the catalog's own consistency tests
    /// enumerate it: the identifier, the name (the curated `name_path` and the `NAME_KEYS` the
    /// fallback walks), the columns the table draws, the columns a page pane draws, the
    /// addresses `core::search` matches on, `probe_by`, `action_parents`, and `isCancelable`
    /// where a kind can be cancelled.
    pub select: Option<&'static str>,
    /// The property that identifies an entity of this kind. `"extId"` everywhere but storage
    /// containers, which carry `containerExtId` and no `extId` at all.
    pub ext_id_key: &'static str,
    /// Dotted path to the entity's display name, when `NAME_KEYS` cannot find it at the top
    /// level of the JSON. Empty means "use NAME_KEYS". Curated: `prism.config.DomainManager`
    /// carries its name at `config.name` and would otherwise name itself with its own extId,
    /// so every reference to it would stay an eight-character stub however well the cache
    /// were warmed.
    pub name_path: &'static str,
    /// Warm this kind's names at connect: `core::names` lists one page of it so a `Reference`
    /// cell resolves before anything has opened the kind it points at. The generator refuses
    /// it on a kind that cannot be listed on its own and on one that names itself with its
    /// own extId.
    pub warm: bool,
    /// Curated columns from the overlay; empty when none are curated.
    pub columns: &'static [Column],
    /// Curated detail sections; empty when none are curated, in which case `nutsh_core::detail`
    /// composes a fallback from the entity itself.
    ///
    /// One array, not the `columns`/`fallback_columns` pair. Columns keep both because `w` shows
    /// the derived ones *past* the curated ones; a detail has no `w`, so a fallback is not a
    /// continuation of the curation but what happens instead of it. And the arithmetic matters:
    /// 262 kinds × 6 sections × 8 fields is roughly 12 500 field literals appended to a
    /// `generated.rs` that is already 927 KB, for a list that could not adapt to what the Prism
    /// Central actually returned.
    pub detail: &'static [DetailSection],
    /// Columns derived from the schema; always non-empty.
    pub fallback_columns: &'static [Column],
    pub preview: bool,
    /// Has an entry in `curated.toml`.
    pub curated: bool,
    /// Per-kind status-word overrides from `curated.toml`, sorted by the normalised word.
    pub status_roles: &'static [(&'static str, Role)],
}

impl Kind {
    pub fn is_top_level(&self) -> bool {
        self.parent.is_none()
    }

    /// Whether a Prism Central pinned at `pinned` for this namespace has this kind's endpoints.
    pub fn served_at(&self, pinned: &str) -> bool {
        version_key(self.since) <= version_key(pinned)
    }

    pub fn action(&self, name: &str) -> Option<&'static Action> {
        self.actions.iter().find(|a| a.name == name)
    }

    /// The kind whose actions apply to a row of this one.
    pub fn action_target(&'static self) -> &'static Kind {
        self.action_kind.and_then(crate::kind).unwrap_or(self)
    }

    /// This kind's override for an already-normalised status word.
    pub fn status_role(&self, word: &str) -> Option<Role> {
        self.status_roles
            .binary_search_by(|(w, _)| (*w).cmp(word))
            .ok()
            .map(|i| self.status_roles[i].1)
    }
}

pub fn placeholder_count(path: &str) -> usize {
    path.matches('{').count()
}

/// Every `{...}` segment of `path` replaced left to right by `values`; `None` when the counts
/// differ, which is a programming error in the caller, never a user-facing state.
pub fn fill_placeholders(path: &str, values: &[&str]) -> Option<String> {
    if placeholder_count(path) != values.len() {
        return None;
    }
    let mut out = String::with_capacity(path.len());
    let mut values = values.iter();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}')? + start;
        out.push_str(&rest[..start]);
        out.push_str(values.next()?);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Kinds whose `parent` is `parent_id`, sorted by display name.
pub fn children(parent_id: &str) -> Vec<&'static Kind> {
    let mut kids: Vec<&'static Kind> = KINDS
        .iter()
        .filter(|k| k.parent == Some(parent_id))
        .collect();
    kids.sort_by(|a, b| a.display.cmp(b.display).then(a.id.cmp(b.id)));
    kids
}

/// The kind's ancestors, root first; empty for a top-level kind.
pub fn parent_chain(kind: &Kind) -> Vec<&'static Kind> {
    let mut chain = Vec::new();
    let mut current = kind.parent.and_then(crate::kind);
    while let Some(p) = current {
        chain.push(p);
        current = p.parent.and_then(crate::kind);
    }
    chain.reverse();
    chain
}

/// How a kind is opened. `Direct` from the palette; `FromParent` by drilling into a row of
/// the named parent, which supplies every placeholder in the list path; `NeedsParameter`
/// when a query parameter is required or a placeholder has no listable owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Direct,
    FromParent(&'static Kind),
    NeedsParameter,
}

impl Reach {
    /// The palette's subtitle for a kind that cannot be opened directly; `None` for `Direct`.
    pub fn reason(&self) -> Option<String> {
        match self {
            Reach::Direct => None,
            Reach::FromParent(p) => Some(format!("open from {}", p.display)),
            Reach::NeedsParameter => Some("needs a parameter".to_string()),
        }
    }
}

/// How `kind` is reached. A required query parameter greys it regardless of parents;
/// otherwise the parent chain must supply every placeholder of the list path.
pub fn reach(kind: &Kind) -> Reach {
    if kind.list_params.required {
        return Reach::NeedsParameter;
    }
    let chain = parent_chain(kind);
    if chain.len() != placeholder_count(kind.list_path) {
        return Reach::NeedsParameter;
    }
    match chain.last() {
        None => Reach::Direct,
        Some(parent) => Reach::FromParent(parent),
    }
}

/// `path` with its version segment replaced: `/vmm/v4.3/ahv/config/vms` with `v4.1` →
/// `/vmm/v4.1/ahv/config/vms`. Every API path is `/<namespace>/<version>/<rest>`; a path
/// without both a namespace and a version segment is returned unchanged. The generator and the client both
/// use this, so they cannot disagree on where the version lives.
pub fn with_version(path: &str, version: &str) -> String {
    let mut segments: Vec<&str> = path.split('/').collect();
    // A leading `/` yields an empty first segment, so the version sits at index 2.
    if segments.len() < 3 || !path.starts_with('/') {
        return path.to_string();
    }
    segments[2] = version;
    segments.join("/")
}

/// The version segment of an API path: `Some("v4.3")` for `/vmm/v4.3/ahv/config/vms`, `None`
/// for a path without both a namespace and a version segment. The inverse of `with_version`.
pub fn version_in(path: &str) -> Option<&str> {
    if !path.starts_with('/') {
        return None;
    }
    path.split('/').nth(2).filter(|v| !v.is_empty())
}

/// Rank of a GA version's stage: above every alpha and beta.
const GA_RANK: u64 = 2;

/// Orders API versions: alpha before beta before GA inside one `major.minor`, so
/// `v4.0.a3` < `v4.0.b1` < `v4.0` < `v4.1`. `v4.0.a3` → `[4, 0, 0, 3]`, `v4.0.b1` → `[4, 0, 1, 1]`,
/// `v4.0` → `[4, 0, 2, 0]`.
pub fn version_key(version: &str) -> [u64; 4] {
    let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    match parts.next() {
        Some(stage) => {
            let letter = stage.chars().next().unwrap_or('z');
            let rank = match letter {
                'a' => 0,
                'b' => 1,
                _ => GA_RANK,
            };
            let number = stage
                .get(letter.len_utf8()..)
                .unwrap_or("")
                .parse()
                .unwrap_or(0);
            [major, minor, rank, number]
        }
        None => [major, minor, GA_RANK, 0],
    }
}

pub fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

pub fn namespace(name: &str) -> Option<&'static Namespace> {
    NAMESPACES.iter().find(|n| n.name == name)
}

pub fn page(id: &str) -> Option<&'static PageDef> {
    PAGES.iter().find(|p| p.id == id)
}

/// The group and item labels that lead to `target`, for the header's breadcrumb.
pub fn nav_path(target: &NavTarget) -> Option<(&'static str, &'static str)> {
    NAV.iter().find_map(|g| {
        g.items
            .iter()
            .find(|i| i.target == *target)
            .map(|i| (g.name, i.label))
    })
}

/// Kinds matching `term`, best first: exact id, an alias of a curated kind, any alias,
/// display prefix, id substring. Within a rank, kinds that cannot be listed without
/// parameters sink, then sub-resources sink below top-level kinds.
pub fn lookup(term: &str) -> Vec<&'static Kind> {
    let t = term.as_bytes();
    let mut scored: Vec<(u8, bool, bool, &'static Kind)> = KINDS
        .iter()
        .filter_map(|k| {
            let rank = if k.id.eq_ignore_ascii_case(term) {
                0
            } else if k.aliases.iter().any(|a| a.eq_ignore_ascii_case(term)) {
                if k.curated { 1 } else { 2 }
            } else if starts_with_ignore_ascii_case(k.display, t) {
                3
            } else if contains_ignore_ascii_case(k.id, t) {
                4
            } else {
                return None;
            };
            let r = reach(k);
            Some((
                rank,
                matches!(r, Reach::NeedsParameter),
                matches!(r, Reach::FromParent(_)),
                k,
            ))
        })
        .collect();
    scored.sort_by(|a, b| (a.0, a.1, a.2, a.3.id).cmp(&(b.0, b.1, b.2, b.3.id)));
    scored.into_iter().map(|(_, _, _, k)| k).collect()
}

fn starts_with_ignore_ascii_case(haystack: &str, needle: &[u8]) -> bool {
    haystack
        .as_bytes()
        .get(..needle.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(needle))
}

fn contains_ignore_ascii_case(haystack: &str, needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_placeholders() {
        assert_eq!(placeholder_count("/a/{b}/c/{d}"), 2);
        assert_eq!(placeholder_count("/a"), 0);
    }

    /// Cross-multiplied, so a per-minute tier compares against a per-second one exactly.
    #[test]
    fn rate_limits_compare_by_requests_per_second() {
        let per_minute = RateLimit {
            count: 60,
            per_secs: 60,
        };
        let per_second = RateLimit {
            count: 2,
            per_secs: 1,
        };
        assert!(per_minute.tighter_than(per_second));
        assert!(!per_second.tighter_than(per_minute));
        assert!(
            !RateLimit::DEFAULT.tighter_than(RateLimit::DEFAULT),
            "equal is not tighter"
        );
        let half = RateLimit {
            count: 30,
            per_secs: 60,
        };
        let one = RateLimit {
            count: 1,
            per_secs: 1,
        };
        assert!(half.tighter_than(one));
        assert_eq!(RateLimit::default(), RateLimit::DEFAULT);
    }

    #[test]
    fn lookup_never_panics() {
        let _ = lookup("vm");
        assert!(lookup("").len() <= KINDS.len());
    }

    #[test]
    fn case_insensitive_helpers() {
        assert!(starts_with_ignore_ascii_case("Virtual Machines", b"virt"));
        assert!(!starts_with_ignore_ascii_case("VM", b"vms"));
        assert!(contains_ignore_ascii_case("vmm.ahv.config.Vm", b"CONFIG"));
        assert!(!contains_ignore_ascii_case("abc", b"abcd"));
        assert!(contains_ignore_ascii_case("abc", b""));
    }

    #[test]
    fn role_names_round_trip_and_reject_junk() {
        for name in ROLE_NAMES {
            let role = Role::from_name(name).unwrap_or_else(|| panic!("{name} is a role"));
            assert_eq!(role.as_str(), *name);
        }
        assert_eq!(Role::from_name("Ok"), Some(Role::Ok));
        assert_eq!(
            Role::from_name("ok"),
            None,
            "the spelling is the one in the table"
        );
        assert_eq!(Role::from_name("Nope"), None);
    }

    /// The one vocabulary the generator validates a curated `kind = "..."` against.
    #[test]
    fn column_kind_names_round_trip_and_reject_junk() {
        for k in ColumnKind::ALL {
            assert_eq!(
                ColumnKind::from_name(k.as_str()),
                Some(*k),
                "{}",
                k.as_str()
            );
        }
        let mut names: Vec<&str> = ColumnKind::ALL.iter().map(|k| k.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a name is in ALL twice");
        assert_eq!(ColumnKind::from_name("text"), None, "the spelling is exact");
        assert_eq!(ColumnKind::from_name("Nope"), None);
    }

    /// Normalisation is the catalog's so the generator and the runtime cannot disagree about
    /// which word an override matches.
    #[test]
    fn status_words_normalise_the_same_way_everywhere() {
        assert_eq!(normalize_status("  ON "), "ON");
        assert_eq!(normalize_status("$UNKNOWN"), "UNKNOWN");
        assert_eq!(normalize_status("in-progress"), "IN_PROGRESS");
        assert_eq!(normalize_status("Powered Off"), "POWERED_OFF");
        assert_eq!(normalize_status("$"), "");
    }

    /// Every key the TUI binds or reserves, in one place, so the administration generator can
    /// refuse a curated action key that would shadow navigation.
    #[test]
    fn reserved_keys_are_sorted_unique_and_hold_both_specs_claims() {
        assert!(
            RESERVED_KEYS.windows(2).all(|w| w[0] < w[1]),
            "sorted, so an addition lands in one obvious place"
        );
        for key in [
            ":",
            "/",
            "?",
            "0",
            "9",
            "G",
            "J",
            "O",
            "S",
            "a",
            "c",
            "ctrl-b",
            "ctrl-c",
            "ctrl-d",
            "ctrl-e",
            "ctrl-r",
            "ctrl-t",
            "enter",
            "esc",
            "h",
            "l",
            "left",
            "m",
            "p",
            "P",
            "q",
            "r",
            "right",
            "s",
            "shift-tab",
            "space",
            "tab",
            "w",
            "y",
        ] {
            assert!(RESERVED_KEYS.contains(&key), "{key} must be reserved");
        }
    }

    /// The three rules the generator enforces, asserted again on the committed catalog so a
    /// hand-edit of `generated.rs` cannot slip past them either.
    #[test]
    fn every_nav_target_resolves_and_is_openable() {
        for group in NAV {
            for item in group.items {
                match item.target {
                    NavTarget::Kind(id) => {
                        let k = kind(id).unwrap_or_else(|| panic!("{id} is not in KINDS"));
                        assert_eq!(
                            reach(k),
                            Reach::Direct,
                            "{id} is not openable from the menu"
                        );
                    }
                    NavTarget::Page(id) => {
                        assert!(page(id).is_some(), "{id} is not in PAGES");
                    }
                    NavTarget::Contexts | NavTarget::Settings | NavTarget::Missing => {}
                }
            }
        }
    }

    /// The curated groups, then what each namespace has left over, so nothing in the catalog is
    /// unreachable. `CURATED_GROUPS` is generated from `nav.toml`'s own group count, so adding a
    /// group to the file moves the digit keys and this assertion together.
    #[test]
    fn every_directly_openable_kind_is_in_some_group() {
        assert_eq!(CURATED_GROUPS, 11, "the eleven groups nav.toml curates");
        assert!(
            NAV.len() <= CURATED_GROUPS + NAMESPACES.len(),
            "a namespace contributes at most one group"
        );
        let listed: std::collections::HashSet<&str> = NAV
            .iter()
            .flat_map(|g| g.items)
            .filter_map(|i| match i.target {
                NavTarget::Kind(id) => Some(id),
                _ => None,
            })
            .collect();
        for k in KINDS {
            if reach(k) == Reach::Direct {
                assert!(
                    listed.contains(k.id),
                    "{} is unreachable from the menu",
                    k.id
                );
            }
        }
    }

    /// A namespace group is what the curated tree left over, never a second copy of it.
    ///
    /// Appending every `Reach::Direct` kind of a namespace with no regard for what `nav.toml`
    /// has already placed would draw all 71 curated kind rows a second time under their raw
    /// namespace - the whole menu, twice, under two different names, with `datapolicies` and
    /// `dataprotection` under the curated `Data Protection` they duplicate. And an empty group
    /// is a header over nothing, so it is not written at all.
    #[test]
    fn a_kind_is_in_one_group_and_no_group_is_empty() {
        let mut groups_of: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for g in NAV {
            assert!(!g.items.is_empty(), "{} is a header over nothing", g.name);
            for i in g.items {
                if let NavTarget::Kind(id) = i.target {
                    groups_of.entry(id).or_default().push(g.name);
                }
            }
        }
        let mut twice: Vec<String> = groups_of
            .iter()
            .filter(|(_, gs)| gs.len() > 1)
            .map(|(id, gs)| format!("{id} in {gs:?}"))
            .collect();
        twice.sort();
        assert!(twice.is_empty(), "{} duplicated: {twice:#?}", twice.len());
    }

    /// Every big curated group opens on a landing page, the way Data Protection opens on
    /// Disaster Recovery: the group's first item is a page over that group's own kinds, and the
    /// individual resources stay under it. Dashboard *is* a page, and Contexts and Settings are
    /// screens, so those three are the exceptions - and they are named, not skipped by shape,
    /// because a group that lost its page would otherwise look like a fourth exception.
    #[test]
    fn every_big_group_opens_on_a_landing_page() {
        let mut landed = Vec::new();
        for g in &NAV[..CURATED_GROUPS] {
            if matches!(g.name, "Dashboard" | "Contexts" | "Settings") {
                continue;
            }
            let first = g
                .items
                .first()
                .unwrap_or_else(|| panic!("{} has no items", g.name));
            let NavTarget::Page(id) = first.target else {
                panic!("{}'s first item is {:?}, not a page", g.name, first.target);
            };
            let def = page(id).unwrap_or_else(|| panic!("{} names no page {id}", g.name));
            assert!(!def.panes.is_empty(), "{id} has no panes");
            landed.push(id);
        }
        assert_eq!(landed.len(), 8, "eight groups land on a page: {landed:?}");
    }

    /// A page's panes are capped at six, its grid references rows that exist, and its columns
    /// name the kinds the design verified.
    #[test]
    fn the_two_pages_are_shaped_the_way_the_design_says() {
        let dr = page("disaster-recovery").expect("the DR page");
        assert_eq!(dr.panes.len(), 4);
        assert_eq!(dr.row_heights, &[5, 7, 0, 0]);
        assert_eq!(dr.summary, Some("disaster-recovery"));
        assert_eq!(dr.summary_rows, 2);
        assert_eq!(dr.panes[0].kind, "multidomain.config.RegisteredDomain");
        assert!(dr.panes[0].empty.contains("single PC"));
        assert_eq!(
            dr.panes[1].columns[2].kind,
            ColumnKind::Duration,
            "the RPO is a duration"
        );
        assert_eq!(
            dr.panes[1].columns[1].path, "replicationLocations[].domainManagerExtId",
            "the site is the domain manager, not the replication location's join label"
        );
        assert!(
            !dr.panes[1]
                .columns
                .iter()
                .any(|c| c.path.ends_with("isPrimary")),
            "every policy has exactly one primary location, so the column said nothing"
        );
        let dash = page("dashboard").expect("the Dashboard page");
        assert_eq!(dash.panes.len(), 4);
        assert_eq!(
            dash.panes[0].columns[1].kind,
            ColumnKind::Text,
            "nodes.numberOfNodes is an integer; Count over it printed 1 for every cluster"
        );
        assert_eq!(dash.panes[1].filter, Some("isResolved eq false"));
        assert_eq!(dash.panes[3].orderby, Some("name"));
        for p in PAGES {
            assert!(p.panes.len() <= 6, "{}", p.id);
            for pane in p.panes {
                assert!(kind(pane.kind).is_some(), "{}", pane.kind);
                assert!(usize::from(pane.row) < p.row_heights.len(), "{}", p.id);
            }
        }
    }

    /// A kind with no overrides answers `None` for every word, and one with them answers by
    /// the normalised word only.
    #[test]
    fn status_role_overrides_are_looked_up_by_the_normalised_word() {
        let vm = kind("vmm.ahv.config.Vm").expect("the catalog has VMs");
        assert_eq!(vm.status_role("ON"), None, "no overrides, no answer");
        let overriding: Vec<&str> = KINDS
            .iter()
            .filter(|k| !k.status_roles.is_empty())
            .map(|k| k.id)
            .collect();
        assert_eq!(
            overriding,
            [
                "clustermgmt.ahv.config.PcieDevice",
                "clustermgmt.config.Disk",
                "clustermgmt.config.Host",
                "clustermgmt.config.Host~hosts",
                "multidomain.config.RegisteredDomain",
                "objects.config.ObjectStore",
                "prism.config.Task",
                "vmm.content.Image",
                "volumes.config.VolumeGroup",
            ],
            "the kinds whose words the vocabulary cannot colour on its own"
        );
        // `DiskStatus` and `PcieDeviceState` are two whole enums the shared vocabulary has no
        // word of: every value would be `Role::Neutral`, so a disk migrating its data off and
        // a broken card would draw exactly like a healthy one.
        let disk = kind("clustermgmt.config.Disk").expect("the catalog has Disks");
        assert_eq!(disk.status_role("DETACHABLE"), Some(Role::Ending));
        assert_eq!(
            disk.status_role("DATA_MIGRATION_INITIATED"),
            Some(Role::Pending)
        );
        assert_eq!(disk.status_role("NORMAL"), None, "the vocabulary answers");
        let pcie = kind("clustermgmt.ahv.config.PcieDevice").expect("the catalog has PCIe devices");
        assert_eq!(pcie.status_role("HOST_BROKEN"), Some(Role::Error));
        assert_eq!(pcie.status_role("UVM_AVAILABLE"), Some(Role::Ok));
        assert_eq!(
            pcie.status_role("HOST_USED"),
            None,
            "which side of the hypervisor a working device is on is not a health word"
        );
        // The v4 spec spells a drained host `in_maintenanace`; the vocabulary knows neither
        // spelling, so without the override a host being drained reads as healthy.
        let host = kind("clustermgmt.config.Host").expect("the catalog has Hosts");
        assert_eq!(host.status_role("IN_MAINTENANACE"), Some(Role::Pending));
        assert_eq!(host.status_role("IN_MAINTENANCE"), Some(Role::Pending));
        assert_eq!(
            host.status_role("NORMAL"),
            None,
            "the vocabulary has NORMAL"
        );
        // `RegistrationState`, `ApiCredentialStatus` and `objects.v4.1.config.State` name the
        // thing in every word, and the vocabulary matches whole words: `REGISTRATION_ERROR` is
        // not `ERROR`, and `OBJECT_STORE_AVAILABLE` is not `AVAILABLE`.
        let domain =
            kind("multidomain.config.RegisteredDomain").expect("the catalog has registered PCs");
        assert_eq!(domain.status_role("REGISTRATION_ERROR"), Some(Role::Error));
        assert_eq!(domain.status_role("EXPIRED"), Some(Role::Error));
        assert_eq!(domain.status_role("NEAR_EXPIRY"), Some(Role::Warn));
        assert_eq!(
            domain.status_role("CONNECTED"),
            None,
            "connectivityStatus is a free string whose words the vocabulary knows"
        );
        let store = kind("objects.config.ObjectStore").expect("the catalog has object stores");
        assert_eq!(
            store.status_role("OBJECT_STORE_DEPLOYMENT_FAILED"),
            Some(Role::Error)
        );
        assert_eq!(store.status_role("OBJECT_STORE_AVAILABLE"), Some(Role::Ok));
        assert_eq!(
            store.status_role("UNDEPLOYED_OBJECT_STORE"),
            None,
            "a store nobody has deployed is neither well nor unwell"
        );
        let task = kind("prism.config.Task").expect("the catalog has Tasks");
        assert_eq!(task.status_role("QUEUED"), Some(Role::Muted));
        assert_eq!(task.status_role("RUNNING"), None);
        assert!(
            task.status_roles.windows(2).all(|w| w[0].0 < w[1].0),
            "sorted, so the lookup can binary search"
        );
    }

    /// The `when` grammar's three forms, and the shapes that are not one. The generator refuses
    /// anything `parse_when` cannot spell and `nutsh_core::detail::holds` evaluates exactly what
    /// it returns, so this test is the whole vocabulary.
    #[test]
    fn the_when_grammar_has_three_forms_and_no_expressions() {
        assert_eq!(parse_when("guestTools"), Some(When::Present("guestTools")));
        assert_eq!(
            parse_when("  guestTools "),
            Some(When::Present("guestTools"))
        );
        assert_eq!(parse_when("!host.extId"), Some(When::Absent("host.extId")));
        assert_eq!(
            parse_when("bootConfig.$objectType = UefiBoot"),
            Some(When::Equals {
                path: "bootConfig.$objectType",
                value: "UefiBoot",
            })
        );
        assert_eq!(
            parse_when("powerState=ON"),
            Some(When::Equals {
                path: "powerState",
                value: "ON",
            })
        );
        for bad in ["", "   ", "!", "!  ", "= ON", "powerState =", "!a = b"] {
            assert_eq!(parse_when(bad), None, "{bad:?}");
        }
        // Not an expression parser, and never will be: `a && b` is read as a path, and a path
        // with spaces in it resolves against nothing, so the generator's path check refuses it.
        assert_eq!(parse_when("a && b"), Some(When::Present("a && b")));
    }

    /// The `$objectType` tag is compared on its short name, because both spellings are in the
    /// committed corpus for one shape: `disks[].backingInfo` is tagged
    /// `vmm.v4.r0.b1.ahv.config.VmDisk` in the curated `vms.json` and `vmm.v4.ahv.config.VmDisk`
    /// in the `disks.json` beside it and on 198 of the recording's 353 disks. A `when`
    /// written against the full string would be right on one Prism Central build and silently
    /// wrong on the next.
    #[test]
    fn an_object_type_path_is_recognised_and_its_value_shortened() {
        assert_eq!(
            object_type_prefix("bootConfig.$objectType"),
            Some("bootConfig")
        );
        assert_eq!(object_type_prefix("$objectType"), Some(""));
        assert_eq!(
            object_type_prefix("disks[].backingInfo.$objectType"),
            Some("disks[].backingInfo")
        );
        assert_eq!(object_type_prefix("bootConfig.bootOrder"), None);
        assert_eq!(object_type_prefix("my$objectType"), None);
        // The two spellings the committed fixtures really carry, on one shape.
        assert_eq!(short_type("vmm.v4.ahv.config.VmDisk"), "VmDisk");
        assert_eq!(short_type("vmm.v4.r0.b1.ahv.config.VmDisk"), "VmDisk");
        assert_eq!(short_type("VmDisk"), "VmDisk");
    }

    /// The label column's width is one number, shared by the generator's check and the pane's
    /// layout, for the reason `DEFAULT_COLUMNS` is: a second copy would let the two disagree
    /// silently.
    #[test]
    fn a_detail_section_is_a_titled_list_of_labelled_paths() {
        const FIELDS: &[DetailField] = &[
            DetailField {
                label: "Name",
                path: "name",
                kind: ColumnKind::Text,
                when: None,
            },
            DetailField {
                label: "Secure boot",
                path: "bootConfig.isSecureBootEnabled",
                kind: ColumnKind::Bool,
                when: Some("bootConfig.$objectType = UefiBoot"),
            },
        ];
        const SECTION: DetailSection = DetailSection {
            title: "Identity",
            fields: FIELDS,
            when: None,
        };
        assert_eq!(SECTION.fields.len(), 2);
        assert!(SECTION.fields[0].label.chars().count() <= DETAIL_LABEL);
        assert_eq!(DETAIL_LABEL, 18);
    }
}

//! Intermediate representation between the OpenAPI specs and `generated.rs`.

use nutsh_catalog::RateLimit;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceModel {
    pub name: String,
    pub version: String,
    pub preview: bool,
    pub versions: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListParamsModel {
    pub page: bool,
    pub limit: bool,
    pub filter: bool,
    pub orderby: bool,
    pub select: bool,
    pub expand: bool,
    pub required: bool,
    /// The `$filter` `x-odata-fields` allowlist, or empty when the endpoint declares none.
    pub filter_fields: Vec<String>,
    /// The `$orderby` `x-odata-fields` allowlist, or empty when the endpoint declares none.
    /// Generator-only, and only ever a **warning**: the allowlist is empty at `prism` v4.0,
    /// v4.0.a2 and v4.0.b1 and at `monitoring` v4.0 and v4.0.b1, so a hard check would refuse
    /// to emit `probe_by` for exactly the Prism Centrals whose slowness makes it worth having.
    pub orderby_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldModel {
    pub name: String,
    pub label: String,
    /// Rendered as `FieldType::…`.
    pub ty: FieldKind,
    pub required: bool,
    pub value: Option<String>,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Bool,
    Integer,
    Enum(Vec<String>),
    /// During parsing this holds the referenced schema's short name; a post-pass over every
    /// namespace turns it into a kind id, or `None` when nothing matches.
    Reference(Option<String>),
    Json(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionModel {
    pub name: String,
    pub path: String,
    /// One of `Get`, `Post`, `Put`, `Patch`, `Delete` (rendered as `Method::<value>`).
    pub method: String,
    pub needs_etag: bool,
    pub needs_body: bool,
    pub takes_body: bool,
    /// One of `Task`, `Payload`, `None` (rendered as `ActionReturn::<value>`).
    pub returns: String,
    pub scope: Vec<String>,
    pub roles: Vec<String>,
    pub key: String,
    pub label: String,
    /// One of `None`, `Low`, `Medium`, `High`.
    pub danger: String,
    /// One of `None`, `Yes`, `TypeName`.
    pub confirm: String,
    pub order: u16,
    pub hidden: bool,
    pub body: Option<String>,
    pub form: Vec<FieldModel>,
    pub rate: RateLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnModel {
    pub header: String,
    pub path: String,
    /// One of the `ColumnKind` variant names (rendered as `ColumnKind::<value>`).
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailFieldModel {
    pub label: String,
    pub path: String,
    /// One of the `ColumnKind` variant names (rendered as `ColumnKind::<value>`).
    pub kind: String,
    /// One of the three `when` forms; `nutsh_catalog::parse_when` is what says which.
    #[serde(default)]
    pub when: Option<String>,
}

/// An array-of-tables in `curated.toml` rather than an inline array, because a section holds an
/// array of its own and TOML's inline form does not survive that legibly.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailSectionModel {
    pub title: String,
    #[serde(default)]
    pub when: Option<String>,
    pub fields: Vec<DetailFieldModel>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindModel {
    pub id: String,
    pub namespace: String,
    pub version: String,
    pub since: String,
    pub display: String,
    pub aliases: Vec<String>,
    pub category: String,
    pub poll_secs: u32,
    pub list_path: String,
    pub get_path: Option<String>,
    pub schema: String,
    pub list_params: ListParamsModel,
    /// Curated: the `$orderby` every list of this kind is sent with.
    pub orderby: Option<String>,
    /// Curated: the monotone last-modified field the change-probe orders by.
    pub probe_by: Option<String>,
    /// Top-level properties the schema declares as `string`/`date-time`. Generator-only: it is
    /// what `probe_by` is checked against, and it is never rendered.
    pub timestamps: Vec<String>,
    /// The row budget of a list walk over this kind: curated, else `DEFAULT_MAX_ROWS`, which
    /// `generate` fills in after the overlay so that no kind reaches the catalog without one.
    pub max_rows: Option<u32>,
    pub parent: Option<String>,
    pub actions: Vec<ActionModel>,
    pub action_kind: Option<String>,
    pub action_parents: Vec<String>,
    pub rate: RateLimit,
    /// The top-level properties a table row is read for, comma separated: `Kind::select`.
    /// Computed after the pages are parsed, because a page pane may draw a column the kind's
    /// own list does not.
    pub select: Option<String>,
    /// Every top-level property name the kind's schema declares. Generator-only: it is what a
    /// `$select` root is checked against, and it is never rendered.
    pub properties: Vec<String>,
    pub ext_id_key: String,
    /// Curated: dotted path to the entity's display name; empty means `NAME_KEYS`.
    pub name_path: String,
    /// Curated: list one page of this kind at connect to warm the name cache.
    pub warm: bool,
    pub columns: Vec<ColumnModel>,
    /// Curated: the detail view's sections. Empty for the ~250 kinds nobody curates, whose
    /// detail `nutsh_core::detail::fallback` composes from the entity at render time.
    pub detail: Vec<DetailSectionModel>,
    pub fallback_columns: Vec<ColumnModel>,
    pub preview: bool,
    /// Set by the overlay: the kind has an entry in `curated.toml`.
    pub curated: bool,
    /// Set by the overlay: `(normalised word, role name)`, sorted by word.
    pub status_roles: Vec<(String, String)>,
}

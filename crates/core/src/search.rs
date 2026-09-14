//! What a search matches a row on: its name, its addresses and its identifier.
//!
//! Three fields and no more, so the rule can be stated in one sentence and a row that did not
//! match is never a mystery. A status word, a cluster reference and a memory size are not
//! search material: nobody hunts for a VM by its socket count, and a term that quietly matched
//! half the table on `ON` would make the count in the title useless.
//!
//! Case-insensitive substring is the baseline. An **address** is the exception: a term made of
//! digits and dots is read octet by octet, so a complete octet has to match a whole octet.
//! `192.0.2` is then 192.0.2.x and 192.0.21.x, while `92.0.2` and `2.0` - which a substring
//! finds inside `192.0.2.21`, one across the first octet and one in the middle - find nothing.
//! That is the difference between a search over addresses and a search over the text an
//! address happens to be written in.

use std::collections::{BTreeMap, BTreeSet};

use nutsh_catalog::{ColumnKind, Kind, Reach};
use nutsh_prism::Entity;
use serde_json::Value;

/// A term, parsed once and matched against many rows.
///
/// Both surfaces build one: `/` from what is typed into it, `:search` from the rest of the
/// palette's line. The parse is the whole cost - the match itself allocates one lowercased
/// copy per field it reads - so a query is built per keystroke and matched per row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// The term as typed, which is what the title and the footer show.
    text: String,
    /// The same, lowercased, which is what a substring match reads.
    needle: String,
    /// The octets the term names, when it reads as part of an address.
    dotted: Option<Dotted>,
}

impl Query {
    pub fn new(term: &str) -> Query {
        Query {
            text: term.to_string(),
            needle: term.to_lowercase(),
            dotted: Dotted::parse(term),
        }
    }

    /// The term as typed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Nothing typed: every row matches, which is what keeps an empty `/` from emptying a table
    /// between the keystroke that opens it and the first letter.
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// Whether the term reads as part of a dotted address.
    pub fn is_address(&self) -> bool {
        self.dotted.is_some()
    }

    /// Whether this row is one the term names.
    pub fn matches(&self, entity: &Entity) -> bool {
        self.why(entity).is_some()
    }

    /// Which of the three fields the term was found in, or `None` for a row it does not name.
    ///
    /// One walk answers both questions a caller has. `/` wants the yes or no; `:search` wants
    /// the yes **and** what to put beside the name in the result row, and computing that twice
    /// is how the list ends up showing a row a second rule would not have matched.
    pub fn why(&self, entity: &Entity) -> Option<Match> {
        if self.is_empty() {
            return Some(Match::Name);
        }
        if contains_ci(&entity.name, &self.needle) {
            return Some(Match::Name);
        }
        if contains_ci(&entity.ext_id, &self.needle) {
            return Some(Match::ExtId);
        }
        self.values(entity)
            .find(|(_, value)| self.matches_address(value))
            .map(|(label, value)| Match::Address { label, value })
    }

    /// Every address this row carries, under the name the curator gave its field, in the order
    /// the kind declares them.
    ///
    /// The label travels with the value because a Host has four addresses - its CVM, its
    /// hypervisor, its backplane and its IPMI - and a result row that showed one of them
    /// without saying which would leave the reader to guess at the very thing they searched by.
    fn values<'a>(&self, entity: &'a Entity) -> impl Iterator<Item = (&'static str, String)> + 'a {
        addresses(entity.kind).flat_map(|(label, path)| {
            nutsh_catalog::path::get(&entity.raw, path)
                .into_iter()
                .filter_map(Value::as_str)
                .map(|value| (label, value.to_string()))
                .collect::<Vec<_>>()
        })
    }

    /// One address. A dotted term reads it octet by octet and **only** that way: falling back
    /// to a substring here is what would let `2.0` match `192.0.2.21` again, which is the
    /// whole reason the octets are parsed.
    fn matches_address(&self, value: &str) -> bool {
        match &self.dotted {
            Some(dotted) => dotted.matches(value),
            None => contains_ci(value, &self.needle),
        }
    }
}

/// Which field the term was found in, so a result row can say why it is on the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    Name,
    ExtId,
    /// The address the term matched, as the row holds it, under the curator's name for the
    /// field it came out of: `CVM IP`, `IPMI IP`, `IP`.
    Address {
        label: &'static str,
        value: String,
    },
}

impl Match {
    /// `IPMI IP 192.0.2.21`: an address with the field it came from in front of it, which is
    /// what a result row shows beside the name.
    fn labelled(label: &str, value: &str) -> String {
        format!("{label} {value}")
    }
}

/// Every path a kind keeps an address at: the table's columns, the fallback columns a kind with
/// none curated shows, and the composed detail's fields.
///
/// The detail is in here because that is where some kinds keep the address a person searches
/// by: a Host's IPMI address is a detail field and not a column, and an administrator holding a
/// DRAC address does not care which of the two the curator chose. Static data, walked per row;
/// a kind has a handful of columns and a couple of dozen detail fields.
pub fn addresses(kind: &'static Kind) -> impl Iterator<Item = (&'static str, &'static str)> {
    let columns = kind
        .columns
        .iter()
        .chain(kind.fallback_columns)
        .filter(|c| c.kind == ColumnKind::Ip)
        .map(|c| (c.header, c.path));
    let detail = kind
        .detail
        .iter()
        .flat_map(|section| section.fields)
        .filter(|f| f.kind == ColumnKind::Ip)
        .map(|f| (f.label, f.path));
    columns.chain(detail)
}

/// The OData `$filter` this term can be sent to a Prism Central as, for this kind.
///
/// `None` means the term has to be matched here instead. Three reasons, and the caller says
/// which in the title: the endpoint takes no `$filter`, it declares no field the term could
/// match, or the term is an address. An address is always `None`: five kinds in the catalog
/// declare an address filterable and not one of them is a kind anybody searches by address.
///
/// `startswith` and `eq` only. The specs' own examples use those two, Nutanix's OData is a
/// subset, and a `contains` that four namespaces reject is worse than a filter never sent.
pub fn odata(kind: &'static Kind, query: &Query) -> Option<String> {
    if !kind.list_params.filter || query.is_empty() || query.is_address() {
        return None;
    }
    let fields = kind.list_params.filter_fields;
    let term = query.text();
    if fields.contains(&"name") {
        return Some(format!("startswith(name,'{}')", quote(term)));
    }
    // Whole identifier only. `extId` is a guid to the server, and a prefix of one is not.
    if fields.contains(&"extId") && is_uuid(term) {
        return Some(format!("extId eq '{}'", quote(term)));
    }
    None
}

/// One kind a search will ask a Prism Central about, and what it will send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub kind: &'static Kind,
    /// The `$filter` to send, or `None` to walk the list and match the rows here.
    pub filter: Option<String>,
}

/// Every kind a term is worth asking about, and how.
///
/// Only kinds the palette can open on their own: a child needs a parent's identifier, and a
/// search cannot supply one without first listing every parent, which is a different and much
/// larger question than the one that was asked.
///
/// Two shapes, and the term decides which:
///
/// - a **name or identifier** goes as a `$filter`, so the Prism Central answers with the rows
///   that match and nothing else. Only the kinds that declare the field filterable are asked;
///   the rest would answer 400.
/// - an **address** cannot be filtered anywhere worth asking, so the kinds that keep an address
///   at all are listed in full and matched here. There are nine of them, which is why the
///   narrower-looking search is the cheaper one.
pub fn plan(query: &Query) -> Vec<Ask> {
    if query.is_empty() {
        return Vec::new();
    }
    nutsh_catalog::KINDS
        .iter()
        .filter(|k| nutsh_catalog::reach(k) == Reach::Direct)
        .filter_map(|k| {
            if query.is_address() {
                // `addresses` reads the columns and the composed detail, which is where a Host
                // keeps its IPMI address, so the set is wider than the table shows.
                addresses(k).next()?;
                return Some(Ask {
                    kind: k,
                    filter: None,
                });
            }
            Some(Ask {
                kind: k,
                filter: Some(odata(k, query)?),
            })
        })
        .collect()
}

/// OData escapes a single quote by doubling it. Without this a name holding one is a filter the
/// Prism Central answers 400 to, and the term is under the user's hand.
fn quote(term: &str) -> String {
    term.replace('\'', "''")
}

fn is_uuid(term: &str) -> bool {
    let groups = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = term.split('-').collect();
    parts.len() == groups.len()
        && parts
            .iter()
            .zip(groups)
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `needle` is already lowercased; the haystack is lowercased per call, which is what makes the
/// match work for a name that is not ASCII.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(needle)
}

/// A term read as part of a dotted address: the complete octets it names, and the incomplete
/// one it may end on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dotted {
    /// Octets that have to match a candidate's octet exactly.
    exact: Vec<String>,
    /// A trailing octet still being typed, which matches as a prefix. `None` when the term ends
    /// on a dot, which is the user saying the octet before it is finished.
    partial: Option<String>,
}

impl Dotted {
    /// The octets of a term that reads as part of an address: digits and dots, at least one of
    /// each, and no empty octet in the middle.
    ///
    /// `.21` and `10..5` are deliberately **not** addresses. They have an empty octet where a
    /// number should be, so there is no run of octets to look for, and a plain substring - which
    /// does find `.21` at the end of `192.0.2.21` - is the better answer for them.
    fn parse(term: &str) -> Option<Dotted> {
        if !term.contains('.') || !term.bytes().any(|b| b.is_ascii_digit()) {
            return None;
        }
        if !term.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        let mut parts: Vec<&str> = term.split('.').collect();
        // A trailing dot leaves an empty last part: every octet the term names is complete.
        let partial = match parts.last() {
            Some(&"") => {
                parts.pop();
                None
            }
            _ => parts.pop().map(str::to_string),
        };
        if parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        Some(Dotted {
            exact: parts.into_iter().map(str::to_string).collect(),
            partial,
        })
    }

    /// Whether the term's octets line up with a run of this address's octets, starting at any
    /// octet boundary. Every complete octet matches whole; the trailing incomplete one matches
    /// as a prefix, which is what makes the filter narrow as the address is typed.
    ///
    /// The candidate is split and never parsed, so a prefix length rides along with the last
    /// octet: `192.0.2` finds the subnet `192.0.2.0/24` because `0/24` starts with `0`.
    fn matches(&self, address: &str) -> bool {
        let candidate: Vec<&str> = address.split('.').collect();
        let wanted = self.exact.len() + usize::from(self.partial.is_some());
        if wanted == 0 || wanted > candidate.len() {
            return false;
        }
        (0..=candidate.len() - wanted).any(|start| {
            self.exact
                .iter()
                .enumerate()
                .all(|(i, octet)| candidate[start + i] == octet)
                && self
                    .partial
                    .as_ref()
                    .is_none_or(|p| candidate[start + self.exact.len()].starts_with(p.as_str()))
        })
    }
}

/// One row a term found, with what it takes to identify it and to go and open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: &'static Kind,
    pub ext_id: String,
    pub name: String,
    /// What to show beside the name: the address the term matched, the identifier when that is
    /// what matched, or - when the name matched - the first address the row carries, which is
    /// the next most identifying thing it has. Empty when there is none.
    pub detail: String,
}

/// The hits of one kind, in the order that kind's table holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub kind: &'static Kind,
    pub hits: Vec<Hit>,
}

/// What a search of the store found, **and how far it could see**.
///
/// The two travel together on purpose. A list of results says what was found; only `loaded` and
/// `not_loaded` say what was looked at, and a search that covered fourteen kinds of two hundred
/// and sixty-two while looking complete is the kind of half-truth a screen full of rows tells
/// very convincingly. Whoever draws a `Found` draws both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub groups: Vec<Group>,
    /// Kinds the store holds a table for, whether or not that table has rows: a kind that was
    /// listed and came back empty *was* searched.
    pub loaded: usize,
    /// Kinds in the catalog the store holds nothing for. Not fetched, and not counted as a
    /// failure - it is the reach, stated.
    pub not_loaded: usize,
}

impl Found {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// How many rows were found, across every kind.
    pub fn hits(&self) -> usize {
        self.groups.iter().map(|g| g.hits.len()).sum()
    }
}

/// Every row in the store that `query` names, grouped by kind, with the reach beside it.
///
/// **It asks for nothing.** The store is what this session has polled plus whatever the on-disk
/// cache painted into it before the first frame, and that is the whole corpus: a search that
/// went and listed two hundred and forty-eight kinds to answer one term would be a minute of
/// waiting and a thousand requests for a question the user expected to be instant.
///
/// A kind may hold several tables - a child table under a parent row, a page pane's filtered
/// one - and the same entity can be in more than one of them, so hits are deduplicated by
/// identifier within a kind. The groups come out in the order the menu lists their kinds, which
/// is the app's own curated order of importance: Virtual Machines before Hosts before Subnets,
/// as the sidebar has them, with anything the menu does not name after them by display name.
pub fn across(store: &crate::store::Store, query: &Query) -> Found {
    let mut by_kind: BTreeMap<&'static str, Group> = BTreeMap::new();
    let mut loaded: BTreeSet<&'static str> = BTreeSet::new();
    for (key, table) in store.tables() {
        loaded.insert(key.kind.id);
        for entity in table.rows.values() {
            let Some(why) = query.why(entity) else {
                continue;
            };
            let group = by_kind.entry(entity.kind.id).or_insert_with(|| Group {
                kind: entity.kind,
                hits: Vec::new(),
            });
            if group.hits.iter().any(|h| h.ext_id == entity.ext_id) {
                continue;
            }
            group.hits.push(Hit {
                kind: entity.kind,
                ext_id: entity.ext_id.clone(),
                name: entity.name.clone(),
                detail: match why {
                    Match::Address { label, value } => Match::labelled(label, &value),
                    Match::ExtId => entity.ext_id.clone(),
                    Match::Name => query
                        .values(entity)
                        .next()
                        .map(|(label, value)| Match::labelled(label, &value))
                        .unwrap_or_default(),
                },
            });
        }
    }
    let mut groups: Vec<Group> = by_kind.into_values().collect();
    groups.sort_by_key(|g| (menu_position(g.kind), g.kind.display));
    Found {
        groups,
        loaded: loaded.len(),
        not_loaded: nutsh_catalog::KINDS.len().saturating_sub(loaded.len()),
    }
}

/// Where the menu lists this kind, or past the end of the menu for one it does not name.
///
/// The menu's order is curated - it is what the sidebar draws - so reading it here costs one
/// walk of two hundred-odd static items per search and saves inventing a second opinion about
/// which kind matters most.
fn menu_position(kind: &'static Kind) -> usize {
    let mut at = 0;
    for group in nutsh_catalog::NAV {
        for item in group.items {
            if item.target == nutsh_catalog::NavTarget::Kind(kind.id) {
                return at;
            }
            at += 1;
        }
    }
    usize::MAX
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The VM kind: a curated `IP` column over `nics[]`, which is the shape every address
    /// assertion here is made through.
    fn vm() -> &'static Kind {
        nutsh_catalog::kind("vmm.ahv.config.Vm").expect("the catalog has VMs")
    }

    fn entity(name: &str, ip: &str) -> Entity {
        Entity::new(
            vm(),
            json!({
                "extId": format!("id-{name}"),
                "name": name,
                "nics": [{"nicNetworkInfo": {"ipv4Config": {"ipAddress": {"value": ip}}}}],
            }),
            None,
        )
    }

    /// The three fields, and the case-insensitivity that makes a term typed in a hurry work.
    #[test]
    fn a_term_matches_a_name_an_address_or_an_identifier() {
        let e = entity("web-01", "203.0.113.11");
        assert!(Query::new("web").matches(&e), "the name");
        assert!(Query::new("WEB-0").matches(&e), "case-insensitively");
        assert!(Query::new("id-web").matches(&e), "the identifier");
        assert!(Query::new("203.0.113.11").matches(&e), "the address");
        assert!(!Query::new("db").matches(&e));
        // A memory size or a power state is not search material, and neither is the raw JSON
        // the two of them live in.
        assert!(!Query::new("nicNetworkInfo").matches(&e));
    }

    /// An empty term is not a filter: every row matches, so the frame between `/` and the first
    /// letter is the table as it was.
    #[test]
    fn an_empty_term_matches_everything() {
        let q = Query::new("");
        assert!(q.is_empty());
        assert!(q.matches(&entity("web-01", "203.0.113.11")));
        assert_eq!(q.text(), "");
    }

    /// The octet rule, stated against the case a substring gets wrong.
    #[test]
    fn a_complete_octet_matches_a_whole_octet() {
        let alpha = entity("alpha", "192.0.2.21");
        let beta = entity("beta", "110.1.2.3");
        let gamma = entity("gamma", "10.1.2.3");
        assert!(Query::new("10.1").matches(&gamma));
        assert!(!Query::new("10.1").matches(&beta), "110 is not 10");
        assert!(Query::new("192.0").matches(&alpha));
        assert!(!Query::new("92.0").matches(&alpha), "192 is not 92");
        // A run found anywhere in the address counts.
        assert!(Query::new("2.21").matches(&alpha));
        assert!(Query::new("0.2").matches(&alpha));
        // A partial octet in the middle of one does not, which is where a substring says yes.
        assert!(!Query::new("2.0").matches(&alpha));
        assert!(!Query::new("2.0.2").matches(&alpha));
        // A trailing dot says the octet before it is finished.
        assert!(Query::new("192.0.").matches(&alpha));
        assert!(!Query::new("19.0.").matches(&alpha));
        // And a term longer than the address matches nothing.
        assert!(!Query::new("192.0.2.21.9").matches(&alpha));
    }

    /// The subnet case: the address is split and never parsed, so the prefix length rides along
    /// with the last octet instead of making the row unmatchable.
    #[test]
    fn a_prefix_length_rides_along_with_the_last_octet() {
        let dotted = Dotted::parse("192.0.2").expect("an address");
        assert!(dotted.matches("192.0.2.0/24"));
        assert!(
            Dotted::parse("192.0.2.0")
                .expect("an address")
                .matches("192.0.2.0/24")
        );
    }

    /// What is an address term and what is only text. The three that are not fall through to a
    /// substring, which is the better answer for each of them.
    #[test]
    fn only_digits_and_dots_are_read_as_octets() {
        assert!(Dotted::parse("10.12").is_some());
        assert!(Dotted::parse("10.").is_some());
        assert!(Dotted::parse("10").is_none(), "no dot, no octets");
        assert!(Dotted::parse("v1.2").is_none(), "a letter is not an octet");
        assert!(Dotted::parse("...").is_none(), "no digit anywhere");
        assert!(Dotted::parse(".21").is_none(), "an empty leading octet");
        assert!(
            Dotted::parse("10..5").is_none(),
            "an empty octet in the middle"
        );
        // And what falls through still matches as text: `.21` is at the end of the address.
        assert!(Query::new(".21").matches(&entity("alpha", "192.0.2.21")));
    }

    /// One walk answers both questions: whether the row matched, and what to show beside its
    /// name for having matched.
    #[test]
    fn a_name_asks_the_kinds_that_will_filter_on_one() {
        let plan = plan(&Query::new("web"));
        assert!(
            plan.iter().all(|a| a.filter.is_some()),
            "a name goes as a filter or the kind is not asked"
        );
        assert!(
            plan.iter()
                .all(|a| nutsh_catalog::reach(a.kind) == Reach::Direct),
            "a child needs a parent id a search has no way to supply"
        );
        let vm = plan
            .iter()
            .find(|a| a.kind.id == "vmm.ahv.config.Vm")
            .expect("VMs filter on name");
        assert_eq!(vm.filter.as_deref(), Some("startswith(name,'web')"));
        // Every kind asked really does declare the field, which is what keeps a 400 off the wire.
        assert!(
            plan.iter()
                .all(|a| a.kind.list_params.filter_fields.contains(&"name")
                    || a.kind.list_params.filter_fields.contains(&"extId"))
        );
    }

    #[test]
    fn an_address_asks_only_the_kinds_that_keep_one_and_sends_no_filter() {
        let plan = plan(&Query::new("10.12.54"));
        assert!(
            plan.iter().all(|a| a.filter.is_none()),
            "no kind worth asking will filter on an address"
        );
        let ids: Vec<&str> = plan.iter().map(|a| a.kind.id).collect();
        assert!(ids.contains(&"vmm.ahv.config.Vm"), "{ids:?}");
        assert!(ids.contains(&"clustermgmt.config.Host"), "{ids:?}");
        assert!(ids.contains(&"networking.config.Subnet"), "{ids:?}");
        // The whole point of the address case: it is the narrow one.
        assert!(
            plan.len() < 15,
            "an address search is a handful of kinds, not the catalog: {ids:?}"
        );
    }

    #[test]
    fn nothing_typed_asks_nothing() {
        assert!(plan(&Query::new("")).is_empty());
    }

    #[test]
    fn the_filter_a_term_can_be_sent_as() {
        let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
        assert_eq!(
            odata(vm, &Query::new("web")),
            Some("startswith(name,'web')".into())
        );
        // An address names no field any of these endpoints will filter on.
        assert_eq!(odata(vm, &Query::new("10.12.54")), None);
        // Nothing typed is every row, which is a list and not a filter.
        assert_eq!(odata(vm, &Query::new("")), None);
    }

    #[test]
    fn a_quote_in_the_term_is_escaped_not_sent() {
        let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").expect("VMs");
        assert_eq!(
            odata(vm, &Query::new("o'brien")),
            Some("startswith(name,'o''brien')".into())
        );
    }

    #[test]
    fn a_whole_identifier_goes_as_ext_id_and_a_prefix_does_not() {
        let task = nutsh_catalog::kind("prism.config.Task").expect("Tasks");
        assert!(!task.list_params.filter_fields.contains(&"name"));
        let uuid = "22262664-c65f-f854-65af-7cc25581f458";
        assert_eq!(
            odata(task, &Query::new(uuid)),
            Some(format!("extId eq '{uuid}'"))
        );
        assert_eq!(odata(task, &Query::new("22262664-c65f")), None);
    }

    #[test]
    fn a_kind_that_takes_no_filter_is_never_sent_one() {
        let k = nutsh_catalog::KINDS
            .iter()
            .find(|k| !k.list_params.filter)
            .expect("a kind that takes no $filter");
        assert_eq!(odata(k, &Query::new("web")), None);
    }

    #[test]
    fn why_a_row_matched_is_what_the_result_row_shows() {
        let e = entity("web-01", "203.0.113.11");
        assert_eq!(Query::new("web").why(&e), Some(Match::Name));
        assert_eq!(Query::new("id-web").why(&e), Some(Match::ExtId));
        assert_eq!(
            Query::new("203.0").why(&e),
            Some(Match::Address {
                label: "IP",
                value: "203.0.113.11".into()
            })
        );
        assert_eq!(Query::new("nope").why(&e), None);
        assert!(!Query::new("nope").matches(&e));
    }

    /// The store is the corpus, and the reach is stated beside the results: two kinds looked at
    /// of the catalog's two hundred and sixty-two, said in the numbers and not in a hedge.
    #[test]
    fn a_search_of_the_store_is_grouped_by_kind_and_states_its_reach() {
        use crate::store::{Store, TableKey, Update};
        let mut store = Store::default();
        let host = nutsh_catalog::kind("clustermgmt.config.Host").expect("hosts");
        let mut put = |key: TableKey, rows: Vec<Entity>| {
            store.apply(
                &key,
                Update::Page {
                    generation: 1,
                    entities: rows,
                    total: None,
                },
            );
            store.apply(&key, Update::Complete { generation: 1 });
        };
        put(
            TableKey::top(vm()),
            vec![
                entity("web-01", "203.0.113.11"),
                entity("db-01", "10.1.2.3"),
            ],
        );
        put(
            TableKey::top(host),
            vec![Entity::new(
                host,
                json!({
                    "extId": "h1",
                    "hostName": "ahv-node-1",
                    "hypervisor": {"externalAddress": {"ipv4": {"value": "203.0.113.101"}}},
                }),
                None,
            )],
        );
        let found = across(&store, &Query::new("203.0.113"));
        assert_eq!(found.hits(), 2);
        assert_eq!(found.loaded, 2);
        assert_eq!(found.not_loaded, nutsh_catalog::KINDS.len() - 2);
        // The menu's order: Virtual Machines are above Hosts in the sidebar, so they are here.
        let kinds: Vec<&str> = found.groups.iter().map(|g| g.kind.display).collect();
        assert_eq!(kinds, ["Virtual Machines", "Hosts"]);
        assert_eq!(found.groups[0].hits[0].name, "web-01");
        // The curator's name for the field the address came out of, so a Host with four of
        // them says which one answered.
        assert_eq!(found.groups[0].hits[0].detail, "IP 203.0.113.11");
        assert_eq!(
            found.groups[1].hits[0].detail,
            "HYPERVISOR IP 203.0.113.101"
        );

        // A term nothing answers still reports how far it looked.
        let none = across(&store, &Query::new("no-such-thing"));
        assert!(none.is_empty() && none.hits() == 0);
        assert_eq!((none.loaded, none.not_loaded), (2, found.not_loaded));
    }

    /// The same entity can be in a child table and in its kind's own table; it is one row in
    /// the results, not two.
    #[test]
    fn a_row_in_two_tables_is_found_once() {
        use crate::store::{Store, TableKey, Update};
        let mut store = Store::default();
        for key in [
            TableKey::top(vm()),
            TableKey::under(vm(), vec!["parent-1".into()]),
        ] {
            store.apply(
                &key,
                Update::Page {
                    generation: 1,
                    entities: vec![entity("web-01", "203.0.113.11")],
                    total: None,
                },
            );
            store.apply(&key, Update::Complete { generation: 1 });
        }
        let found = across(&store, &Query::new("web-01"));
        assert_eq!(found.hits(), 1);
        assert_eq!(found.loaded, 1, "two tables, one kind");
    }

    /// A kind keeps addresses in its columns and in its composed detail, and a person searching
    /// by one does not know which. Both are read.
    #[test]
    fn addresses_come_from_the_columns_and_from_the_detail() {
        let host = nutsh_catalog::kind("clustermgmt.config.Host").expect("the catalog has hosts");
        let paths: Vec<(&str, &str)> = addresses(host).collect();
        assert!(
            paths.contains(&("CVM IP", "controllerVm.externalAddress.ipv4.value")),
            "a column, under its heading: {paths:?}"
        );
        assert!(
            paths.contains(&("IPMI IP", "ipmi.ip.ipv4.value")),
            "a detail field, under its label: {paths:?}"
        );
        let ipmi = Entity::new(
            host,
            json!({"extId": "h1", "hostName": "ntnx-a-1", "ipmi": {"ip": {"ipv4": {"value": "192.0.2.21"}}}}),
            None,
        );
        assert!(Query::new("192.0.2").matches(&ipmi));
    }
}

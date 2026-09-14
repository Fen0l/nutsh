//! A dynamic entity: the raw JSON plus what every view needs without knowing the kind.

use nutsh_catalog::{Kind, NAME_KEYS};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub kind: &'static Kind,
    pub ext_id: String,
    pub name: String,
    /// From the HTTP `ETag` header of a single-entity GET; never from the body.
    pub etag: Option<String>,
    pub raw: Value,
}

impl Entity {
    pub fn new(kind: &'static Kind, raw: Value, etag: Option<String>) -> Entity {
        // Never `raw["extId"]`: storage containers identify themselves with `containerExtId`
        // and carry no `extId`, so the key comes from the catalog.
        let ext_id = raw
            .get(kind.ext_id_key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // The curated path first, then `NAME_KEYS`, then the extId: the existing order with
        // one step in front of it. One field fixes the name everywhere at once - the cache,
        // the detail title, the breadcrumb, the sidebar's `[n]`, the confirm dialog and the
        // journal all read `Entity::name`.
        let named = if kind.name_path.is_empty() {
            None
        } else {
            nutsh_catalog::path::first(&raw, kind.name_path).and_then(Value::as_str)
        };
        let name = named
            .or_else(|| {
                NAME_KEYS
                    .iter()
                    .find_map(|k| raw.get(*k).and_then(Value::as_str))
            })
            .map(str::to_string)
            .unwrap_or_else(|| ext_id.clone());
        Entity {
            kind,
            ext_id,
            name,
            etag,
            raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nutsh_catalog::{ColumnKind, ListParams, RateLimit};
    use serde_json::json;

    fn any_kind() -> &'static Kind {
        &nutsh_catalog::KINDS[0]
    }

    /// A kind that identifies its entities the way a storage container does: this pins the
    /// mechanism rather than any one curated kind.
    static CONTAINER_LIKE: Kind = Kind {
        id: "test.config.Container",
        namespace: "test",
        version: "v4.3",
        since: "v4.0",
        display: "Containers",
        aliases: &[],
        category: "Storage",
        poll_secs: 30,
        list_path: "/test/v4.3/config/containers",
        get_path: Some("/test/v4.3/config/containers/{extId}"),
        schema: "test.v4.3.config.Container",
        select: None,
        list_params: ListParams {
            page: true,
            limit: true,
            filter: false,
            orderby: false,
            select: false,
            expand: false,
            required: false,
            filter_fields: &[],
        },
        orderby: None,
        probe_by: None,
        max_rows: None,
        parent: None,
        actions: &[],
        action_kind: None,
        action_parents: &[],
        rate: RateLimit::DEFAULT,
        ext_id_key: "containerExtId",
        name_path: "",
        warm: false,
        columns: &[],
        detail: &[],
        fallback_columns: &[nutsh_catalog::Column {
            header: "NAME",
            path: "name",
            kind: ColumnKind::Text,
        }],
        preview: false,
        curated: true,
        status_roles: &[],
    };

    /// A kind whose name is not at the top level of its JSON. `prism.config.DomainManager` is
    /// the real one: its own properties are `config`, `network`, `isRegisteredWithHostingCluster`,
    /// `hostingClusterExtId`, `shouldEnableHighAvailability`, `nodeExtIds` and `createdTime`,
    /// and the name is at `config.name`.
    static NESTED_NAME: Kind = Kind {
        id: "test.config.Manager",
        namespace: "test",
        version: "v4.4",
        since: "v4.0",
        display: "Managers",
        aliases: &[],
        category: "Test",
        poll_secs: 30,
        list_path: "/test/v4.4/config/managers",
        get_path: Some("/test/v4.4/config/managers/{extId}"),
        schema: "test.v4.4.config.Manager",
        select: None,
        list_params: ListParams {
            page: false,
            limit: false,
            filter: false,
            orderby: false,
            select: true,
            expand: false,
            required: false,
            filter_fields: &[],
        },
        orderby: None,
        probe_by: None,
        max_rows: None,
        parent: None,
        actions: &[],
        action_kind: None,
        action_parents: &[],
        rate: RateLimit::DEFAULT,
        ext_id_key: "extId",
        name_path: "config.name",
        warm: true,
        columns: &[],
        detail: &[],
        fallback_columns: &[nutsh_catalog::Column {
            header: "NAME",
            path: "config.name",
            kind: ColumnKind::Text,
        }],
        preview: false,
        curated: true,
        status_roles: &[],
    };

    #[test]
    fn name_prefers_known_keys_then_ext_id() {
        let e = Entity::new(
            any_kind(),
            json!({"extId": "x1", "templateName": "gold"}),
            None,
        );
        assert_eq!((e.ext_id.as_str(), e.name.as_str()), ("x1", "gold"));
        let e = Entity::new(any_kind(), json!({"extId": "x2"}), Some("\"abc\"".into()));
        assert_eq!(e.name, "x2");
        assert_eq!(e.etag.as_deref(), Some("\"abc\""));
    }

    #[test]
    fn ext_id_comes_from_the_kind_s_id_key() {
        let e = Entity::new(
            &CONTAINER_LIKE,
            json!({"containerExtId": "sc-1", "name": "default"}),
            None,
        );
        assert_eq!(e.ext_id, "sc-1");
        assert_eq!(e.name, "default");
        // And the ordinary key is still read for every other kind.
        let e = Entity::new(any_kind(), json!({"extId": "x1"}), None);
        assert_eq!(e.ext_id, "x1");
    }

    /// The curated path wins over `NAME_KEYS`, `NAME_KEYS` still wins over the extId, and a
    /// `name_path` that resolves to nothing falls through rather than blanking the name.
    #[test]
    fn name_path_is_tried_before_the_name_keys() {
        let e = Entity::new(
            &NESTED_NAME,
            json!({"extId": "pc-1", "config": {"name": "pc-lab"}}),
            None,
        );
        assert_eq!((e.ext_id.as_str(), e.name.as_str()), ("pc-1", "pc-lab"));

        // The path wins even when a top-level NAME_KEYS property is also present: the curator
        // said where the name is.
        let e = Entity::new(
            &NESTED_NAME,
            json!({"extId": "pc-1", "name": "wrong", "config": {"name": "pc-lab"}}),
            None,
        );
        assert_eq!(e.name, "pc-lab");

        // Nothing at the path: NAME_KEYS, then the extId, exactly as before.
        let e = Entity::new(
            &NESTED_NAME,
            json!({"extId": "pc-1", "name": "fallback"}),
            None,
        );
        assert_eq!(e.name, "fallback");
        let e = Entity::new(&NESTED_NAME, json!({"extId": "pc-1"}), None);
        assert_eq!(e.name, "pc-1");

        // A kind with no `name_path` is unchanged.
        let e = Entity::new(
            &CONTAINER_LIKE,
            json!({"containerExtId": "sc-1", "name": "default"}),
            None,
        );
        assert_eq!(e.name, "default");
    }
}

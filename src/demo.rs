//! `nutsh demo`: the ordinary TUI on a Prism Central this binary brought with it.
//!
//! One mock per [`SITES`] row boots from the embedded fixture estate on a loopback port. The
//! session then goes through the same door every other run does - [`crate::tui::run_on`] and
//! `CliContexts` - reading a context file this module wrote into a private temporary directory
//! and a password it put in memory, so the Contexts screen, `:settings`, the guardrail file and
//! the skin all work, and none of the user's own files is read or written.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;
use nutsh_config::{Config, Context, MemoryStore, RuleSpec, SecretStore as _, Secrets, secret_key};
use nutsh_mockpc::{MockPc, MockPcBuilder, Store};

use crate::ConnArgs;
use crate::tui::{self, TuiArgs};

/// What `nutsh demo` was asked for: the TUI's own arguments, minus the connection ones, which
/// the demo answers for itself.
pub(crate) struct DemoArgs {
    /// A page id or a kind id, resolved by `resolve_home` before any mock boots.
    pub(crate) start: String,
    pub(crate) snapshot: bool,
    pub(crate) size: (u16, u16),
    pub(crate) format: Option<nutsh_core::export::Format>,
    pub(crate) readonly: bool,
    /// `--join`: the other Prism Central is read beside the session from the first frame.
    pub(crate) join: bool,
    /// `--site`: which context the session opens on; `demo` when absent. clap has already
    /// refused anything but `demo`, `demo-dr`, `demo-edge`.
    pub(crate) site: Option<String>,
}

impl DemoArgs {
    /// The context the session opens on: `--site`, or the first built-in one.
    fn session(&self) -> &str {
        self.site.as_deref().unwrap_or(SITES[0].name)
    }
}

/// One built-in Prism Central: the context it is reached as, the embedded files it serves, the
/// account whose password is [`PASSWORD`], and the knobs that make it the Prism Central the
/// story needs (`snapshot` says whether the frame is a one-shot, for knobs that only make
/// sense interactively).
struct Site {
    name: &'static str,
    table: &'static [(&'static str, &'static str)],
    username: &'static str,
    /// Namespaces the knobs answer for themselves. `namespaces_without_fixtures` leaves these
    /// alone: the mock tests `unavailable_namespace` before `forbid_namespace`, so a namespace
    /// in both lists would answer 404 where the story wants the 403.
    explicit: &'static [&'static str],
    knobs: fn(MockPcBuilder, bool) -> MockPcBuilder,
}

/// Every account on every site has this password. It is not a secret - `docs/reference.md`
/// prints it - but it goes through the same store and the same key as a real one, so the demo
/// exercises the path a real session takes and the log test can scan for it.
const PASSWORD: &str = "secret";

const SITES: &[Site] = &[
    Site {
        name: "demo",
        table: nutsh_mockpc::demo::DEMO,
        username: "admin",
        explicit: &[],
        knobs: harbor_knobs,
    },
    Site {
        name: "demo-dr",
        table: nutsh_mockpc::demo::DEMO_DR,
        username: "operator",
        explicit: &["licensing"],
        knobs: ridge_knobs,
    },
];

/// The harbor site under a cluster pin: the same mock, reached as a second context, so the
/// header's `Cluster:` line, the CLUSTER column, the scoped counters and a context with
/// guardrails of its own are all one row away.
struct Edge {
    /// The context name.
    name: &'static str,
    /// The site it is a pin on.
    on: &'static str,
    cluster: &'static str,
}

const EDGE: Edge = Edge {
    name: "demo-edge",
    on: "demo",
    cluster: "harbor-edge",
};

/// The harbor Prism Central: two lists long enough to page past the scheduler's fifty, made
/// by repeating the fixture's own rows (repeated event titles are what a log looks like; a
/// named kind is never repeated). Everything else about it is the estate itself.
fn harbor_knobs(b: MockPcBuilder, _snapshot: bool) -> MockPcBuilder {
    b.repeat_fixture("/monitoring/v4.3/serviceability/events", 120)
        .repeat_fixture("/monitoring/v4.3/serviceability/audits", 80)
}

/// The ridge Prism Central: served, but not entirely and not to this account. Three greyed
/// states in one place - a namespace this account may not read (403), a namespace pinned at a
/// version two kinds postdate (`:try` then meets the server's own 404), and the namespaces
/// with no fixtures, which `namespaces_without_fixtures` 404s - plus a pacing tier below the
/// client's ceiling so the meter has something to show. The tier is interactive only: a
/// `--snapshot` run negotiates twice under `--join` and pays for every request it is paced on.
fn ridge_knobs(b: MockPcBuilder, snapshot: bool) -> MockPcBuilder {
    let b = b
        .forbid_namespace("licensing")
        .serve_versions("networking", &["v4.0"])
        // Keyed by the path the client sends at its pin, before the mock maps the version:
        // `crates/mockpc/tests/serve.rs` `a_missing_path_is_keyed_by_the_clients_path_under_a_version_pin`.
        .missing_path("/networking/v4.0/config/nic-profiles")
        .missing_path("/networking/v4.0/config/network-functions");
    if snapshot { b } else { b.rate_limit(10, 1) }
}

/// What `--join` joins: the other Prism Central. `App::start_join` refuses the session's own
/// name, so a session on `demo-dr` joins `demo`; `demo-edge` is the harbor mock under a
/// cluster pin, and joining the same mock twice would list every harbor row twice.
fn peer_of(site: &str) -> &'static str {
    if site == "demo-dr" { "demo" } else { "demo-dr" }
}

/// The catalog's namespaces this store has no fixture under, `explicit` excepted. Those answer
/// 404 - the namespace this Prism Central does not serve - rather than an empty list for every
/// kind, which is what the mock answers for a catalog-shaped path with no fixture and what a
/// real Prism Central never says about a namespace it lacks. `explicit` is for a site whose
/// knobs answer for a namespace themselves: the mock tests `unavailable_namespace` before
/// `forbid_namespace`, so a namespace in both lists would 404 where a 403 was meant.
fn namespaces_without_fixtures(store: &Store, explicit: &[&str]) -> Vec<&'static str> {
    nutsh_catalog::NAMESPACES
        .iter()
        .map(|n| n.name)
        .filter(|name| store.version_of(name).is_none() && !explicit.contains(name))
        .collect()
}

/// The row of [`SITES`] a context name belongs to. Every name reaching here came out of that
/// table, so a miss is a bug in this module and not a condition to report.
fn site_named(name: &str) -> &'static Site {
    SITES
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no demo site named {name}"))
}

/// The password store for this run: [`PASSWORD`] under the key `session::target` derives for
/// each site's account, host and port, and nothing else. In memory, so it is gone with the
/// process.
fn memory_store(sites: &[(&str, MockPc)]) -> MemoryStore {
    let store = MemoryStore::default();
    for (name, pc) in sites {
        let site = site_named(name);
        store
            .set(&secret_key(site.username, &pc.host(), pc.port()), PASSWORD)
            .expect("the memory store accepts every write");
    }
    store
}

/// A context row for a running mock: plain `host`/`port`/`username`; `--plain-http` reaches it
/// as an override on the run, not as a field, because the file has no scheme field.
fn context(pc: &MockPc, username: &str, cluster: Option<&str>) -> Context {
    Context {
        host: pc.host(),
        port: pc.port(),
        username: username.to_string(),
        verify_tls: true,
        ca_bundle: None,
        cluster: cluster.map(str::to_string),
        readonly: false,
        skin: None,
    }
}

/// The `[[guardrails]]` rows the demo's own file carries, on the edge context alone: a deny
/// with its reason, and a raised confirm. They are what the user's own rules would be in a
/// real session - those never apply here, since the file read is this one - and they make
/// `Verdict::Deny` greying and a type-the-name confirm visible one `:ctx demo-edge` away,
/// while `demo` keeps the plain built-ins.
fn edge_rules() -> Vec<RuleSpec> {
    let on_edge_vms = || RuleSpec {
        contexts: vec![EDGE.name.to_string()],
        kinds: vec!["vmm.ahv.config.Vm".to_string()],
        ..RuleSpec::default()
    };
    vec![
        RuleSpec {
            actions: vec!["delete".to_string()],
            deny: true,
            reason: Some("the demo keeps its edge VMs".to_string()),
            ..on_edge_vms()
        },
        RuleSpec {
            actions: vec!["power-off".to_string()],
            confirm: Some("type-name".to_string()),
            ..on_edge_vms()
        },
    ]
}

/// Write the demo's context file into `dir` and return its path: `current_context`, one
/// `[contexts.<site>]` per running mock, the edge pin, and [`edge_rules`]. Through
/// `nutsh_config::save`, so the file is written the way every file this program owns is:
/// atomically, at 0600. The password is not in it and could not be (`nutsh_config::load`
/// refuses the key).
fn write_config(dir: &Path, sites: &[(&str, MockPc)], a: &DemoArgs) -> anyhow::Result<PathBuf> {
    let mut cfg = Config {
        current_context: Some(a.session().to_string()),
        guardrails: edge_rules(),
        ..Config::default()
    };
    for (name, pc) in sites {
        let site = site_named(name);
        cfg.contexts
            .insert(name.to_string(), context(pc, site.username, None));
        if *name == EDGE.on {
            cfg.contexts.insert(
                EDGE.name.to_string(),
                context(pc, site.username, Some(EDGE.cluster)),
            );
        }
    }
    let path = dir.join("config.toml");
    nutsh_config::save(&path, &cfg)?;
    Ok(path)
}

/// A directory nobody else can read, for the one file the demo writes: `0700` from the
/// `mkdir` itself, and never an existing directory, which could be somebody else's. The name
/// carries the pid so two demos on one machine do not meet.
fn scratch_dir() -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("nutsh-demo-{}", std::process::id()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Boot every site, then run the session on them. The mocks live in this function's frame
/// for the whole run and are dropped after it - `MockPc` aborts its server on drop - and the
/// scratch directory is removed on every exit, the failed ones included, which is why the
/// session is a separate function whose `?` cannot skip the removal.
pub(crate) fn run(a: DemoArgs) -> anyhow::Result<i32> {
    // Before anything boots: an unknown kind is a mistake in the command line, and two mock
    // Prism Centrals are a lot of work to do before saying so.
    nutsh_tui::app::resolve_home(&a.start)?;
    let rt = crate::runtime()?;
    rt.block_on(async {
        // Under `--snapshot` the estate is served as written, at its own instant, and the
        // frame reads the same every time but for the port; interactively the fixtures are
        // rebased onto the wall clock the mock's own task stamps use, so ages agree.
        let now = if a.snapshot {
            nutsh_mockpc::demo::t0()
        } else {
            SystemTime::now()
        };
        let mut sites: Vec<(&str, MockPc)> = Vec::new();
        for site in SITES {
            let store =
                nutsh_mockpc::demo::store(site.table, now).context("the embedded demo fixtures")?;
            let absent = namespaces_without_fixtures(&store, site.explicit);
            let mut b = MockPc::builder()
                .store(store)
                .credentials(site.username, PASSWORD)
                .honours_filter()
                .honours_orderby()
                .live_clock();
            for namespace in absent {
                b = b.unavailable_namespace(namespace);
            }
            let b = (site.knobs)(b, a.snapshot);
            let pc = b
                .try_start()
                .await
                .context("binding a loopback port for the demo")?;
            sites.push((site.name, pc));
        }
        let dir = scratch_dir()?;
        let result = session(&sites, &dir, now, a).await;
        // Best effort: the file holds `127.0.0.1:<port>` and two usernames, nothing that needs
        // a second attempt.
        let _ = std::fs::remove_dir_all(&dir);
        drop(sites);
        result
    })
}

/// The run itself, on mocks somebody else keeps alive: the file, the store, and the ordinary
/// TUI entry point with the demo's context named and `--plain-http` set for its loopback host.
/// `no_cache` is unconditional - a mock on an ephemeral port would never be adopted by a cache
/// and would leave a directory under the user's state per run - and `peers` is the other
/// Prism Central under `--join`, nothing otherwise.
async fn session(
    sites: &[(&str, MockPc)],
    dir: &Path,
    now: SystemTime,
    a: DemoArgs,
) -> anyhow::Result<i32> {
    let config_path = write_config(dir, sites, &a)?;
    let secrets = Secrets::with(vec![Box::new(memory_store(sites))]);
    let peers = if a.join {
        vec![peer_of(a.session()).to_string()]
    } else {
        Vec::new()
    };
    let conn = ConnArgs {
        context: Some(a.session().to_string()),
        host: None,
        port: None,
        username: None,
        insecure: false,
        ca_bundle: None,
        plain_http: true,
    };
    tui::run_on(
        TuiArgs {
            start: a.start,
            snapshot: a.snapshot,
            peers,
            size: a.size,
            format: a.format,
            readonly: a.readonly,
            no_cache: true,
            now: a.snapshot.then_some(now),
        },
        &conn,
        None,
        config_path,
        secrets,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use nutsh_mockpc::Store;
    use nutsh_prism::{Availability, Client, ListOptions, PrismError, Profile};

    /// The four namespaces the harbor estate has no fixture for answer 404, and nothing else
    /// does; a namespace the caller says it handles itself is left alone, whatever the store
    /// holds, because the mock tests `unavailable_namespace` before `forbid_namespace` and a
    /// namespace in both lists would 404 where a 403 was meant.
    #[test]
    fn namespaces_without_fixtures_are_marked_unavailable() {
        let mut store = Store::default();
        store.insert(
            "/vmm/v4.3/ahv/config/vms",
            vec![serde_json::json!({"extId": "a"})],
        );
        store.insert("/licensing/v4.4/config/licenses", vec![]);
        let absent = namespaces_without_fixtures(&store, &[]);
        assert!(absent.contains(&"aiops"), "{absent:?}");
        assert!(absent.contains(&"objects"), "{absent:?}");
        assert!(!absent.contains(&"vmm"), "{absent:?}");
        assert!(
            !absent.contains(&"licensing"),
            "an empty list is still a fixture: {absent:?}"
        );
        let explicit = namespaces_without_fixtures(&store, &["aiops"]);
        assert!(!explicit.contains(&"aiops"), "{explicit:?}");
        assert!(explicit.contains(&"storage"), "{explicit:?}");

        let harbor =
            nutsh_mockpc::demo::store(nutsh_mockpc::demo::DEMO, nutsh_mockpc::demo::t0()).unwrap();
        let mut absent = namespaces_without_fixtures(&harbor, &[]);
        absent.sort_unstable();
        assert_eq!(absent, ["aiops", "objects", "storage", "tenancy"]);
    }

    /// One file, private, holding the two contexts the harbor site is reached as and the two
    /// rules that make `demo-edge` the context with guardrails of its own; the password is
    /// nowhere in it, and it is under the key `session::target` will look it up by.
    #[test]
    fn write_config_lists_contexts_and_two_rules() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pc = rt.block_on(MockPc::builder().start());
        let dir = tempfile::tempdir().unwrap();
        let sites = [("demo", pc)];
        let args = DemoArgs {
            start: "dashboard".into(),
            snapshot: true,
            size: (120, 40),
            format: None,
            readonly: false,
            join: false,
            site: None,
        };
        let path = write_config(dir.path(), &sites, &args).unwrap();
        assert_eq!(path, dir.path().join("config.toml"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(PASSWORD), "{text}");
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.current_context.as_deref(), Some("demo"));
        assert_eq!(
            cfg.contexts.keys().map(String::as_str).collect::<Vec<_>>(),
            ["demo", "demo-edge"]
        );
        let (_, pc) = &sites[0];
        for name in ["demo", "demo-edge"] {
            let ctx = &cfg.contexts[name];
            assert_eq!(
                (ctx.host.as_str(), ctx.port),
                (pc.host().as_str(), pc.port())
            );
            assert_eq!(ctx.username, "admin");
            assert!(!ctx.readonly, "{name}: the flags column stays empty");
        }
        assert_eq!(cfg.contexts["demo"].cluster, None);
        assert_eq!(
            cfg.contexts["demo-edge"].cluster.as_deref(),
            Some("harbor-edge")
        );
        assert_eq!(cfg.guardrails.len(), 2, "{text}");
        nutsh_core::guardrails::Guardrails::from_config(false, &cfg.guardrails)
            .expect("both rows convert");
        assert!(
            cfg.guardrails
                .iter()
                .all(|r| r.contexts == ["demo-edge"] && r.kinds == ["vmm.ahv.config.Vm"]),
            "{text}"
        );
        assert!(
            cfg.guardrails
                .iter()
                .any(|r| r.deny && r.actions == ["delete"]),
            "{text}"
        );
        assert!(
            cfg.guardrails
                .iter()
                .any(|r| r.confirm.as_deref() == Some("type-name") && r.actions == ["power-off"]),
            "{text}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the file is the user's alone");
        }
        // The password the file does not hold is in memory, under the file's own key.
        let store = memory_store(&sites);
        let key = nutsh_config::secret_key("admin", &pc.host(), pc.port());
        assert_eq!(store.get(&key).unwrap().as_deref(), Some(PASSWORD));
        assert_eq!(store.get("admin@pc.example.com:9440").unwrap(), None);
    }

    /// A ridge mock exactly as `run` would build it, minus the namespaces-without-fixtures loop,
    /// which is PR2's own test's business. The runtime is the caller's: `MockPc` serves on a
    /// task spawned onto it, and a runtime dropped at the end of this function would take the
    /// server with it.
    fn ridge(rt: &tokio::runtime::Runtime, snapshot: bool) -> MockPc {
        let site = SITES.iter().find(|s| s.name == "demo-dr").unwrap();
        let store =
            nutsh_mockpc::demo::store(site.table, nutsh_mockpc::demo::t0()).expect("DEMO_DR");
        let b = MockPc::builder()
            .store(store)
            .credentials(site.username, PASSWORD)
            .honours_filter()
            .honours_orderby();
        let b = (site.knobs)(b, snapshot);
        rt.block_on(b.try_start()).expect("a loopback port")
    }

    fn operator(pc: &MockPc) -> Profile {
        Profile {
            host: pc.host(),
            port: pc.port(),
            username: "operator".into(),
            verify_tls: true,
            ca_bundle: None,
            plain_http: true,
        }
    }

    /// One raw HTTP/1.1 request, no client: the tier is on the 401 as on every other answer, and
    /// the point is to read the header the mock wrote, not what a client made of it.
    fn advertised_limit(pc: &MockPc) -> String {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect((pc.host(), pc.port())).unwrap();
        write!(
            s,
            "GET /api/prism/v4.4/config/domain-managers HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            pc.host()
        )
        .unwrap();
        let mut text = String::new();
        s.read_to_string(&mut text).unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix("x-api-ratelimit-limit: "))
            .map(str::trim)
            .unwrap_or_else(|| panic!("no tier header in:\n{text}"))
            .to_string()
    }

    /// Interactively the ridge site paces at ten a second; under `--snapshot` it advertises the
    /// client's own ceiling, so two negotiations stay inside the frame's ten-second wait.
    #[test]
    fn the_ridge_site_paces_interactively_only() {
        let rt = crate::runtime().unwrap();
        let interactive = ridge(&rt, false);
        assert_eq!(advertised_limit(&interactive), "10");
        let snapshot = ridge(&rt, true);
        assert_eq!(advertised_limit(&snapshot), "30");
    }

    /// The three greyed states, as the client sees them after one negotiation: licensing served
    /// but forbidden, networking pinned at v4.0 with the two v4.1 kinds not served at the pin,
    /// and `:try`'s request meeting the server's own 404 rather than an empty list.
    #[test]
    fn the_ridge_site_greys_three_ways() {
        let rt = crate::runtime().unwrap();
        let pc = ridge(&rt, true);
        rt.block_on(async {
            let client = Client::connect(&operator(&pc), PASSWORD).unwrap();
            client.negotiate().await.unwrap();
            let status = |ns: &str| {
                client
                    .statuses()
                    .into_iter()
                    .find(|s| s.name == ns)
                    .unwrap()
            };
            let licensing = status("licensing");
            assert!(
                licensing.ok && licensing.pinned == Some("v4.4"),
                "{licensing:?}"
            );
            assert!(
                licensing.detail.contains("not permitted for this account"),
                "{}",
                licensing.detail
            );
            let networking = status("networking");
            assert_eq!(networking.pinned, Some("v4.0"), "{}", networking.detail);
            let nic = nutsh_catalog::kind("networking.config.NicProfile").unwrap();
            assert!(
                matches!(
                    client.availability(nic),
                    Availability::NotServedAtPin {
                        pinned: "v4.0",
                        since: "v4.1",
                        ..
                    }
                ),
                "{:?}",
                client.availability(nic)
            );
            // `:try`: ask anyway, and the answer is the server's 404, not a 200 with no rows.
            client.ask_anyway(nic);
            let answer = client.list_page(nic, 0, &ListOptions::default()).await;
            assert!(matches!(answer, Err(PrismError::NotFound(_))), "{answer:?}");
            // A licensing list is the 403, not an empty table.
            let licenses = nutsh_catalog::kind("licensing.config.License").unwrap();
            let answer = client.list_page(licenses, 0, &ListOptions::default()).await;
            assert!(
                matches!(answer, Err(PrismError::Forbidden(_))),
                "{answer:?}"
            );
        });
    }

    /// `operator` is exactly one role: power on and off say Yes through it, delete, clone and
    /// migrate say No and name the roles that would have done - the greying the storyboard's
    /// action menu shows.
    #[test]
    fn the_operator_may_power_off_and_may_not_delete() {
        use nutsh_core::can_i::{CanI, resolve};
        let rt = crate::runtime().unwrap();
        let pc = ridge(&rt, true);
        rt.block_on(async {
            let client = Client::connect(&operator(&pc), PASSWORD).unwrap();
            client.negotiate().await.unwrap();
            let index = resolve(&client, "operator").await;
            assert_eq!(
                index.roles(),
                Some(&["Virtual Machine Operator".to_string()][..])
            );
            let vm = nutsh_catalog::kind("vmm.ahv.config.Vm").unwrap();
            for yes in ["power-on", "power-off"] {
                assert!(
                    matches!(index.can(vm.action(yes).unwrap()), CanI::Yes { .. }),
                    "{yes}"
                );
            }
            for no in ["delete", "clone", "migrate-to-host"] {
                let action = vm.action(no).unwrap();
                assert_eq!(
                    index.can(action),
                    CanI::No {
                        missing: action.roles
                    },
                    "{no}"
                );
            }
        });
    }

    #[test]
    fn join_means_the_other_prism_central() {
        assert_eq!(peer_of("demo"), "demo-dr");
        assert_eq!(peer_of("demo-edge"), "demo-dr");
        assert_eq!(peer_of("demo-dr"), "demo");
    }

    /// `--site` is `current_context` in the demo's own file, and the `demo-dr` row points at the
    /// second mock under its own account; `demo-edge` stays the harbor mock with its cluster pin.
    #[test]
    fn write_config_makes_the_site_current() {
        let rt = crate::runtime().unwrap();
        let started: Vec<(&str, MockPc)> = SITES
            .iter()
            .map(|s| {
                (
                    s.name,
                    rt.block_on(MockPc::builder().store(Store::default()).try_start())
                        .unwrap(),
                )
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let args = DemoArgs {
            start: "vm".into(),
            snapshot: true,
            size: (120, 40),
            format: None,
            readonly: false,
            join: false,
            site: Some("demo-dr".into()),
        };
        let path = write_config(dir.path(), &started, &args).unwrap();
        let cfg = nutsh_config::load(&path).unwrap();
        assert_eq!(cfg.current_context.as_deref(), Some("demo-dr"));
        let dr = &cfg.contexts["demo-dr"];
        assert_eq!(
            (dr.username.as_str(), dr.port),
            ("operator", started[1].1.port())
        );
        let edge = &cfg.contexts["demo-edge"];
        assert_eq!(
            (edge.port, edge.cluster.as_deref()),
            (started[0].1.port(), Some("harbor-edge"))
        );
        assert_eq!(cfg.contexts["demo"].port, started[0].1.port());
    }
}

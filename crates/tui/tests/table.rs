mod common;

use nutsh_core::scheduler::{Msg, SubId};
use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key};

async fn app(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap()
}

#[tokio::test]
async fn vm_table_renders_default_columns_and_selection() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    // The header's counters are part of the frame, so the poller that fills them is settled
    // rather than raced, exactly as the page snapshots settle the sampler.
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("vm_table", app.snapshot(120, 19).unwrap()));

    app.handle(Key::Char('j'));
    app.handle(Key::Char('j'));
    settings.bind(|| insta::assert_snapshot!("vm_table_third_row", app.snapshot(120, 19).unwrap()));
    app.handle(Key::Char('G'));
    assert_eq!(app.selected_name(), Some("db-01"));
    app.handle(Key::Char('g'));
    assert_eq!(app.selected_name(), Some("web-01"));
}

#[tokio::test]
async fn the_header_box_names_the_session_and_the_view() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    let before = app.snapshot(120, 12).unwrap();
    assert!(before.contains("○ syncing"), "{before}");
    common::settle(&mut app).await;
    // The block reads numbers on a connected app, so the counters are settled before it is
    // read; `-` is what it says before the first cycle, which is what `before` above holds.
    assert!(
        before
            .lines()
            .nth(1)
            .unwrap()
            .ends_with("clusters -  hosts -"),
        "{before}"
    );
    common::settle_stats(&mut app).await;
    let frame = app.snapshot(120, 12).unwrap();
    let lines: Vec<&str> = frame.lines().collect();
    assert!(lines[0].starts_with("╭ nutsh "), "{frame}");
    // The field with its padding: the stats block shares these rows, so a bare `-` or `3`
    // would match whatever `Context:` and `Count:` said.
    assert!(lines[1].contains("Context:    -"), "{frame}");
    assert!(
        lines[2].contains("PC:") && lines[2].contains("pc.2024.3"),
        "{frame}"
    );
    assert!(
        lines[3].contains("Cluster:") && lines[3].contains("<all>"),
        "{frame}"
    );
    // The breadcrumb, not the kind's display name: the menu's group and item lead it.
    assert!(
        lines[4].contains("Kind:") && lines[4].contains("Compute & Storage › VMs"),
        "{frame}"
    );
    assert!(lines[5].contains("Count:      3"), "{frame}");
    assert!(lines[6].starts_with('╰'), "{frame}");
    // The hint grid appears from 96 columns up, and the stats block is right-aligned in 26:
    // its counters level with the fields, its version line level with the bottom border, and
    // nothing beside the top border or `Count:` (§11.1).
    assert!(frame.contains("a actions"), "{frame}");
    assert!(lines[0].ends_with('╮'), "{frame}");
    assert!(lines[1].ends_with("clusters 1  hosts 1"), "{frame}");
    assert!(lines[5].ends_with('│'), "{frame}");
    let version = concat!("v", env!("CARGO_PKG_VERSION"), " · pc.2024.3");
    assert!(lines[6].ends_with(version), "{frame}");
    assert!(frame.contains("● live"), "{frame}");
    // 80 columns: no room for the grid, so the hints move to the prompt line.
    let narrow = app.snapshot(80, 12).unwrap();
    assert!(!narrow.contains("a actions"), "{narrow}");
    assert!(narrow.contains(":palette"), "{narrow}");
}

#[tokio::test]
async fn a_poll_error_keeps_rows_and_shows_on_the_status_line() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.inject_error("simulated: HTTP 503");
    let frame = app.snapshot(120, 14).unwrap();
    let last = frame.lines().last().unwrap();
    assert!(last.contains("simulated: HTTP 503"), "{frame}");
    assert!(last.contains("○ syncing"), "{frame}");
    assert!(frame.contains("web-01"), "rows stay: {frame}");
}

/// A selection below the fold has to drag the viewport with it, or pressing `G` on a long
/// table looks like nothing happened.
#[tokio::test]
async fn the_selection_scrolls_into_view() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.inject_rows(50);
    app.handle(Key::Char('G'));
    assert_eq!(app.selected_name(), Some("vm-049"));
    // Fifteen rows of terminal, nine of them chrome, is four rows of table: the last row
    // cannot be on screen at the same time as the first.
    let frame = app.snapshot(120, 15).unwrap();
    assert!(
        frame.contains("▌ vm-049"),
        "the last row is selected: {frame}"
    );
    assert!(
        !frame.contains("vm-000"),
        "the viewport scrolled off the first row: {frame}"
    );
}

/// `enter`ing a term that only resolves to a child kind explains where to open it from
/// instead of opening an empty table.
#[tokio::test]
async fn opening_a_child_kind_says_where_it_lives() {
    let pc = MockPc::builder().start().await;
    let session = common::session(&pc).await;
    let error = App::open(
        session,
        Box::new(common::NoContexts),
        "nics",
        common::config(),
    )
    .err()
    .expect("NICs cannot be a root table");
    assert_eq!(error.to_string(), "NICs: open from Virtual Machines");
}

#[tokio::test]
async fn opening_a_kind_this_pc_does_not_serve_says_so() {
    let pc = MockPc::builder().unavailable_namespace("vmm").start().await;
    let session = common::session(&pc).await;
    let error = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .err()
    .expect("vmm is not served");
    assert_eq!(
        error.to_string(),
        "Virtual Machines: namespace not served",
        "{error}"
    );
}

/// A view that was popped keeps polling for a moment; its messages must not reach the store,
/// or an old table's error would land on the new one.
#[tokio::test]
async fn a_message_from_a_dead_subscription_is_dropped() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.inject_error("still broken");
    let key = app.view().unwrap().key.clone();
    // A `Complete` clears the error; this one must not, because SubId(999) is not live.
    app.apply(Msg::Complete {
        sub: SubId(999),
        key,
        generation: 99,
    });
    let frame = app.snapshot(120, 12).unwrap();
    assert!(
        frame.lines().last().unwrap().contains("still broken"),
        "the dead subscription's Complete was applied: {frame}"
    );
}

/// A poll that returns fewer rows than the selection index must not leave the selection past
/// the end of the table.
#[tokio::test]
async fn the_selection_is_clamped_when_the_rows_shrink() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.inject_rows(5);
    app.handle(Key::Char('G'));
    assert_eq!(app.selected_name(), Some("vm-004"));
    app.inject_rows(2);
    assert_eq!(app.selected_name(), Some("vm-001"));
}

/// ctrl-c is the one key that works everywhere, including modes the table handler never sees.
#[tokio::test]
async fn ctrl_c_quits_from_any_mode() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    assert!(!app.should_quit());
    app.mode = Mode::Help;
    app.handle(Key::Ctrl('c'));
    assert!(app.should_quit());
}

/// A session pinned to one cluster shows a subset of what the tables would otherwise hold.
/// The header is where the session is described, so that is where it has to say so.
#[tokio::test]
async fn the_header_names_a_pinned_cluster() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    assert!(
        app.snapshot(120, 12)
            .unwrap()
            .lines()
            .nth(3)
            .unwrap()
            .contains("Cluster:    <all>"),
        "nothing is pinned by default"
    );
    app.live.as_mut().unwrap().session.cluster = Some(nutsh_prism::ClusterRef {
        ext_id: "0006-cafe".into(),
        name: "lab-cluster".into(),
    });
    let frame = app.snapshot(120, 12).unwrap();
    assert!(
        frame
            .lines()
            .nth(3)
            .unwrap()
            .contains("Cluster:    lab-cluster"),
        "{frame}"
    );
}

/// The warm-up reaches the store through `App::apply`, so a table whose only subscription is
/// VMs still shows the cluster's and the host's names. This is exit criterion 1 against the
/// mock, where both always arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_warm_up_names_the_cluster_and_the_host() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    common::settle_names(&mut app).await;
    let frame = app.snapshot(120, 19).unwrap();
    assert!(frame.contains("lab-cluster"), "{frame}");
    assert!(frame.contains("ahv-node-1"), "{frame}");
    assert!(
        !frame.contains("0006158a") && !frame.contains("7b2f2f70"),
        "no stub survives the warm-up: {frame}"
    );
}

/// A `Msg::Names` from a session that has been replaced is dropped: the abort races a message
/// already in the channel, and the generation is what settles the race.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_names_message_is_dropped() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.apply(Msg::Names {
        generation: 999,
        names: vec![(
            "0006158a-2f0d-4d5a-8e2d-000000000010".into(),
            "someone-elses-cluster".into(),
        )],
    });
    let frame = app.snapshot(120, 19).unwrap();
    assert!(!frame.contains("someone-elses-cluster"), "{frame}");
}

/// The table's own lines from the pane title down: the sidebar that shares every line cut
/// away, the trailing border dropped, and the runs of padding collapsed - so what an assertion
/// below pins is the cells and their order, not the widths a 140-column frame happened to
/// choose for them.
fn table_lines(frame: &str) -> Vec<String> {
    let lines: Vec<&str> = frame.lines().collect();
    let title = lines
        .iter()
        .position(|l| l.contains("[3] \u{2500}"))
        .unwrap_or_else(|| panic!("no settled pane title in\n{frame}"));
    lines[title + 1..]
        .iter()
        .take(4)
        .map(|l| {
            l.rsplit("\u{2502}\u{2502}")
                .next()
                .unwrap_or(l)
                .trim_end_matches('\u{2502}')
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// One assertion per newly-fixtured kind: the curated columns are read against rows rather
/// than against an empty table, which is the difference between "the paths resolve" - which
/// the generator already proves - and "the paths hold what we think they hold". A column whose
/// path is wrong renders `-` on every row, and an empty table cannot tell the two apart.
///
/// The header line is pinned as the frame draws it, clipping included: a `Count` column is as
/// wide as its widest number, so `EXTERNAL SUBNETS` and `ROUTABLE PREFIXES` arrive as
/// `EXTERNAL SUB` and `ROUTABLE PRE`. The row lines are the rendered cells, so they pin what
/// each `ColumnKind` makes of the wire word: `Status` shouts (`ENFORCE`), `Enum`
/// sentence-cases through the acronym table (`SERVICE_ACCOUNT` → `Service account`, `VPC_LIST`
/// → `VPC list`), `Bool` marks (`✓`/`✗`), `Count` counts, and `Reference` names.
///
/// The one `-` in the five tables is deliberate: a service account has never logged in, which
/// is the absence of a fact rather than a path that missed. Everything else reads.
///
/// The users fixture deliberately does **not** name the session's own account. `can_i::resolve`
/// walks `/iam/v4.0/authn/users` for the connected username, and the mock's is `admin`: an
/// `admin` row with no matching authorization policy would resolve to *no roles*, which answers
/// `No` to every action on the default fixture tree. `Unknown` is the answer that belongs there
/// - a false `No` hides an action, a false `Yes` costs one 403 - so the local account here is
/// `nutanix`, which Prism Central predefines just as it does `admin`, and
/// `can_i::resolve_says_why_when_the_user_list_lacks_the_account` keeps reading the case it is
/// named for, now against a user list that genuinely holds users.
///
/// Only `lcm-entities` settles the warm-up: `CLUSTER` is the one `Reference` column among the
/// five kinds, and four more cycles for a cache nothing reads out of would be eleven seconds
/// of `names::PACE` spent to assert nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_new_curated_tables_read_as_rows() {
    let pc = MockPc::builder().start().await;
    for (kind, warm, expected) in [
        (
            "vpc",
            false,
            [
                "NAME TYPE SCOPE EXTERNAL SUB ROUTABLE PRE",
                "▌ lab-vpc Regular VMs 2 1",
                "dmz-vpc Transit VMs and containers 1 0",
                "k8s-vpc Regular Containers 0 2",
            ],
        ),
        (
            "security-policies",
            false,
            [
                "NAME STATE TYPE SCOPE UPDATED",
                "▌ isolate-prod MONITOR Isolation All VLAN 22h",
                "app-tier ENFORCE Application Global 16d",
                "quarantine-default SAVE Quarantine VPC list 95d",
            ],
        ),
        (
            "user",
            false,
            [
                "USERNAME STATUS TYPE DISPLAY NAME EMAIL LAST LOGIN",
                "▌ nutanix ACTIVE Local Local administrator nutanix@example.invalid 1h",
                "alice@example.invalid ACTIVE LDAP Alice Example alice@example.invalid 17h",
                "svc-backup INACTIVE Service account Backup service svc-backup@example.invalid -",
            ],
        ),
        (
            "role",
            false,
            [
                "NAME SYSTEM OPERATIONS ENTITY TYPES CLIENT UPDATED",
                "▌ Prism Admin ✓ 3 182 Prism 244d",
                "Prism Viewer ✓ 2 176 Prism 244d",
                "lab-operator ✗ 1 12 Identity and Access Management 16d",
            ],
        ),
        (
            "lcm-entities",
            true,
            [
                "ENTITY CLASS VERSION TARGET CLUSTER VENDOR",
                "▌ AHV hypervisor Hypervisor 10.0.1 10.0.3 lab-cluster Nutanix",
                "Foundation Platforms Cluster Service 2.20 2.20 lab-cluster Nutanix",
                "NX-G8 BIOS BIOS 42.300 42.600 lab-cluster Supermicro",
            ],
        ),
    ] {
        let session = common::session(&pc).await;
        let mut app = App::open(
            session,
            Box::new(common::NoContexts),
            kind,
            common::config(),
        )
        .unwrap();
        common::settle(&mut app).await;
        if warm {
            common::settle_names(&mut app).await;
        }
        let frame = app.snapshot(140, 20).unwrap();
        assert_eq!(table_lines(&frame), expected, "{kind}:\n{frame}");
        assert!(
            !frame.contains("00000000-0000"),
            "{kind}: a raw UUID reached the frame:\n{frame}"
        );
    }
}

/// `/` narrows the rows to what matches, and the title says so: a filtered table that looked
/// complete would be the one lie a table can tell.
#[tokio::test]
async fn slash_filters_the_rows_and_the_title_counts_them() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    // The header's counters are part of the frame this records, so the pollers that fill them
    // are settled rather than raced, exactly as `vm_table` settles them.
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app.handle(Key::Char('/'));
    for c in "web".chars() {
        app.handle(Key::Char(c));
    }
    let frame = app.snapshot(120, 19).unwrap();
    common::settings(&pc).bind(|| insta::assert_snapshot!("vm_table_filtered", frame.clone()));
    assert!(frame.contains("Virtual Machines [2 of 3] /web"), "{frame}");
    assert!(
        frame.contains("web-01") && frame.contains("web-02"),
        "{frame}"
    );
    assert!(
        !frame.contains("db-01"),
        "the third row is filtered out: {frame}"
    );
    // What is being typed is on the prompt line, under the table.
    assert!(frame.contains("/web"), "{frame}");
    // The header's own count agrees with the title's.
    assert!(frame.contains("Count:      2 of 3"), "{frame}");

    // `enter` keeps the filter and gives the keys back to the table.
    app.handle(Key::Enter);
    app.handle(Key::Char('j'));
    assert_eq!(app.selected_name(), Some("web-02"));
    assert!(
        app.snapshot(120, 19).unwrap().contains("[2 of 3] /web"),
        "the filter outlives the typing"
    );

    // `esc` clears it.
    app.handle(Key::Esc);
    let cleared = app.snapshot(120, 19).unwrap();
    assert!(cleared.contains("Virtual Machines [3] "), "{cleared}");
    assert!(cleared.contains("db-01"), "{cleared}");
}

/// The filter reads an address octet by octet, so a complete octet has to match a whole octet:
/// `10.1` is 10.1.x and 10.15.x, and never 110.1.x.
#[tokio::test]
async fn the_filter_reads_an_address_octet_by_octet() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    let vm = |name: &str, ip: &str| {
        serde_json::json!({
            "extId": name,
            "name": name,
            "nics": [{"nicNetworkInfo": {"ipv4Config": {"ipAddress": {"value": ip}}}}],
        })
    };
    app.inject_entities(vec![
        vm("alpha", "192.0.2.21"),
        vm("beta", "110.1.2.3"),
        vm("gamma", "10.1.2.3"),
    ]);
    let filter = |app: &mut App, term: &str| {
        app.handle(Key::Char('/'));
        for c in term.chars() {
            app.handle(Key::Char(c));
        }
        let frame = app.snapshot(120, 15).unwrap();
        app.handle(Key::Esc);
        frame
    };
    let frame = filter(&mut app, "10.1");
    assert!(frame.contains("gamma"), "{frame}");
    // By the address and not by the row's name: a name is a short word and the frame carries
    // the program's version too, where a short word can turn up by accident.
    assert!(!frame.contains("110.1.2.3"), "110 is not 10: {frame}");
    assert!(frame.contains("[1 of 3] /10.1"), "{frame}");

    // A run of octets found anywhere in the address counts; a partial one in the middle of one
    // does not, which is where a plain substring would have said yes.
    let frame = filter(&mut app, "2.21");
    assert!(frame.contains("alpha"), "{frame}");
    assert!(frame.contains("[1 of 3] /2.21"), "{frame}");
    let frame = filter(&mut app, "2.0");
    assert!(frame.contains("[0 of 3] /2.0"), "{frame}");
    assert!(frame.contains("no row here matches 2.0"), "{frame}");

    // And the identifier matches as plain text.
    let frame = filter(&mut app, "GAMM");
    assert!(frame.contains("[1 of 3] /GAMM"), "{frame}");
}

/// `/` in the body is the table's; `/` in the menu is still the menu's.
#[tokio::test]
async fn slash_belongs_to_whichever_pane_has_the_keys() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Tab);
    app.handle(Key::Char('/'));
    for c in "sub".chars() {
        app.handle(Key::Char(c));
    }
    let frame = app.snapshot(120, 15).unwrap();
    assert!(frame.contains("Subnets"), "the menu filtered: {frame}");
    assert!(
        frame.contains("Virtual Machines [3]"),
        "and the table did not: {frame}"
    );
}

/// The term has the keys exactly while the body does. `tab` leaves it standing rather than
/// typing, so a digit pressed in the menu is still a jump to a group and not part of an address.
#[tokio::test]
async fn the_term_stops_taking_keys_when_the_menu_takes_them() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Char('/'));
    for c in "web".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Tab);
    app.handle(Key::Char('2'));
    let frame = app.snapshot(120, 15).unwrap();
    assert!(
        frame.contains("[2 of 3] /web"),
        "the term stands while the menu has the keys: {frame}"
    );
    assert!(
        !frame.contains("/web█"),
        "and nothing is being typed into it: {frame}"
    );
}

/// A filter belongs to the view. Drilling into a child and coming back finds it where it was -
/// the marks beside it behave the same way - and opening the kind fresh opens it whole.
#[tokio::test]
async fn a_filter_lives_as_long_as_the_view_does() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Char('/'));
    for c in "web-01".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    // Into the VM's disks and back out again: `enter` opens the picker, the term picks the
    // child kind, and `enter` opens it.
    app.handle(Key::Enter);
    for c in "disk".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    common::settle(&mut app).await;
    assert_eq!(app.view().unwrap().key.kind.id, "vmm.ahv.config.Disk");
    app.handle(Key::Esc);
    assert!(
        app.snapshot(120, 15).unwrap().contains("[1 of 3] /web-01"),
        "the filter is where it was left"
    );
    // Opening the kind again drains the stack, and a fresh view has no filter.
    app.open_root(nutsh_catalog::kind("vmm.ahv.config.Vm").unwrap());
    assert!(
        app.snapshot(120, 15)
            .unwrap()
            .contains("Virtual Machines [3] "),
        "a fresh view opens whole"
    );
}

mod common;

use nutsh_core::scheduler::Msg;
use nutsh_core::store::Failure;
use nutsh_mockpc::MockPc;
use nutsh_prism::PrismError;
use nutsh_tui::app::{Focus, Mode};
use nutsh_tui::{App, Key};
use ratatui::buffer::Buffer;
use ratatui::style::Style;

/// A connected app on the VM table, nothing polled yet: what every page test starts from.
async fn vm_app(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap()
}

/// The settled VM table, with `term` typed into the palette to open a page over it: how a page
/// is reached from a table, and the way the menu and the command line are not.
async fn page_from_palette(pc: &MockPc, term: &str) -> App {
    let mut app = vm_app(pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    for c in term.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    common::settle_page(&mut app).await;
    app
}

async fn dr(pc: &MockPc) -> App {
    page_from_palette(pc, "disaster").await
}

/// Four panes and a summary column, each pane with the table chrome and the rows its fixtures
/// gave it, and the summary filled by the sampler's first cycle.
///
/// The sample is settled rather than raced: `settle_page` leaves the sampler running, so a
/// frame taken straight after it pins whichever of the two landed first. What the summary
/// looks like *before* a cycle - every sampled line a muted `-` - is pinned directly, by
/// `the_summary_lines_carry_the_roles_the_block_is_for`. The header's counters are settled for
/// the same reason: the stats poller is a second background task, and the snapshot pins its
/// numbers.
#[tokio::test]
async fn the_disaster_recovery_page_draws_four_panes() {
    let pc = MockPc::builder().start().await;
    let mut app = dr(&pc).await;
    common::settle_sample(&mut app).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    let frame = app.snapshot(120, 40).unwrap();
    for title in [
        "Registered Prism Centrals",
        "Protection Policies",
        "Recovery Plans",
        "Recent DR jobs",
        "Summary",
    ] {
        assert!(frame.contains(title), "{title} is missing:\n{frame}");
    }
    assert!(
        frame.contains("Registered Prism Centrals [1]")
            && frame.contains("Protection Policies [3]")
            && frame.contains("Recovery Plans [2]")
            && frame.contains("Recent DR jobs [2]"),
        "each pane counts the rows its fixtures gave it:\n{frame}"
    );
    assert!(
        frame.contains("Data Protection › Disaster Recovery"),
        "{frame}"
    );
    assert!(frame.contains("4 panes"), "the Count field: {frame}");
    assert!(
        frame.contains("pc-lab +1") && frame.contains("pc-dr"),
        "a settled warm-up names the local domain manager the policies and plans point at, \
         and the page itself names the remote one: {frame}"
    );
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("dr_page", frame));
}

/// A frame too short for the whole grid draws the rows that fit and says so in the header's
/// `Count:` field - not over the first pane, whose border and title stay whole. The sample and
/// the counters are settled for the same reason as above: a snapshot must not race a poller.
#[tokio::test]
async fn a_short_frame_drops_rows_and_the_header_counts_them() {
    let pc = MockPc::builder().start().await;
    let mut app = dr(&pc).await;
    common::settle_sample(&mut app).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    let frame = app.snapshot(80, 24).unwrap();
    assert!(
        frame.contains("2 of 4 panes"),
        "the Count field carries the notice: {frame}"
    );
    assert!(
        frame.contains("Registered Prism Centrals"),
        "the first pane's title is intact: {frame}"
    );
    assert!(
        !frame.contains("Recovery Plans"),
        "the rows that did not fit are not drawn: {frame}"
    );
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("dr_page_short", frame));
}

/// `nutsh disaster-recovery`: the command line's own way onto a page. It goes through
/// `App::open` rather than the palette, and it has to leave the menu marking what it opened -
/// the breadcrumb is the menu's path to the open view, so a page that revealed nothing would
/// open with an empty `Kind:` field.
#[tokio::test]
async fn the_command_line_opens_a_page_and_the_menu_marks_it() {
    let pc = MockPc::builder().start().await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "disaster-recovery",
        common::config(),
    )
    .unwrap();
    common::settle_page(&mut app).await;
    assert!(app.page().is_some(), "the page is the root view");
    let frame = app.snapshot(120, 40).unwrap();
    assert!(
        frame.contains("Data Protection › Disaster Recovery"),
        "the breadcrumb: {frame}"
    );
    assert!(
        frame.contains("• Disaster Recover"),
        "the menu marks it: {frame}"
    );
}

/// `tab` cycles the panes, `O` opens the focused pane's kind as a full table, and `esc` comes
/// back to the page.
#[tokio::test]
async fn tab_cycles_panes_and_o_opens_one_as_a_table() {
    let pc = MockPc::builder().start().await;
    let mut app = dr(&pc).await;
    assert_eq!(app.page().unwrap().focus, 0);
    app.handle(Key::Tab);
    assert_eq!(app.page().unwrap().focus, 1);
    app.handle(Key::BackTab);
    assert_eq!(app.page().unwrap().focus, 0);

    app.handle(Key::Char('O'));
    common::settle(&mut app).await;
    assert_eq!(
        app.view().unwrap().key.kind.id,
        "multidomain.config.RegisteredDomain"
    );
    app.handle(Key::Esc);
    assert!(app.page().is_some(), "back on the page");
}

/// Rule 6 on a page: `open_page` drains the stack, so a page is always its own root and `esc`
/// has nowhere to pop to - it goes to the menu, which is what the page's prompt line says.
/// Without this the whole suite stays green with `handle_page`'s `esc` back at `pop`, where it
/// was a no-op and the frame's `esc:menu` a promise nothing held it to.
#[tokio::test]
async fn esc_on_a_page_focuses_the_menu() {
    let pc = MockPc::builder().start().await;
    let mut app = dr(&pc).await;
    assert_eq!(app.focus, Focus::Body);
    app.handle(Key::Esc);
    assert_eq!(app.focus, Focus::Sidebar);
    assert!(app.page().is_some(), "and the page is still open");
}

/// A buried page stops polling: six lists every few seconds for a screen nobody is looking at
/// is six requests too many, and re-subscribing starts a fresh generation.
///
/// The page's own sampler is held to the same rule, though the scheduler knows nothing about
/// it: its cycle is a list and up to fifty gets, the most expensive thing the page does.
#[tokio::test]
async fn a_buried_page_releases_its_subscriptions() {
    let pc = MockPc::builder().start().await;
    let mut app = dr(&pc).await;
    assert_eq!(app.page().unwrap().live_panes(), 4);
    assert!(app.page().unwrap().sampling(), "the sampler runs with it");
    app.handle(Key::Char('O'));
    assert_eq!(app.page_under().unwrap().live_panes(), 0, "released");
    assert!(
        !app.page_under().unwrap().sampling(),
        "and the sampler with them"
    );
    app.handle(Key::Esc);
    assert_eq!(app.page().unwrap().live_panes(), 4, "taken out again");
    assert!(app.page().unwrap().sampling(), "and the sampler with them");
}

/// A pane whose namespace this Prism Central does not serve keeps its block and says the
/// reason instead of rows; the others still poll.
#[tokio::test]
async fn a_pane_the_pc_cannot_serve_says_so_and_the_others_carry_on() {
    let pc = MockPc::builder()
        .unavailable_namespace("multidomain")
        .start()
        .await;
    let mut app = dr(&pc).await;
    let frame = app.snapshot(120, 40).unwrap();
    assert!(frame.contains("namespace not served"), "{frame}");
    assert!(frame.contains("gold-sync"), "the others polled: {frame}");
    // `O` refuses for the reason the pane is already showing, rather than opening a table that
    // would poll and fail on a loop with nothing marking it.
    app.handle(Key::Char('O'));
    assert!(app.page().is_some(), "still on the page: {:?}", app.status);
    let status = app.status.clone().expect("a refusal says why");
    assert!(
        status.starts_with("Registered Prism Centrals: ") && status.contains("namespace"),
        "{status}"
    );
}

/// The four panes with their verified paths, drawn from the fixtures: the RPO is a duration,
/// the sites fan out, and a `Reference` column resolves through the store's name cache - the
/// recovery site does, because the registered-domains pane taught the cache `pc-dr` in the
/// same cycle that drew it.
///
/// The local domain manager - the primary site of every plan, and the first of every policy's
/// sites - is named by the warm-up instead: `prism.config.DomainManager` is warm and carries
/// `name_path = "config.name"`, so a settled session reads `pc-lab`. This test does not settle
/// the warm-up, so those cells are still the dim 8-character stub, and the fan-out is what it
/// asserts rather than the id inside it. `the_disaster_recovery_page_draws_four_panes` settles
/// the warm-up and is where they read as names.
#[tokio::test]
async fn the_disaster_recovery_panes_render_their_columns() {
    let pc = MockPc::builder().start().await;
    let app = dr(&pc).await;
    let frame = app.snapshot(140, 40).unwrap();
    assert!(frame.contains("pc-dr"), "{frame}");
    assert!(frame.contains("CONNECTED"), "{frame}");
    assert!(
        frame.contains("gold-sync") && frame.contains("bronze-local"),
        "{frame}"
    );
    assert!(
        frame.contains(" +1"),
        "the sites fan out to the first plus a count of the rest: {frame}"
    );
    assert!(
        frame.contains("6h"),
        "21600 seconds is an RPO nobody reads: {frame}"
    );
    assert!(frame.contains("0s"), "a synchronous policy: {frame}");
    assert!(frame.contains("dr-tier1"), "{frame}");
    assert!(
        !frame.contains("pc-lab"),
        "the local PC's name comes from the warm-up, which this test leaves unsettled, so the \
         primary site and the first of the sites are stubs: {frame}"
    );
    // An `Enum` is sentence case and a `Status` shouts, so the action and the state of the
    // same row are written two different ways on purpose.
    assert!(
        frame.contains("Test failover") && frame.contains("SUCCEEDED"),
        "{frame}"
    );
}

/// `multidomain` sub-paths are known to 404 on real Prism Centrals even when the spec declares
/// them, so a 404 on a pane is the pane's reason, not an error banner over the page.
#[tokio::test]
async fn a_404_on_one_pane_is_drawn_inside_that_pane() {
    let pc = MockPc::builder()
        .missing_path("/multidomain/v4.3/config/registered-domains")
        .start()
        .await;
    let app = dr(&pc).await;
    let frame = app.snapshot(120, 40).unwrap();
    assert!(
        frame.contains("Registered Prism Centrals"),
        "the block stays: {frame}"
    );
    // Inside the pane's border, which is the whole claim: the flash at the foot of the frame
    // carries the error too, and a needle that either line could satisfy would pass on a page
    // that drew the 404 as a banner and left the pane blank.
    assert!(
        frame
            .lines()
            .any(|l| l.starts_with('│') && l.contains("not served by this Prism Central")),
        "the reason is inside it: {frame}"
    );
    assert!(
        frame.contains("gold-sync"),
        "the other panes still poll: {frame}"
    );
}

/// And the answer sticks. A pane's reason was fixed at construction, so the only 404 that
/// could ever grey a pane was one the session already knew about: this one arrives from a live
/// poll, greys the pane it arrived for, and ends that pane's subscription - and opening the
/// page again asks nothing more, because the client remembers what the server said.
#[tokio::test]
async fn a_live_404_greys_the_pane_and_the_page_does_not_ask_again() {
    const REGISTERED: &str = "/config/registered-domains";
    let pc = MockPc::builder()
        .missing_path("/multidomain/v4.3/config/registered-domains")
        .start()
        .await;
    let mut app = dr(&pc).await;
    {
        let page = app.page().expect("the DR page");
        assert_eq!(
            page.panes[0].reason.as_deref(),
            Some("not served by this Prism Central (HTTP 404)"),
            "the pane learned it from the poll"
        );
        assert_eq!(page.live_panes(), 3, "and stopped polling for it");
    }
    let asked = pc.requests_to(REGISTERED).len();
    assert!(asked > 0, "it did ask, once");

    // Open the page again, the way a person would.
    app.handle(Key::Char(':'));
    for c in "disaster".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    common::settle_page(&mut app).await;
    assert_eq!(
        pc.requests_to(REGISTERED).len(),
        asked,
        "the second visit asks nothing the first one already had an answer to"
    );
    assert_eq!(
        app.page().expect("the DR page").panes[0].reason.as_deref(),
        Some("not served by this Prism Central (HTTP 404)")
    );
}

/// `multidomain` pinned at v4.2 is a real case: `RegisteredDomain` first appears at v4.3, and
/// v4.2 has no `registered-domains` path to step down to - the catalog knows it, because the
/// path is absent from the version's own spec.
///
/// So the pane says which version it is not in, and says it without asking. The request it
/// would otherwise make could only ever 404, once a cycle, for the session.
#[tokio::test]
async fn a_pane_newer_than_the_pin_greys_without_asking() {
    const REGISTERED: &str = "/config/registered-domains";
    let pc = MockPc::builder()
        .serve_versions("multidomain", &["v4.2"])
        .start()
        .await;
    let mut app = vm_app(&pc).await;
    common::settle(&mut app).await;
    // Negotiation probes multidomain at v4.3 before stepping down, and this pane's path is one
    // of the candidates it may try. What is under test is what the *page* asks for, so the
    // count starts where the page does.
    let probed = pc.requests_to(REGISTERED).len();

    app.handle(Key::Char(':'));
    for c in "disaster".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    common::settle_page(&mut app).await;

    assert_eq!(
        pc.requests_to(REGISTERED).len(),
        probed,
        "the pane must not ask for a path this Prism Central has no version of"
    );
    // Inside the pane's own border, for the reason the test above gives: a needle any line
    // could satisfy would pass on a page that drew the sentence as a banner.
    //
    // Both versions, because one of them alone leaves the reader with the wrong question:
    // which version this session settled on, and which one the catalog first saw the path at.
    let frame = app.snapshot(140, 40).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.starts_with('│')
                && l.contains("needs multidomain v4.3 (this PC pinned v4.2)")),
        "{frame}"
    );
    assert!(
        frame.contains("gold-sync"),
        "the other panes still poll: {frame}"
    );
}

/// The greying above is a prediction the catalog makes before the server has had a vote, and
/// `Client::pinned_path_for` argues the other side of the same case: a Prism Central may route
/// a newer path while answering the probes only at an older version. `:try` is where a person
/// who thinks the prediction is wrong about their Prism Central settles it.
#[tokio::test]
async fn try_asks_for_a_kind_the_catalog_calls_too_new() {
    const REGISTERED: &str = "/config/registered-domains";
    let pc = MockPc::builder()
        .serve_versions("multidomain", &["v4.2"])
        .start()
        .await;
    let mut app = vm_app(&pc).await;
    common::settle(&mut app).await;
    let probed = pc.requests_to(REGISTERED).len();

    // Opened by name, the kind is refused, and the refusal says what it would take.
    palette(&mut app, "registered-domains");
    assert_eq!(
        app.status.as_deref(),
        Some("needs multidomain v4.3 (this PC pinned v4.2)")
    );
    assert_eq!(
        pc.requests_to(REGISTERED).len(),
        probed,
        "and nothing was asked for"
    );

    palette(&mut app, "try registered-domains");
    common::settle(&mut app).await;
    assert!(
        pc.requests_to(REGISTERED).len() > probed,
        "the server is asked: {:?}",
        pc.requests().iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    assert!(
        pc.requests_to(REGISTERED)
            .iter()
            .all(|r| r.path.contains("/multidomain/v4.3/")),
        "at the version the catalog says the path is at: {:?}",
        pc.requests_to(REGISTERED)
            .iter()
            .map(|r| &r.path)
            .collect::<Vec<_>>()
    );
    // This mock does not route v4.3, so the answer is a 404 - and that is now what greys the
    // kind, which is a better reason than the one it replaced.
    let frame = app.snapshot(140, 40).unwrap();
    assert!(
        frame.contains("not served by this Prism Central (HTTP 404)"),
        "{frame}"
    );
}

/// `term` typed into the palette and run.
fn palette(app: &mut App, term: &str) {
    app.handle(Key::Char(':'));
    for c in term.chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
}

/// A 503 is **not now**, where a 404 is **not here**, and the pane has to say which: a Recent
/// DR jobs pane can answer `503 Failed to perform the operation due to backend service
/// unavailability, retry after some time.` once and 200 the next time in the same session.
///
/// Two claims: the sentence names the service and says it is being retried, and not one word of
/// the server's own is on the frame.
#[tokio::test]
async fn a_5xx_on_a_pane_says_which_service_and_that_it_is_being_retried() {
    let pc = MockPc::builder()
        .fail_path("/dataprotection/v4.4/config/recovery-plan-jobs", 503)
        .start()
        .await;
    let app = dr(&pc).await;
    let frame = app.snapshot(140, 40).unwrap();
    assert!(
        frame.lines().any(|l| l.starts_with('│')
            && l.contains("dataprotection unavailable (HTTP 503) - retrying")),
        "{frame}"
    );
    assert!(
        !frame.contains("is failing with HTTP"),
        "the mock's own sentence is the far end's, and stays off the frame: {frame}"
    );
    assert!(
        frame.contains("Recovery Plans [2]"),
        "the other panes are unaffected: {frame}"
    );
}

/// And a table that 404s draws the reason where its rows would be, and the menu greys the item
/// **after the fact** - before it is opened there is nothing to know.
#[tokio::test]
async fn a_table_that_404s_draws_the_reason_and_greys_its_menu_item() {
    let pc = MockPc::builder()
        .missing_path("/vmm/v4.3/ahv/config/vms")
        .start()
        .await;
    let mut app = vm_app(&pc).await;
    common::settle(&mut app).await;
    let frame = app.snapshot(120, 30).unwrap();
    // Where the rows would be, inside the table's border: the status flash carries the error
    // too, and a needle the whole frame answers would not tell the two apart.
    assert!(
        frame
            .lines()
            .any(|l| l.starts_with('│') && l.contains("not served by this Prism Central")),
        "{frame}"
    );
    // And nowhere on the frame - not the body, not the status line - the far end's own words.
    // A real Prism Central answers an unrouted path with "The requested URL was not found on
    // the server. If you entered the URL manually please check your spelling and try again.",
    // which in a TUI names a URL nobody typed; the mock's stand-in for it is `no such path`.
    assert!(!frame.contains("no such path"), "{frame}");
    // Once, not twice. A status line repeating the same failure under a body already stating
    // it is one frame, one 404, said in two different vocabularies.
    assert_eq!(
        frame
            .lines()
            .filter(|l| l.contains("not served by this Prism Central"))
            .count(),
        1,
        "{frame}"
    );
    let _skin = common::pin_skin();
    let p = nutsh_tui::theme::snapshot();
    let buf = app.frame(120, 30).unwrap();
    assert_eq!(
        common::style_of_within(&buf, 0..24, "VMs").fg,
        Some(p.overlay1),
        "the menu item is greyed once the answer is in"
    );
}

/// And a table that already has rows keeps them: the store's contract for a failed cycle is
/// that the rows stay, so the sentence takes the place of nothing. Reachable whenever a cache
/// restore painted rows before the first live cycle answered - and the subscription stops on
/// that answer, so rows hidden here would never come back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_404_over_rows_that_are_already_drawn_leaves_them_alone() {
    let pc = MockPc::builder().start().await;
    let mut app = vm_app(&pc).await;
    common::settle(&mut app).await;
    let view = app.view().expect("the VM table");
    let (sub, key) = (view.sub, view.key.clone());
    let generation = app.live.as_ref().unwrap().store.table(&key).generation + 1;
    app.apply(Msg::Error {
        sub,
        key: key.clone(),
        generation,
        error: Failure::of(key.kind, &PrismError::NotFound("no such path".into()), true),
    });
    assert!(app.live.as_ref().unwrap().store.table(&key).not_served);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(
        frame.contains("web-01"),
        "the rows are still drawn:\n{frame}"
    );
    assert!(
        !frame
            .lines()
            .any(|l| l.starts_with('│') && l.contains("not served by this Prism Central")),
        "and nothing replaced them:\n{frame}"
    );
    // It is on the status line instead, which is the case `error_text` exists for: rows on
    // screen, a failure the body has no room to state.
    assert!(
        frame
            .lines()
            .last()
            .is_some_and(|l| l.contains("not served by this Prism Central (HTTP 404)")),
        "{frame}"
    );
}

/// The summary box: four `$limit=1` counters and a bounded sampler that never claims to be a
/// census.
#[tokio::test]
async fn the_summary_counts_and_samples() {
    let pc = MockPc::builder().start().await;
    let mut app = dr(&pc).await;
    common::settle_sample(&mut app).await;
    let frame = app.snapshot(120, 40).unwrap();
    for line in [
        "Policies",
        "Recovery plans",
        "Recovery points",
        "Recovery jobs",
        "Sampled VMs",
        "in sync",
        "syncing",
        "out of sync",
    ] {
        assert!(frame.contains(line), "{line} is missing:\n{frame}");
    }
    let sample = app.live.as_ref().unwrap().store.sample();
    assert_eq!(sample.policies, Some(3));
    assert_eq!(sample.plans, Some(2));
    assert_eq!(sample.recovery_points, Some(5));
    assert_eq!(sample.jobs, Some(2));
    assert_eq!(
        (
            sample.sampled,
            sample.in_sync,
            sample.syncing,
            sample.out_of_sync
        ),
        (3, 1, 1, 1)
    );
    // One request per VM, and the counters read the total rather than the rows.
    assert_eq!(
        pc.requests_to("/config/protected-resources").len(),
        0,
        "never listed"
    );
    assert_eq!(
        pc.requests()
            .iter()
            .filter(|r| r.path.contains("/protected-resources/"))
            .count(),
        3
    );
    assert!(
        pc.requests_to("/config/recovery-points")
            .iter()
            .any(|r| r.query.iter().any(|(k, v)| k == "$limit" && v == "1")),
        "the counters are $limit=1"
    );
    // The one thing a text frame cannot say: an out-of-sync VM is drawn in the error colour,
    // which is the whole reason the block exists rather than a line of the header.
    let _skin = common::pin_skin();
    let buf = app.frame(120, 40).unwrap();
    assert_eq!(
        summary_value_style(&buf, "out of sync").fg,
        Some(nutsh_tui::theme::role_fg(nutsh_catalog::Role::Error)),
        "one VM out of sync is an error, not a count"
    );
    assert_eq!(
        summary_value_style(&buf, "in sync").fg,
        Some(nutsh_tui::theme::role_fg(nutsh_catalog::Role::Ok))
    );
}

/// A 404 on one VM is an answer, not a missing endpoint: inside a served `dataprotection` it
/// means the VM is not protected, so it tallies as nothing, leaves `available` true, and the
/// block shows zeroes rather than dashes. The dashes are for a namespace that is not served -
/// `an_unserved_namespace_dashes_the_sampled_lines`, below.
#[tokio::test]
async fn a_404_per_vm_is_not_protected_not_unavailable() {
    let pc = MockPc::builder()
        .missing_path(
            "/dataprotection/v4.4/config/protected-resources/3d0c4a2e-1b8f-4c1a-9e2f-000000000001",
        )
        .missing_path(
            "/dataprotection/v4.4/config/protected-resources/3d0c4a2e-1b8f-4c1a-9e2f-000000000002",
        )
        .missing_path(
            "/dataprotection/v4.4/config/protected-resources/3d0c4a2e-1b8f-4c1a-9e2f-000000000003",
        )
        .start()
        .await;
    let mut app = dr(&pc).await;
    common::settle_sample(&mut app).await;
    let sample = app.live.as_ref().unwrap().store.sample();
    assert!(
        sample.available,
        "a per-VM 404 is 'not protected', not 'endpoint missing'"
    );
    assert_eq!(
        (sample.in_sync, sample.syncing, sample.out_of_sync),
        (0, 0, 0)
    );
    assert_eq!(sample.sampled, 3, "three VMs were asked about");
    assert_eq!(sample.policies, Some(3), "the counters are unaffected");
    assert_eq!(summary_value(&app, "in sync"), "0", "zeroes, not dashes");
}

/// The namespace is not served at all: the three sampled lines are `-` and the counters beside
/// them keep whatever they could read. Nothing is asked about any VM - three zeroes would be a
/// silent wrong answer, since a 404 from an absent namespace looks exactly like a VM that is
/// not protected.
#[tokio::test]
async fn an_unserved_namespace_dashes_the_sampled_lines() {
    let pc = MockPc::builder()
        .unavailable_namespace("dataprotection")
        .start()
        .await;
    let mut app = dr(&pc).await;
    common::settle_sample(&mut app).await;
    let sample = app.live.as_ref().unwrap().store.sample();
    assert!(!sample.available, "the question was never asked");
    assert_eq!(sample.sampled, 0);
    assert_eq!(
        pc.requests()
            .iter()
            .filter(|r| r.path.contains("/protected-resources/"))
            .count(),
        0,
        "and not fifty requests spent learning that"
    );
    for label in ["in sync", "syncing", "out of sync"] {
        assert_eq!(summary_value(&app, label), "-", "{label}");
    }
    assert_eq!(
        summary_value(&app, "Policies"),
        "3",
        "the counters beside them are unaffected"
    );
    let frame = app.snapshot(120, 40).unwrap();
    assert!(
        frame
            .lines()
            .any(|l| l.contains("in sync") && l.trim_end().ends_with("-│")),
        "the dashes are drawn, not merely built:\n{frame}"
    );
}

/// The roles the block assigns, which no text snapshot can see: `Ok` and `Pending` for the two
/// healthy tallies, `Error` the moment anything is out of sync, and a muted `-` on all three
/// before the first cycle or where the namespace is not served. Built from the store directly,
/// so both states are pinned without a Prism Central that can produce them.
#[test]
fn the_summary_lines_carry_the_roles_the_block_is_for() {
    use nutsh_catalog::Role;
    use nutsh_core::sampler::ProtectionSample;
    use nutsh_core::store::Store;

    let built = |sample| {
        let mut store = Store::default();
        store.set_sample(sample);
        nutsh_tui::page::summary::build("disaster-recovery", &store)
            .into_iter()
            .map(|l| (l.label, l.value, l.role))
            .collect::<Vec<_>>()
    };
    let line = |label: &str, value: &str, role| (label.to_string(), value.to_string(), role);

    assert_eq!(
        built(ProtectionSample {
            policies: Some(3),
            plans: Some(2),
            recovery_points: Some(5),
            jobs: Some(2),
            sampled: 3,
            in_sync: 1,
            syncing: 1,
            out_of_sync: 1,
            available: true,
        }),
        vec![
            line("Policies", "3", Role::Neutral),
            line("Recovery plans", "2", Role::Neutral),
            line("Recovery points", "5", Role::Neutral),
            line("Recovery jobs", "2", Role::Neutral),
            line("Sampled VMs", "3", Role::Neutral),
            line("  in sync", "1", Role::Ok),
            line("  syncing", "1", Role::Pending),
            line("  out of sync", "1", Role::Error),
        ]
    );
    // Nothing out of sync is not an error, whatever the line is called.
    assert_eq!(
        built(ProtectionSample {
            sampled: 2,
            in_sync: 2,
            available: true,
            ..Default::default()
        })[7],
        line("  out of sync", "0", Role::Muted)
    );
    // Before the first cycle, and wherever the namespace is not served: the sampled lines
    // dash and mute, `Sampled VMs` still prints its count, and the counters are untouched.
    assert_eq!(
        built(ProtectionSample {
            policies: Some(3),
            ..Default::default()
        }),
        vec![
            line("Policies", "3", Role::Neutral),
            line("Recovery plans", "-", Role::Neutral),
            line("Recovery points", "-", Role::Neutral),
            line("Recovery jobs", "-", Role::Neutral),
            line("Sampled VMs", "0", Role::Neutral),
            line("  in sync", "-", Role::Muted),
            line("  syncing", "-", Role::Muted),
            line("  out of sync", "-", Role::Muted),
        ]
    );
}

/// The value the open page's summary shows against `label`.
fn summary_value(app: &App, label: &str) -> String {
    app.page()
        .expect("a page is open")
        .summary
        .iter()
        .find(|l| l.label.trim() == label)
        .unwrap_or_else(|| panic!("no summary line labelled {label:?}"))
        .value
        .clone()
}

/// The style of a summary line's value.
///
/// `common::style_of` finds text, and the three tallies of these fixtures are all `1`, so a
/// needle cannot say which line it means: this finds the row by its label and reads the value
/// from the other end of it, where the block right-aligns it.
fn summary_value_style(buf: &Buffer, label: &str) -> Style {
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width)
            .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        if !row.contains(label) {
            continue;
        }
        for x in (0..buf.area.width).rev() {
            let cell = buf.cell((x, y)).expect("inside the buffer");
            if cell
                .symbol()
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-')
                && !cell.symbol().trim().is_empty()
            {
                return Style::default()
                    .fg(cell.fg)
                    .bg(cell.bg)
                    .add_modifier(cell.modifier);
            }
        }
        panic!(
            "{label:?} has no value on its row:\n{}",
            common::text_of(buf)
        );
    }
    panic!("{label:?} is not in the frame:\n{}", common::text_of(buf));
}

/// The stats poller's seven counters, read off the mock. The mock ignores `$filter` and
/// reports the unfiltered total, so a filtered counter is asserted on the recorded request,
/// not on the number.
#[tokio::test]
async fn the_stats_poller_fills_the_header() {
    let pc = MockPc::builder().start().await;
    let mut app = vm_app(&pc).await;
    common::settle_stats(&mut app).await;
    let stats = *app.live.as_ref().unwrap().store.stats();
    assert_eq!(stats.clusters, Some(1));
    assert_eq!(stats.hosts, Some(1));
    assert_eq!(stats.vms, Some(3));
    assert!(!stats.stale);
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("clusters 1"), "{frame}");
    assert!(frame.contains("vms 3"), "{frame}");
    for (path, needle) in [
        ("/ahv/config/vms", "PowerState'ON'"),
        ("/serviceability/alerts", "Severity'CRITICAL'"),
        ("/config/tasks", "TaskStatus'RUNNING'"),
    ] {
        assert!(
            pc.requests_to(path).iter().any(|r| {
                r.query
                    .iter()
                    .any(|(k, v)| k == "$filter" && v.contains(needle))
            }),
            "{path} was never asked with {needle}"
        );
    }
}

/// A counter whose namespace this Prism Central does not serve is `-`; the others are
/// unaffected, and the failing one is dropped from the cycle rather than refused every 30 s
/// for ever.
#[tokio::test]
async fn one_unserved_namespace_dashes_one_counter() {
    let pc = MockPc::builder()
        .unavailable_namespace("monitoring")
        .start()
        .await;
    let mut app = vm_app(&pc).await;
    common::settle_stats(&mut app).await;
    let stats = *app.live.as_ref().unwrap().store.stats();
    assert_eq!(stats.alerts_critical, None);
    assert_eq!(stats.alerts_warning, None);
    assert_eq!(stats.vms, Some(3), "the others are unaffected");
    assert!(!stats.stale, "a per-counter refusal is not a failed cycle");
}

/// A stale block still draws the numbers it last had, rather than a screenful of dashes.
///
/// `stale` is injected: what the poller does with a failed cycle is its own rule, and
/// `nutsh_core::stats`'s `a_failed_cycle_keeps_the_previous_values_and_marks_them_stale` drives
/// it there. This pins what the frame does with the flag; `a_stale_stats_block_is_dim` pins
/// the colour.
#[tokio::test]
async fn a_stale_block_still_draws_the_numbers_it_last_had() {
    let pc = MockPc::builder().start().await;
    let mut app = vm_app(&pc).await;
    common::settle_stats(&mut app).await;
    app.inject_stale_stats();
    let stats = *app.live.as_ref().unwrap().store.stats();
    assert_eq!(stats.vms, Some(3), "kept");
    assert!(stats.stale);
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("vms 3"), "{frame}");
}

/// The Dashboard: four panes and a summary that spells out what the header has no room for.
///
/// The pane titles are asserted with the row count the pane's chrome appends, because
/// `Clusters` and `Running tasks` are also summary labels: a bare needle would pass on the
/// summary column alone, with the grid missing.
#[tokio::test]
async fn the_dashboard_draws_four_panes_and_its_summary() {
    let pc = MockPc::builder().start().await;
    let mut app = page_from_palette(&pc, "dashboard").await;
    common::settle_stats(&mut app).await;
    assert_eq!(
        app.page().expect("the Dashboard is open").panes.len(),
        4,
        "four panes"
    );
    let frame = app.snapshot(140, 40).unwrap();
    for title in [
        "Clusters [1]",
        "Unresolved alerts [2]",
        "Running tasks [4]",
        "VMs [3]",
        "Summary",
    ] {
        assert!(frame.contains(title), "{title}:\n{frame}");
    }
    assert!(frame.contains("lab-cluster"), "{frame}");
    assert!(frame.contains("CRITICAL"), "{frame}");
    assert!(
        frame.contains("Virtual machines"),
        "the summary spells the counters out: {frame}"
    );
    // The counters the summary spells out are the ones the header shows.
    assert_eq!(summary_value(&app, "Virtual machines"), "3");
    assert_eq!(summary_value(&app, "Clusters"), "1");
    assert!(frame.contains("clusters 1"), "the header's block: {frame}");
}

/// The pane's own filter, as the Dashboard declares it: the stats poller lists the same path
/// with its own two filters, so a request is the alerts *pane's* only if this is the `$filter`
/// it carries.
const UNRESOLVED: &str = "isResolved eq false";

/// The alerts pane's list requests, in order, with the `$limit` each asked for.
fn pane_limits(pc: &MockPc, path: &str) -> Vec<u32> {
    pc.requests_to(path)
        .iter()
        .filter(|r| {
            r.query
                .iter()
                .any(|(k, v)| k == "$filter" && v == UNRESOLVED)
        })
        .map(|r| {
            r.query
                .iter()
                .find(|(k, _)| k == "$limit")
                .map(|(_, v)| v.parse().expect("a numeric $limit"))
                .unwrap_or(0)
        })
        .collect()
}

/// The open page's pane subscriptions, which change only when a pane resubscribes.
fn pane_subs(app: &App) -> Vec<Option<nutsh_core::scheduler::SubId>> {
    app.page()
        .expect("a page is open")
        .panes
        .iter()
        .map(|p| p.sub)
        .collect()
}

/// A pane fetches the rows it can show, not the kind's whole budget: the Dashboard's alerts
/// pane inherited `Kind::max_rows` and walked five hundred alert objects every ten seconds to
/// draw ten rows. It now asks for twenty, in one request, and asks again only when a frame has
/// drawn it taller than what it asked for.
///
/// This is the wiring the budget needs and the pure function cannot prove: that `draw_pane`
/// records the height, that `App::tick` resizes on it, and that a tick which changes nothing
/// costs nothing.
#[tokio::test]
async fn a_pane_asks_for_its_own_height_and_refetches_only_when_it_grows() {
    let alerts = nutsh_catalog::kind("monitoring.serviceability.Alert").expect("Alerts");
    assert_eq!(
        alerts.max_rows,
        Some(500),
        "the catalog budget the pane no longer inherits"
    );
    let pc = MockPc::builder().start().await;
    let mut app = vm_app(&pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Char(':'));
    for c in "dashboard".chars() {
        app.handle(Key::Char(c));
    }
    app.handle(Key::Enter);
    common::settle_page(&mut app).await;
    assert_eq!(
        pane_limits(&pc, alerts.list_path),
        vec![20],
        "one cycle, one request, twenty objects - not five pages of a hundred"
    );

    // A frame no taller than the floor asks for nothing new: the pane already holds every row
    // it can draw, so the tick leaves the subscription - and the request count - alone.
    let subs = pane_subs(&app);
    app.snapshot(140, 24).unwrap();
    app.tick(common::now(), std::time::Instant::now());
    assert_eq!(
        pane_subs(&app),
        subs,
        "a pane inside its budget is left alone"
    );
    assert_eq!(pane_limits(&pc, alerts.list_path), vec![20]);

    // Drawn taller, it fetches what it can now show.
    app.snapshot(140, 44).unwrap();
    app.tick(common::now(), std::time::Instant::now());
    let grown = pane_subs(&app);
    assert_ne!(grown, subs, "every pane grew, so every pane resubscribed");
    common::settle_page(&mut app).await;
    let limits = pane_limits(&pc, alerts.list_path);
    assert_eq!(
        limits.len(),
        2,
        "one more cycle, one more request: {limits:?}"
    );
    assert!(
        limits[1] > 20,
        "the taller pane asks for more than the floor: {limits:?}"
    );

    // And a tick with the height unchanged is free.
    app.tick(common::now(), std::time::Instant::now());
    assert_eq!(
        pane_subs(&app),
        grown,
        "nothing was redrawn, nothing resubscribes"
    );
    assert_eq!(pane_limits(&pc, alerts.list_path).len(), 2);
}

/// Exit criterion 3 of the visual curation, against the mock: the Protection Policies pane
/// reads a name, a resolved site with a count of the ones it stands for, one RPO where the
/// policy carries two identical ones, and a count of categories.
///
/// `PRIMARY` is asserted against the pane's own header line rather than against the frame: the
/// Recovery Plans pane below still has a `PRIMARY SITE`, and that is not the column that was
/// cut. What was cut is `replicationLocations[].isPrimary` - a `Bool` fan-out over a field
/// every policy sets exactly once, which is where `true,fa` came from and why no cell anywhere
/// on the page may hold it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_protection_policies_pane_reads_as_a_sentence() {
    let pc = MockPc::builder().start().await;
    let mut app = dr(&pc).await;
    common::settle_names(&mut app).await;
    let frame = app.snapshot(140, 40).unwrap();
    let header = frame
        .lines()
        .find(|l| l.contains("SITES"))
        .unwrap_or_else(|| panic!("no Protection Policies header in\n{frame}"));
    for column in ["NAME", "SITES", "RPO", "CATEGORIES"] {
        assert!(header.contains(column), "{column} is missing: {header}");
    }
    assert!(
        !header.contains("PRIMARY"),
        "the isPrimary fan-out is gone from this pane: {header}"
    );
    assert!(
        !frame.contains("true,fa"),
        "and no cell anywhere on the page holds a truncated Bool fan-out:\n{frame}"
    );
    assert!(
        frame.contains("pc-lab +1"),
        "a settled warm-up names the first site and counts the rest: {frame}"
    );
}

/// Six of the nine curated groups now open on a landing page, the way Data Protection opens on
/// Disaster Recovery: `enter` on the group's first item draws that group's own kinds as panes,
/// and the individual resources are still under it.
#[tokio::test]
async fn a_groups_overview_item_opens_its_landing_page() {
    let pc = MockPc::builder().start().await;
    let mut app = vm_app(&pc).await;
    common::settle(&mut app).await;
    app.handle(Key::Tab);
    app.handle(Key::Char('3')); // Network & Security
    app.handle(Key::Char('l')); // expand
    app.handle(Key::Char('j')); // Overview, the group's first item
    app.handle(Key::Enter);
    common::settle_page(&mut app).await;
    let frame = app.snapshot(120, 30).unwrap();
    for title in ["Subnets", "VPCs", "Gateways", "Security Policies"] {
        assert!(frame.contains(title), "{title}:\n{frame}");
    }
    // The fixtures' rows, so the panes are drawing their kinds and not just their titles.
    assert!(frame.contains("lab-vpc"), "{frame}");
    assert!(frame.contains("isolate-prod"), "{frame}");
    // And the resources are still reachable beneath it.
    assert!(frame.contains("Floating IPs"), "{frame}");
}

/// `a` is bound on a page, and it acts on the focused pane's row. The Dashboard is the launch
/// view and every group opens a page, so a session that never leaves one would otherwise never
/// meet an action at all.
///
/// `actions run on a table: O opens this pane's kind` would be true only of a pipeline rooted
/// in `Live::table`.
#[tokio::test]
async fn a_opens_the_menu_on_a_page_for_the_focused_panes_row() {
    let pc = MockPc::builder().start().await;
    let mut app = page_from_palette(&pc, "dashboard").await;
    let kind = app
        .page()
        .and_then(|p| p.focused())
        .expect("a pane has the focus")
        .key
        .kind;
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu, "{:?}", app.status);
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("act on"), "{frame}");
    let mut listed = app.menu_actions();
    listed.sort_unstable();
    let mut expected: Vec<&str> = kind
        .action_target()
        .actions
        .iter()
        .filter(|a| !a.hidden)
        .map(|a| a.name)
        .collect();
    expected.sort_unstable();
    assert_eq!(
        listed, expected,
        "the focused pane's kind, not the page's first"
    );
    // And `esc` gives the page back rather than leaving the menu's mode behind.
    app.handle(Key::Esc);
    assert_eq!(app.mode, Mode::Table);
    assert!(app.page().is_some());
}

/// `ctrl-x` on a pane the Prism Central cannot serve has nothing to stop and says so: the pane
/// never subscribed, so there is no walk and no cycle loop behind it. The pane beside it does
/// poll, and there `ctrl-x` stops it.
#[tokio::test]
async fn ctrl_x_on_a_pane_with_nothing_polling_it_says_so() {
    let pc = MockPc::builder()
        .unavailable_namespace("multidomain")
        .start()
        .await;
    let mut app = dr(&pc).await;
    app.handle(Key::Ctrl('x'));
    assert_eq!(app.status.as_deref(), Some("nothing to stop"));
    let frame = app.snapshot(120, 40).unwrap();
    assert!(!frame.contains("⏹ stopped"), "nothing was stopped: {frame}");

    app.handle(Key::Tab);
    app.handle(Key::Ctrl('x'));
    assert_eq!(app.status.as_deref(), Some("stopped"));
    let frame = app.snapshot(120, 40).unwrap();
    assert!(frame.contains("⏹ stopped"), "{frame}");
}

/// The Attention page: four panes over the kinds the dashboard already reads, and a summary
/// that counts what they hold the way their filters mean it. The mock answers a `$filter`
/// with the whole list, so the pane counts are the fixtures' and the summary's are the
/// narrowed ones - which is what makes the two numbers worth checking against each other.
#[tokio::test]
async fn the_attention_page_counts_what_needs_a_look() {
    let pc = MockPc::builder().start().await;
    let mut app = page_from_palette(&pc, "attention").await;
    common::settle_sample(&mut app).await;
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    let frame = app.snapshot(120, 40).unwrap();
    for title in [
        "Critical and warning alerts",
        "Failed tasks",
        "VMs powered off",
        "Hosts",
        "Summary",
    ] {
        assert!(frame.contains(title), "{title} is missing:\n{frame}");
    }
    assert!(frame.contains("Dashboard › Attention"), "{frame}");
    let summary = |label: &str| {
        frame
            .lines()
            // The summary line, not a pane title that shares the words.
            .find(|l| l.contains(label) && !l.contains('╭'))
            // The value sits against the box's right border.
            .map(|l| {
                l.trim_end_matches('│')
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_else(|| panic!("no {label} line: {frame}"))
    };
    assert_eq!(summary("Critical alerts"), "1");
    assert_eq!(summary("Warning alerts"), "1");
    assert_eq!(
        summary("Failed tasks"),
        "1",
        "one of the seven tasks failed"
    );
    assert_eq!(summary("VMs powered off"), "1", "web-02");
    assert_eq!(summary("Hosts not normal"), "0");
    assert!(
        frame.contains("Delete VM"),
        "the failed task's row: {frame}"
    );
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("attention_page", frame));
}

/// A source namespace this Prism Central does not serve: the pane says so, the summary line
/// that would have counted it reads `-`, and the other three carry on.
#[tokio::test]
async fn an_unserved_source_is_said_not_silently_dropped() {
    let pc = MockPc::builder()
        .unavailable_namespace("prism")
        .start()
        .await;
    let mut app = page_from_palette(&pc, "attention").await;
    common::settle_names(&mut app).await;
    let frame = app.snapshot(120, 40).unwrap();
    assert!(frame.contains("namespace not served"), "{frame}");
    let failed = frame
        .lines()
        .find(|l| l.contains("Failed tasks") && !l.contains("╭"))
        .unwrap_or_else(|| panic!("{frame}"));
    assert!(
        failed.trim_end_matches('│').trim_end().ends_with('-'),
        "counts nothing it could not ask: {failed}"
    );
    assert!(frame.contains("web-02"), "the others polled: {frame}");
}

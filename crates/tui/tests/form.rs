mod common;

use nutsh_mockpc::MockPc;
use nutsh_tui::app::Mode;
use nutsh_tui::{App, Key};

async fn app(pc: &MockPc) -> App {
    let session = common::session(pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "vm",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle(Key::Char(c));
    }
}

/// "for update VM, would be nice to autofill what is already present." An update edits what is
/// there; a form that starts blank invites clearing a field nobody meant to touch, and every
/// untouched field would be sent as an absence.
#[tokio::test]
async fn an_update_form_starts_from_what_the_vm_already_has() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    assert_eq!(app.selected_name(), Some("web-01"));
    app.handle(Key::Char('a'));
    assert_eq!(app.mode, Mode::Menu);
    type_str(&mut app, "update");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields, "update asks for a body");
    let form = app.form.as_ref().expect("the update form");
    assert_eq!(form.value("name"), "web-01");
    assert_eq!(
        form.value("memorySizeBytes"),
        "8589934592",
        "and a number is the digits it was"
    );
    assert_eq!(form.value("description"), "frontend");
    assert_eq!(form.value("numSockets"), "2");
    // A field the VM has `null` for stays empty rather than being invented.
    assert_eq!(form.value("biosUuid"), "");
    let frame = app.snapshot(120, 30).unwrap();
    assert!(frame.contains("web-01"), "{frame}");

    // A clone is a new VM, not an edit of this one: its form starts where it always did.
    app.handle(Key::Esc);
    app.handle(Key::Char('C'));
    assert_eq!(app.mode, Mode::Fields);
    assert_eq!(
        app.form.as_ref().expect("the clone form").value("name"),
        "",
        "a POST that makes something new starts empty"
    );
}

/// An update form prefills from a narrowed row, and sends nothing it could not read off it.
///
/// `vmm.ahv.config.Vm` declares a `$select` of ten fields and its update form has thirty-eight,
/// so most of them are not in the list row at all. The form seeded each of those from its own
/// type - `[{}]` for `disks`, `{"$objectType": null}` for `bootConfig`, the first option for
/// `machineType` - and `build_body` sends whatever a field holds, so a `PUT` nobody typed a
/// character into replaced the VM's disks with one empty object.
///
/// `Client::update` merges the form's body over a fresh `GET`, so a field the body leaves out
/// keeps whatever the Prism Central has. Leaving it out is the whole of "do not touch it".
#[tokio::test]
async fn an_update_form_sends_nothing_it_could_not_read_off_the_vm() {
    let pc = MockPc::builder().narrows().start().await;
    let mut app = app(&pc).await;
    assert_eq!(app.selected_name(), Some("web-01"));
    // The cursor and nothing else: no detail has been opened, so the store holds the narrowed
    // list row alone and there is no whole document to prefill from.
    app.handle(Key::Char('a'));
    type_str(&mut app, "update");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields, "update asks for a body");
    {
        let form = app.form.as_ref().expect("the update form");
        assert_eq!(form.value("name"), "web-01", "inside the $select");
        assert_eq!(form.value("memorySizeBytes"), "8589934592");
        for outside in ["disks", "cdRoms", "bootConfig", "machineType", "categories"] {
            assert_eq!(
                form.value(outside),
                "",
                "{outside} is not in the list row, so the form has nothing to offer for it"
            );
        }
    }

    app.handle(Key::Enter);
    common::settle_acted(&mut app).await;
    let sent: Vec<_> = pc
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .collect();
    assert_eq!(sent.len(), 1, "one update went out");
    let body = sent[0].body.as_ref().expect("a body");
    assert_eq!(
        body["name"], "web-01",
        "an untouched field goes back as it came"
    );
    assert_eq!(
        body["disks"][0]["backingInfo"]["diskSizeBytes"], 53_687_091_200u64,
        "the VM keeps the fifty gigabytes it had, rather than one empty object: {body}"
    );
    assert!(
        body.get("bootConfig").is_none(),
        "and a field the VM has none of gets no skeleton: {body}"
    );
}

/// The other half of the same fix. A detail pane fetches the whole document, and `Table::row`
/// prefers that over the narrowed list row beside it - `rows.get`, which the form used, saw
/// only the list row and so threw the fetched document away.
#[tokio::test]
async fn an_update_form_after_a_detail_starts_from_the_whole_document() {
    let pc = MockPc::builder().narrows().start().await;
    let mut app = app(&pc).await;
    // Open the detail on the cursor's row: that is the `GET` by ext id, and it is not narrowed.
    app.handle(Key::Char('y'));
    common::settle(&mut app).await;
    app.handle(Key::Esc);
    // And one more list cycle over it, which replaces the row map wholesale with narrowed rows
    // again. The whole document survives that - on `Table::whole`, where only `row` looks.
    app.handle(Key::Ctrl('r'));
    common::settle(&mut app).await;

    app.handle(Key::Char('a'));
    type_str(&mut app, "update");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields);
    let form = app.form.as_ref().expect("the update form");
    assert!(
        form.value("disks").contains("53687091200"),
        "the fifty gigabytes the whole document has: {}",
        form.value("disks")
    );
    assert_eq!(
        form.value("machineType"),
        "PC",
        "and a field outside the $select the list row could not have offered"
    );
}

#[tokio::test]
async fn clone_opens_a_form_and_submits_the_body_it_built() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    // The header's counters and the names behind the `CLUSTER` and `HOST` cells are part of the
    // frame - and, since a `Reference` is `Cap(18)`, so are the widths of the columns beside
    // them - so the snapshot settles both pollers rather than racing them, as every other
    // snapshot test does.
    common::settle_stats(&mut app).await;
    common::settle_names(&mut app).await;
    app.handle(Key::Char('C'));
    assert_eq!(app.mode, Mode::Fields);
    let settings = common::settings(&pc);
    settings.bind(|| insta::assert_snapshot!("clone_form", app.snapshot(120, 14).unwrap()));

    // A required field left empty is refused, and nothing is sent.
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields);
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("New name is required"), "{frame}");
    assert!(pc.requests_to("/$actions/clone").is_empty());

    type_str(&mut app, "web-03");
    app.handle(Key::Enter);
    common::settle_acted(&mut app).await;
    let sent = pc.requests_to("/$actions/clone");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].body.as_ref().unwrap()["name"], "web-03");
}

#[tokio::test]
async fn a_hidden_field_is_filled_and_never_shown() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('s')); // create recovery point
    assert_eq!(app.mode, Mode::Fields);
    let frame = app.snapshot(120, 14).unwrap();
    assert!(frame.contains("Name"), "{frame}");
    assert!(frame.contains("Expires"), "{frame}");
    assert!(!frame.contains("vmRecoveryPoints"), "hidden: {frame}");
    type_str(&mut app, "rp-1");
    app.handle(Key::Enter);
    common::settle_acted(&mut app).await;
    // The version probe GETs this same list path while connecting, so the create is named by
    // its method rather than by being the first request to the path.
    let sent: Vec<_> = pc
        .requests_to("/config/recovery-points")
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(sent.len(), 1);
    let body = sent[0].body.as_ref().unwrap();
    assert_eq!(body["name"], "rp-1");
    assert_eq!(
        body["vmRecoveryPoints"][0]["vmExtId"],
        "3d0c4a2e-1b8f-4c1a-9e2f-000000000001"
    );
    // 2026-09-05T10:00:00Z plus thirty days, against the test's fixed clock.
    assert_eq!(body["expirationTime"], "2026-10-05T10:00:00Z");
}

/// A form with more fields than the box has rows is windowed around the cursor rather than
/// cut at the bottom: `create` on a VM has thirty-eight of them against a body of about
/// eleven rows, and `tab` walks the cursor well past what an unwindowed box would ever draw.
#[tokio::test]
async fn a_form_taller_than_the_body_windows_around_the_cursor() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('a'));
    type_str(&mut app, "create");
    // The menu filters on titles, and "Create recovery point" holds the word too.
    while app.menu_selected_action() != Some("create") {
        app.handle(Key::Down);
    }
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields);
    let frame = app.snapshot(120, 20).unwrap();
    assert!(frame.contains("▸ Name"), "{frame}");
    assert!(
        !frame.contains("Pcie devices"),
        "the tail is below: {frame}"
    );
    for _ in 0..60 {
        app.handle(Key::Tab);
    }
    let frame = app.snapshot(120, 20).unwrap();
    assert!(
        frame.contains("▸ Pcie devices"),
        "the last of the thirty-eight is drawn where the cursor is: {frame}"
    );
    assert!(!frame.contains("▸ Name"), "and the first has scrolled off");
}

/// The bulk half of the same fix. `prefill` refused a marked bulk outright, and refusing said
/// "not an edit" - which put `Form::over` back on the JSON skeletons and the first enum option,
/// over entities that already exist, on a `PUT` that merges what it is handed. Two VMs marked
/// was two VMs losing their disks to one empty object: the loss the single-row fix was for, one
/// keystroke further along. An empty prefill says both things at once - these rows exist, and
/// this form knows nothing about any of them.
#[tokio::test]
async fn a_bulk_update_form_invents_nothing_over_the_rows_it_is_for() {
    let pc = MockPc::builder().narrows().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(' ')); // web-01, and the cursor moves on
    app.handle(Key::Char(' ')); // web-02
    assert_eq!(app.marks().len(), 2);
    app.handle(Key::Char('a'));
    type_str(&mut app, "update");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields, "update asks for a body");
    {
        let form = app.form.as_ref().expect("the update form");
        // Nothing invented, and nothing borrowed from the cursor's row either: the marked rows
        // are not the cursor's row, and `db-01`'s values are nobody's but `db-01`'s.
        for blank in [
            "name",
            "description",
            "memorySizeBytes",
            "disks",
            "cdRoms",
            "bootConfig",
            "machineType",
            "categories",
        ] {
            assert_eq!(
                form.value(blank),
                "",
                "{blank} is seeded from nothing on a bulk"
            );
        }
    }

    // A form that knows nothing has nothing to send. It says so, per row, rather than putting a
    // skeleton over two live VMs.
    app.handle(Key::Enter);
    common::settle_acted(&mut app).await;
    common::settle_acted(&mut app).await;
    assert!(
        pc.requests().iter().all(|r| r.method != "PUT"),
        "an empty bulk update sends no PUT at all"
    );
    let said = app.status.clone().unwrap_or_default();
    assert!(said.contains("needs a request body"), "{said}");

    // And the one field the user did fill reaches both rows, while everything they did not
    // touch comes back off each row's own `GET` inside `Client::update`.
    app.handle(Key::Up);
    app.handle(Key::Up);
    assert_eq!(app.selected_name(), Some("web-01"), "back to the top");
    app.handle(Key::Char(' '));
    app.handle(Key::Char(' '));
    assert_eq!(app.marks().len(), 2);
    app.handle(Key::Char('a'));
    type_str(&mut app, "update");
    app.handle(Key::Enter);
    app.handle(Key::Down); // off `name`, onto `description`
    type_str(&mut app, "fleet-a");
    app.handle(Key::Enter);
    common::settle_acted(&mut app).await;
    common::settle_acted(&mut app).await;
    let sent: Vec<_> = pc
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT")
        .collect();
    assert_eq!(sent.len(), 2, "one update per marked row");
    assert_ne!(
        sent[0].path, sent[1].path,
        "two rows, two paths - not one row updated twice"
    );
    for r in &sent {
        let body = r.body.as_ref().expect("a body");
        assert_eq!(
            body["description"], "fleet-a",
            "the one field the user filled: {body}"
        );
        // Each row's own disk, off its own `GET` - not one `[{}]` cloned onto both. The sizes
        // differ between these two VMs, which is the point: nothing here came from the form.
        let disk = &body["disks"][0];
        assert!(
            disk["extId"].is_string(),
            "each marked VM keeps its own disk rather than one empty object: {body}"
        );
        assert!(
            disk["backingInfo"]["diskSizeBytes"]
                .as_u64()
                .is_some_and(|n| n > 0),
            "at the size the Prism Central has for it: {body}"
        );
        assert!(
            body.get("bootConfig").is_none(),
            "and none of them gets a skeleton: {body}"
        );
    }
}

/// A bulk gets one body per row, not one body cloned onto every row: `$ext_id` is the row the
/// request is for. Two VMs marked and one `s` is two recovery points of two different VMs -
/// and the cursor is on neither of them, which is what `subjects()` means by marks.
#[tokio::test]
async fn a_marked_bulk_builds_one_body_per_row() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char(' ')); // web-01, and the cursor moves on
    app.handle(Key::Char(' ')); // web-02
    assert_eq!(app.marks().len(), 2);
    assert_eq!(
        app.selected_name(),
        Some("db-01"),
        "the cursor is on neither"
    );
    app.handle(Key::Char('s'));
    assert_eq!(app.mode, Mode::Fields);
    type_str(&mut app, "rp-bulk");
    app.handle(Key::Enter);
    common::settle_acted(&mut app).await;
    common::settle_acted(&mut app).await;
    let sent: Vec<_> = pc
        .requests_to("/config/recovery-points")
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(sent.len(), 2);
    let of = |i: usize| -> String {
        sent[i].body.as_ref().unwrap()["vmRecoveryPoints"][0]["vmExtId"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(of(0), "3d0c4a2e-1b8f-4c1a-9e2f-000000000001");
    assert_eq!(of(1), "3d0c4a2e-1b8f-4c1a-9e2f-000000000002");
    assert!(
        !of(1).ends_with("000000000003"),
        "and never the cursor's, which is db-01"
    );
}

/// `acknowledge` and `resolve` are one endpoint told apart by the constant body the catalog
/// curated for each. Both also carry the generated form over that endpoint's one enum field,
/// which must not be opened: it would seed on the first variant - `RESOLVE` - and `A` would
/// resolve the alert on the first `enter`, with no confirm on the way.
#[tokio::test]
async fn a_curated_constant_body_is_sent_and_never_asked_for() {
    let pc = MockPc::builder().start().await;
    let session = common::session(&pc).await;
    let mut app = App::open(
        session,
        Box::new(common::NoContexts),
        "alerts",
        common::config(),
    )
    .unwrap();
    common::settle(&mut app).await;
    app.handle(Key::Char('A'));
    assert_eq!(app.mode, Mode::Table, "no form and no confirm stands open");
    common::settle_acted(&mut app).await;
    app.handle(Key::Char('R'));
    common::settle_acted(&mut app).await;
    let sent: Vec<_> = pc
        .requests_to("/$actions/manage-alert")
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].body.as_ref().unwrap()["actionType"], "ACKNOWLEDGE");
    assert_eq!(sent[1].body.as_ref().unwrap()["actionType"], "RESOLVE");
}

/// `enter` on a `Reference` field lists the kind it names rather than submitting, and the row
/// it chooses reaches the body as an extId. Nothing has listed clusters yet, so this exercises
/// the one-shot list too: the box opens empty and fills in.
#[tokio::test]
async fn a_reference_field_picks_a_row_and_sends_its_ext_id() {
    let pc = MockPc::builder().start().await;
    let mut app = app(&pc).await;
    app.handle(Key::Char('a'));
    type_str(&mut app, "another cluster");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields);
    // `targetAvailabilityZone` is a reference the generator could not resolve to a kind, so it
    // is typed; `targetCluster` resolved, so `enter` on it lists the clusters.
    type_str(&mut app, "az-1");
    app.handle(Key::Tab);
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields, "the picker is a modal of the form");
    tokio::time::timeout(std::time::Duration::from_secs(5), app.settle_ref_rows())
        .await
        .expect("the one-shot list lands");
    let frame = app.snapshot(120, 20).unwrap();
    assert!(frame.contains("choose"), "{frame}");
    assert!(frame.contains("lab-cluster"), "{frame}");
    app.handle(Key::Enter); // the cluster under the cursor
    app.handle(Key::Tab); // isLiveMigration
    app.handle(Key::Char(' '));
    app.handle(Key::Enter); // submit
    // Medium danger: `migrate` asks before it sends.
    assert_eq!(app.mode, Mode::Confirm);
    app.handle(Key::Char('y'));
    common::settle_acted(&mut app).await;
    let sent = pc.requests_to("/$actions/migrate");
    assert_eq!(sent.len(), 1);
    let body = sent[0].body.as_ref().unwrap();
    // Inside the reference object, not in place of it: `ClusterReference` is `type: object,
    // required: [extId], additionalProperties: false`, so a bare string here is a wrong body -
    // and the object is the shape this pins.
    assert_eq!(
        body["targetCluster"]["extId"],
        "0006158a-2f0d-4d5a-8e2d-000000000010"
    );
    assert_eq!(body["targetAvailabilityZone"]["extId"], "az-1");
    assert_eq!(body["isLiveMigration"], true);
}

#[test]
fn enums_cycle_bools_toggle_and_integers_reject_letters() {
    use nutsh_catalog::{Field, FieldType};
    use nutsh_tui::form::Form;

    let fields: &[Field] = &[
        Field {
            name: "mode",
            label: "Mode",
            ty: FieldType::Enum(&["FAST", "FULL"]),
            required: false,
            value: None,
            hidden: false,
        },
        Field {
            name: "on",
            label: "On",
            ty: FieldType::Bool,
            required: false,
            value: None,
            hidden: false,
        },
        Field {
            name: "n",
            label: "N",
            ty: FieldType::Integer,
            required: false,
            value: None,
            hidden: false,
        },
    ];
    let mut form = Form::open("clone", fields);
    form.key(Key::Right);
    assert_eq!(form.value("mode"), "FULL");
    form.key(Key::Right);
    assert_eq!(form.value("mode"), "FAST", "and wraps");
    form.key(Key::Tab);
    form.key(Key::Char(' '));
    assert_eq!(form.value("on"), "true");
    form.key(Key::Tab);
    form.key(Key::Char('4'));
    form.key(Key::Char('x'));
    form.key(Key::Char('2'));
    assert_eq!(form.value("n"), "42", "non-digits are refused on input");

    // Neither a checkbox nor an enum is typed into: both are read back as whole words, so one
    // stray letter is a box that silently unticks and a variant Prism Central does not have.
    let mut form = Form::open("clone", fields);
    assert_eq!(form.key(Key::Char('x')), nutsh_tui::form::Event::Ignored);
    assert_eq!(form.key(Key::Backspace), nutsh_tui::form::Event::Ignored);
    assert_eq!(form.value("mode"), "FAST", "an enum is left alone");
    form.key(Key::Tab);
    form.key(Key::Char(' '));
    assert_eq!(form.key(Key::Char('x')), nutsh_tui::form::Event::Ignored);
    assert_eq!(form.key(Key::Backspace), nutsh_tui::form::Event::Ignored);
    assert_eq!(form.value("on"), "true", "a checkbox is left alone");
}

/// The generator carries `$UNKNOWN` and `$REDACTED` because the v4 schemas declare them, but
/// they are what an enum is *answered* with, never something to send: cycling must not reach
/// them, and the seed must not start on one.
#[test]
fn the_generators_enum_sentinels_are_not_offered() {
    use nutsh_catalog::{Field, FieldType};
    use nutsh_tui::form::Form;

    let fields: &[Field] = &[Field {
        name: "actionType",
        label: "Action type",
        ty: FieldType::Enum(&["RESOLVE", "ACKNOWLEDGE", "$UNKNOWN", "$REDACTED"]),
        required: true,
        value: None,
        hidden: false,
    }];
    let mut form = Form::open("Manage alert", fields);
    assert_eq!(form.value("actionType"), "RESOLVE");
    form.key(Key::Right);
    assert_eq!(form.value("actionType"), "ACKNOWLEDGE");
    form.key(Key::Right);
    assert_eq!(form.value("actionType"), "RESOLVE", "and wraps past both");
    form.key(Key::Left);
    assert_eq!(form.value("actionType"), "ACKNOWLEDGE");
}

/// A form whose one visible field is a reference has to be submittable: `migrate-to-host` and
/// `restore` are exactly that shape, and `enter` reopening the picker for ever would leave
/// them with no key that sends.
#[test]
fn a_chosen_reference_submits_and_space_reopens_the_picker() {
    use nutsh_catalog::{Field, FieldType};
    use nutsh_tui::form::{Event, Form};

    let fields: &[Field] = &[Field {
        name: "host",
        label: "Host",
        ty: FieldType::Reference(Some("clustermgmt.config.Host")),
        required: true,
        value: None,
        hidden: false,
    }];
    let mut form = Form::open("Migrate to host", fields);
    assert_eq!(form.key(Key::Enter), Event::Pick("clustermgmt.config.Host"));
    form.set_selected_value("host-1".into());
    assert_eq!(form.key(Key::Enter), Event::Submit, "a chosen one sends");
    assert_eq!(
        form.key(Key::Char(' ')),
        Event::Pick("clustermgmt.config.Host"),
        "and space is the way back to the list"
    );
    // A reference can also be typed by hand, so `backspace` deletes a character rather than
    // the whole value; emptied, `enter` is back to listing.
    for _ in 0.."host-1".len() {
        form.key(Key::Backspace);
    }
    assert_eq!(form.value("host"), "");
    assert_eq!(
        form.key(Key::Enter),
        Event::Pick("clustermgmt.config.Host"),
        "cleared, it lists again"
    );
}

#[test]
fn a_json_field_reports_its_parse_error_under_the_form() {
    use nutsh_catalog::{Field, FieldType};
    use nutsh_tui::form::Form;

    let fields: &[Field] = &[Field {
        name: "disks",
        label: "Disks",
        ty: FieldType::Json("[{\"sizeBytes\": null}]"),
        required: false,
        value: None,
        hidden: false,
    }];
    let mut form = Form::open("clone", fields);
    assert_eq!(form.value("disks"), "[{\"sizeBytes\": null}]", "seeded");
    form.key(Key::Backspace);
    let entered = form.entered();
    let e = nutsh_prism::Entity::new(
        nutsh_catalog::kind("vmm.ahv.config.Vm").unwrap(),
        serde_json::json!({"extId": "x"}),
        None,
    );
    let err = nutsh_core::actions::build_body(fields, &entered, &e, common::now()).unwrap_err();
    assert!(err.contains("Disks"), "{err}");
}

/// The clusters fixture, as `Store::load` keys it: `repeat_fixture` takes the fixture's own
/// path, version and all.
const CLUSTERS: &str = "/clustermgmt/v4.3/config/clusters";

/// A page pane and the table view of its kind share one store entry, and a pane walks only as
/// far as it draws: one cycle of the Dashboard's Clusters pane leaves `TableKey::top(cluster)`
/// holding twenty of sixty clusters with `loading` false. The reference picker must not offer
/// those twenty as though they were every cluster there is - nothing in the box says otherwise
/// - so it compares the rows against the server's total and lists again.
///
/// The refill itself is `a_reference_field_picks_a_row_and_sends_its_ext_id`'s ground; what is
/// pinned here is that the second listing is asked for at all, which `loading` on a table that
/// has completed a cycle cannot ask for.
#[tokio::test]
async fn a_reference_picker_lists_again_when_the_rows_it_finds_are_a_slice() {
    let pc = MockPc::builder().repeat_fixture(CLUSTERS, 60).start().await;
    let mut app = app(&pc).await;
    // The Dashboard, whose Clusters pane is unfiltered and so shares the table view's key.
    app.handle(Key::Char(':'));
    type_str(&mut app, "dashboard");
    app.handle(Key::Enter);
    common::settle_page(&mut app).await;
    // Back onto a VM row, with the pane's twenty clusters left in the store behind it.
    for _ in 0..3 {
        app.handle(Key::Tab);
    }
    app.handle(Key::Char('O'));
    common::settle(&mut app).await;
    assert_eq!(app.view().unwrap().key.kind.id, "vmm.ahv.config.Vm");

    app.handle(Key::Char('a'));
    type_str(&mut app, "another cluster");
    app.handle(Key::Enter);
    assert_eq!(app.mode, Mode::Fields);
    type_str(&mut app, "az-1");
    app.handle(Key::Tab);
    app.handle(Key::Enter);
    let form = app.form.as_ref().expect("the migrate form");
    let picker = form.picker.as_ref().expect("the cluster picker");
    assert_eq!(
        picker.entries.len(),
        20,
        "the pane's budget is what the store holds"
    );
    assert!(
        form.loading,
        "twenty of sixty is a slice: the picker lists again rather than presenting it as the \
         whole collection"
    );
}

/// A text field takes a cursor; an enum and a checkbox keep `←`/`→` for cycling, so the two do
/// not fight. `form.rs`'s fall-through to `Ignored` for every non-enum field type becomes the
/// delegation to `Input::key`, which means `Enum` and `Bool` behave exactly as they did.
#[test]
fn a_text_field_takes_a_cursor_and_an_enum_keeps_its_arrows() {
    use nutsh_catalog::{Field, FieldType};
    use nutsh_tui::form::{Event, Form};

    let fields: &[Field] = &[
        Field {
            name: "name",
            label: "Name",
            ty: FieldType::Text,
            required: true,
            value: None,
            hidden: false,
        },
        Field {
            name: "mode",
            label: "Mode",
            ty: FieldType::Enum(&["FAST", "FULL"]),
            required: false,
            value: None,
            hidden: false,
        },
    ];
    let mut form = Form::open("clone", fields);
    for c in "web-01".chars() {
        form.key(Key::Char(c));
    }
    assert_eq!(form.cursor(), 6);
    assert_eq!(form.key(Key::Left), Event::Moved);
    assert_eq!(form.key(Key::Left), Event::Moved);
    assert_eq!(form.cursor(), 4);
    form.key(Key::Char('X'));
    assert_eq!(form.value("name"), "web-X01");
    assert_eq!(form.key(Key::Ctrl('w')), Event::Edited);
    assert_eq!(form.value("name"), "01", "the word before the cursor");
    form.key(Key::End);
    assert_eq!(form.key(Key::Ctrl('u')), Event::Edited);
    assert_eq!(form.value("name"), "");
    assert_eq!(form.key(Key::Left), Event::Ignored, "nothing to the left");

    // The enum is untouched by all of it: `←`/`→` still cycle, and nothing types into it.
    form.key(Key::Tab);
    assert_eq!(form.key(Key::Right), Event::Edited);
    assert_eq!(form.value("mode"), "FULL");
    assert_eq!(form.key(Key::Ctrl('w')), Event::Ignored, "not typed into");
    assert_eq!(form.key(Key::Home), Event::Ignored, "nor moved through");
}

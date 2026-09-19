//! The action menu and the run: refusal, confirmation, and the wire.

use super::*;

impl App {
    /// The rows an action applies to: the marks in table order, or the row under the cursor.
    ///
    /// In the order the *view* shows, filter included, so an action runs on rows that are on
    /// the frame. A mark made before the term was narrowed is therefore left out - and the
    /// confirm dialog names every row it is about to send to, so what was left out is read
    /// before anything is sent rather than discovered afterwards.
    pub(super) fn subjects(&self) -> Vec<(String, String)> {
        let Some(live) = self.live.as_ref() else {
            return Vec::new();
        };
        let Some(cursor) = self.cursor_table() else {
            return Vec::new();
        };
        let table = live.merged(cursor.key);
        let ordered = crate::table::order(
            &table,
            &cursor.columns,
            live.store.names(),
            self.now,
            cursor.sort,
            cursor.filter,
        );
        let wanted: Vec<&str> = match cursor.marks.filter(|m| !m.is_empty()) {
            None => ordered.get(cursor.selected).copied().into_iter().collect(),
            Some(marks) => ordered
                .into_iter()
                .filter(|id| marks.contains(*id))
                .collect(),
        };
        wanted
            .into_iter()
            .filter_map(|id| {
                // The merged row id, not `e.ext_id`: it says which context the row is from.
                table.rows.get(id).map(|e| (id.to_string(), e.name.clone()))
            })
            .collect()
    }

    /// Every action that runs on the row under the cursor, in the order every surface lists
    /// them, each greyed by the one reason string.
    ///
    /// **The one list.** The `a` menu, the `⏎` picker's actions group and the detail pane's
    /// actions section all draw it, so the three can never drift apart - and the permission
    /// logic is written once, here.
    ///
    /// The two surfaces that say they are showing what can be done to *this row* take
    /// [`App::entity_action_rows`] instead; this one is the whole set, collection-level
    /// `create` included, because the menu is where creating a thing has to stay reachable.
    pub(super) fn action_rows(&self) -> Vec<menu::Row> {
        let Some(cursor) = self.cursor_table() else {
            return Vec::new();
        };
        // Once, not once per row: `subjects` orders the whole table and `selected_entity`
        // finds the cursor's row in it, and the list asks the same question of every action
        // the kind has.
        let count = self.subjects().len().max(1);
        let entity = self.selected_entity();
        let mut rows: Vec<menu::Row> = cursor
            .key
            .kind
            .action_target()
            .actions
            .iter()
            .filter(|a| !a.hidden)
            .map(|action| menu::Row {
                action,
                reason: self.refusal_for(action, count, entity),
            })
            .collect();
        menu::sort(&mut rows);
        rows
    }

    /// The part of [`App::action_rows`] that acts on the row under the cursor.
    ///
    /// The `⏎` picker and the detail pane both say, in so many words, that they are showing
    /// what can be done to one entity, so a collection-level operation there would be a lie
    /// about that entity: `create` makes a *new* VM and does nothing to `web-01`. The filter is
    /// the catalog's own [`Action::acts_on_a_row`], so a curated workflow that POSTs to another
    /// kind's collection naming this row - `Create recovery point` - stays where it belongs.
    pub(super) fn entity_action_rows(&self) -> Vec<menu::Row> {
        let mut rows = self.action_rows();
        rows.retain(|row| row.action.acts_on_a_row());
        rows
    }

    /// `a`: every action of the row kind's `action_target`, greyed by the one reason string.
    pub(super) fn open_menu(&mut self) {
        // Before the rows are built, not after: the menu is the discoverable path, and it
        // greys nothing on this account's roles until the four list calls have landed. Started
        // here, they are in flight while the menu is being read, so the reasons are right by
        // the time `enter` is pressed.
        self.start_can_i();
        let Some(kind) = self.cursor_table().map(|c| c.key.kind) else {
            return;
        };
        let subjects = self.subjects();
        let rows = self.action_rows();
        if rows.is_empty() {
            self.status = Some("no actions for this kind".into());
            return;
        }
        let subject = match subjects.as_slice() {
            [(_, name)] => name.clone(),
            // The same wording `start` refuses with, rather than `0 Virtual Machines`: an
            // empty table has no subject, and saying so is what tells the reader why `enter`
            // will do nothing.
            [] => "no row selected".into(),
            many => format!("{} {}", many.len(), kind.display),
        };
        self.menu = Some(Menu::open(subject, rows));
        self.mode = Mode::Menu;
    }

    /// The single string the menu, a direct key and `:can-i` all render: why this action cannot
    /// run, or `None` when it can. `count` is the number of rows it would run on, so the bulk
    /// cap is answered by the same call.
    pub(super) fn refusal(&self, action: &'static Action, count: usize) -> Option<String> {
        self.refusal_for(action, count, self.selected_entity())
    }

    /// [`App::refusal`] with the cursor's row already in hand too, for a caller asking about
    /// every action of one kind: resolving it orders the whole table, and the menu would
    /// otherwise order it once per action.
    pub(super) fn refusal_for(
        &self,
        action: &'static Action,
        count: usize,
        entity: Option<&nutsh_prism::Entity>,
    ) -> Option<String> {
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        // A row on a joined context is judged by that context's session - its read-only
        // flag, its name in the guardrails. Its roles are not resolved: nothing is greyed
        // on another account's answer, and the Prism Central refuses for itself.
        let site = self
            .selected_ext_id()
            .and_then(|id| Live::split_row_id(&id).0.map(str::to_string))
            .or_else(|| cursor.key.context.as_deref().map(str::to_string));
        let unknown = nutsh_core::can_i::CanIndex::unknown("not resolved for a joined context");
        let own = live.can_i.get();
        let can_i = if site.is_some() { &unknown } else { &own };
        nutsh_core::actions::reason(
            Policy {
                session: live.session_for(site.as_deref()),
                guardrails: &self.guardrails,
                can_i,
            },
            cursor.key.kind,
            action,
            &cursor.key.parents,
            entity,
            count,
        )
    }

    /// The row under the cursor, in whichever view has it: the top table, or the focused pane
    /// of the open page.
    pub(super) fn selected_entity(&self) -> Option<&nutsh_prism::Entity> {
        let live = self.live.as_ref()?;
        let cursor = self.cursor_table()?;
        let id = self.selected_ext_id()?;
        live.find_row(cursor.key, &id).map(|(_, e)| e)
    }

    /// Which joined context the pending action's rows come from, when there are peers and the
    /// rows all come from one: what the confirm names.
    fn pending_site(&self) -> Option<String> {
        let live = self.live.as_ref()?;
        if live.peers.is_empty() {
            return None;
        }
        let pending = self.pending.as_ref()?;
        let key = self.cursor_table()?.key.clone();
        let mut sites: Vec<String> = pending
            .rows
            .iter()
            .filter_map(|(id, _)| live.find_row(&key, id))
            .map(|(ctx, _)| ctx.map_or_else(|| live.primary_name(), str::to_string))
            .collect();
        sites.dedup();
        match sites.as_slice() {
            [one] => Some(one.clone()),
            _ => None,
        }
    }

    /// The menu's `enter`, and every curated key.
    pub(super) fn start(&mut self, action: &'static Action) {
        // Resolution is lazy: it starts the first time the menu, a curated key or `:can-i`
        // needs it, reporting `Unknown` until it lands.
        self.start_can_i();
        let subjects = self.subjects();
        // The count the rows will actually be sent with, and it is already in hand.
        if let Some(reason) = self.refusal(action, subjects.len().max(1)) {
            // Every path that could mutate writes a journal entry, including the ones that
            // never reach the network: `:journal` answers "why did nothing happen" too.
            self.journal_refusal(action, &subjects, &reason);
            self.status = Some(reason);
            return;
        }
        if subjects.is_empty() {
            // An empty table, or a cursor on nothing: `reason` is asked with a count of one and
            // usually has nothing to say about it, so the key would otherwise do and say
            // nothing at all.
            self.status = Some("no row selected".into());
            return;
        }
        // A curated constant body is the whole meaning of the action. `acknowledge` and
        // `resolve` POST to the same `manage-alert` endpoint and are told apart by nothing but
        // this, and both carry the generated form over that endpoint's one enum field: opening
        // it would let `A` send `RESOLVE` on the first `enter`, with no confirm to catch it.
        let body = match action.body.map(serde_json::from_str).transpose() {
            Ok(body) => body,
            Err(e) => {
                self.status = Some(format!("{}: malformed curated body: {e}", action.title()));
                return;
            }
        };
        let asks = body.is_none() && action.takes_body && !action.form.is_empty();
        // Before `subjects` is moved into the plan, and it is the count the body will be sent
        // with: a prefill from the cursor's row is one row's values and nobody else's.
        let prefill = if asks {
            self.prefill(action, subjects.len())
        } else {
            None
        };
        self.pending = Some(Pending {
            action,
            rows: subjects,
            body,
            entered: None,
        });
        // A body first, a confirm after: what the user is confirming has to be what will be
        // sent.
        if asks {
            self.form = Some(Form::over(action.title(), action.form, prefill.as_ref()));
            self.mode = Mode::Fields;
            return;
        }
        self.confirm_or_run();
    }

    /// What an update form starts from: the row's current values, by field name.
    ///
    /// `PUT` and `PATCH` only. Those two replace or amend the entity at the path, so a field
    /// left blank is a field cleared, and starting from the current document is what makes the
    /// form an edit rather than a replacement. A `POST` to `$actions/clone` makes something
    /// new, and seeding it with the source's name would offer a duplicate as the default.
    ///
    /// One row only. A bulk action sends one body to every marked row, and the cursor's values
    /// are nobody's but the cursor's - prefilling there would quietly copy one row over the
    /// rest. It is still an edit, though, and that is the distinction the empty map carries:
    /// marked rows exist, so nothing may be invented over them either.
    ///
    /// `None` is "this is not an edit", which is a different thing from an edit that found no
    /// values: [`Form::over`] invents nothing over an existing entity and seeds a blank form
    /// as it always did, and this is what tells the two apart. Possibly-empty `Some` is the
    /// honest answer for a `PUT` whose row the store cannot produce.
    pub(super) fn prefill(
        &self,
        action: &'static Action,
        rows: usize,
    ) -> Option<std::collections::HashMap<&'static str, String>> {
        if !matches!(action.method, Method::Put | Method::Patch) {
            return None;
        }
        // Empty rather than `None`, and the difference is a VM's disks. `None` says "not an
        // edit", which puts `Form::over` back on the JSON skeletons and the first enum option
        // - over entities that already exist, on a `PUT` that merges what it is handed. One
        // `[{}]` for `disks` sent to every marked row is the same loss the single-row path was
        // fixed for.
        if rows > 1 {
            return Some(std::collections::HashMap::new());
        }
        Some(
            self.selected_entity()
                .map(|e| nutsh_core::actions::current_values(action.form, e))
                .unwrap_or_default(),
        )
    }

    /// Resolve can-i in the background, once. Connect already probes twenty namespaces; four
    /// more list calls on every start would delay the first frame to buy an answer that never
    /// hides anything.
    pub(super) fn start_can_i(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        // `claim` hands out the generation this resolution belongs to; `set` drops a result
        // whose generation an `invalidate` has since retired, so a 403 that lands while the
        // four lists are still in flight is never overwritten by the answer it disproved.
        let Some(generation) = live.can_i.claim() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            live.can_i.set(
                generation,
                nutsh_core::can_i::CanIndex::unknown("no async runtime"),
            );
            return;
        };
        let cell = live.can_i.clone();
        let client = live.session.client.clone();
        let username = live.session.username.clone();
        runtime.spawn(async move {
            cell.set(
                generation,
                nutsh_core::can_i::resolve(&client, &username).await,
            );
        });
    }

    /// Every borrow is resolved into an owned value before anything mutates `self`: the
    /// verdict needs the session and the pending action, and both arms need `&mut self`.
    pub(super) fn confirm_or_run(&mut self) {
        let Some((action, names)) = self.pending.as_ref().map(|p| {
            (
                p.action,
                p.rows
                    .iter()
                    .map(|(_, n)| n.clone())
                    .collect::<Vec<String>>(),
            )
        }) else {
            return;
        };
        // Asked again, not carried from `start`: a form can sit open while the
        // background can-i resolution lands, and a refusal that arrives in between must not be
        // able to become a send. The count is the pending rows', which is what will be sent.
        if let Some(reason) = self.refusal(action, names.len().max(1)) {
            let rows = self.pending.take().map(|p| p.rows).unwrap_or_default();
            self.journal_refusal(action, &rows, &reason);
            self.status = Some(reason);
            self.confirm = None;
            self.menu = None;
            self.mode = self.acting_from();
            return;
        }
        let Some((kind, verdict)) = self.live.as_ref().and_then(|live| {
            let kind = self.cursor_table()?.key.kind;
            let context = live.session.context.as_deref().unwrap_or("(env)");
            Some((
                kind,
                self.guardrails.verdict(
                    context,
                    kind,
                    action,
                    names.len(),
                    &live.can_i.get().can(action),
                ),
            ))
        }) else {
            return;
        };
        match verdict {
            Verdict::Confirm(strength) => {
                let site = self.pending_site();
                self.confirm = Some(Confirm::open(
                    strength,
                    action.title(),
                    kind.display,
                    &names,
                    site.as_deref(),
                ));
                self.mode = Mode::Confirm;
            }
            Verdict::Allow => self.run_pending(),
            // Unreachable: the refusal above answered every one of these and returned. Spelled
            // out rather than left to a `_ => self.run_pending()`, so a verdict added later
            // cannot become a send by falling through.
            Verdict::ReadOnly
            | Verdict::Deny(_)
            | Verdict::NotPermitted(_)
            | Verdict::TooMany { .. } => {
                self.pending = None;
                self.confirm = None;
                self.menu = None;
                self.mode = self.acting_from();
            }
        }
    }

    /// One request per subject, each with its own journal entry and task watch; a failure does
    /// not stop the rest.
    ///
    /// Sequential in one spawned task rather than one task per row: ten marked rows firing ten
    /// simultaneous DELETEs is a different thing from a bulk delete, the bucket paces the rate
    /// but does not order them, and the `Acted` messages - the journal's settle order and the
    /// status line - would arrive in whatever order the responses did.
    pub(super) fn run_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.confirm = None;
        self.menu = None;
        self.mode = self.acting_from();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.status = Some("no async runtime: cannot act from here".into());
            return;
        };
        // Everything `self` owns and the loop needs, taken before `live` is borrowed mutably -
        // the cursor's table included, since `cursor_table` borrows the stack `live` is in.
        let tx = self.poll_tx.clone();
        let now = self.now;
        let Some(key) = self.cursor_table().map(|c| c.key.clone()) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let kind = key.kind;
        let parents = key.parents.clone();
        let primary = live.primary_name();
        // Planned and journalled here, on the UI thread, so the journal holds the rows in table
        // order whatever the network does with them. Each row goes to the client of the
        // context it was listed from.
        let mut work: Vec<(
            std::sync::Arc<nutsh_prism::Client>,
            nutsh_core::actions::Plan,
            JournalId,
        )> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for (ext_id, _) in &pending.rows {
            let Some((site, entity)) = live
                .find_row(&key, ext_id)
                .map(|(ctx, e)| (ctx.map(str::to_string), e.clone()))
            else {
                continue;
            };
            let context = site.clone().unwrap_or_else(|| primary.clone());
            let client = site
                .as_deref()
                .and_then(|name| live.peers.iter().find(|p| &*p.name == name))
                .map_or_else(|| live.session.client.clone(), |p| p.session.client.clone());
            // Built here, per row, rather than once at submit: `$ext_id` and `$now+30d` are
            // substituted against the row the request is for, so ten marked VMs snapshot
            // themselves rather than the one the cursor happened to be on. A curated constant
            // body has nothing to substitute and is sent to every row as it is.
            let body = match pending.entered.as_ref() {
                Some(entered) => match nutsh_core::actions::build_body(
                    pending.action.form,
                    entered,
                    &entity,
                    now,
                ) {
                    Ok(body) => Some(body),
                    Err(e) => {
                        failures.push(e);
                        continue;
                    }
                },
                None => pending.body.clone(),
            };
            let plan =
                match nutsh_core::actions::plan(kind, &entity, pending.action, &parents, body) {
                    Ok(p) => p,
                    Err(e) => {
                        failures.push(e);
                        continue;
                    }
                };
            let journal = live.journal.record(
                Attempt {
                    context: &context,
                    kind,
                    ext_id: &plan.ext_id,
                    name: &plan.name,
                    action: pending.action.name,
                },
                now,
                JournalOutcome::Started,
            );
            work.push((client, plan, journal));
        }
        let started = work.len();
        if started > 0 {
            // The marks are what this action was gathered for, and they are spent: leaving them
            // set would let the next keypress re-run the same bulk on the same rows. Kept when
            // nothing could be planned, so a refusal leaves the selection to try something else
            // with.
            if let Some(view) = live.table_mut() {
                view.marks.clear();
            }
            runtime.spawn(async move {
                for (client, plan, journal) in work {
                    let result = nutsh_core::actions::execute(&client, &plan).await;
                    let _ = tx
                        .send(Msg::Acted {
                            journal,
                            plan: plan.to_ref(),
                            result: Ok(result),
                        })
                        .await;
                }
            });
        }
        if let Some(first) = failures.first() {
            self.status = Some(first.clone());
        } else if started > 1 {
            self.status = Some(format!("{started} actions started"));
        }
    }

    pub(super) fn handle_menu(&mut self, key: Key) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        match menu.key(key) {
            menu::Event::Filtered | menu::Event::Moved | menu::Event::Ignored => {}
            menu::Event::Cancelled => {
                self.menu = None;
                self.mode = self.acting_from();
            }
            // A greyed row is not refused here: `start` composes the same reason the row is
            // showing, so the menu and a direct key can never disagree about why.
            menu::Event::Chose(action) => self.start(action),
        }
    }

    pub(super) fn handle_confirm(&mut self, key: Key) {
        let Some(confirm) = self.confirm.as_mut() else {
            return;
        };
        match confirm.key(key) {
            confirm::Event::Typed | confirm::Event::Ignored => {}
            confirm::Event::Mismatch => self.status = Some("names do not match".into()),
            confirm::Event::Cancelled => {
                self.confirm = None;
                self.menu = None;
                self.pending = None;
                self.mode = self.acting_from();
            }
            confirm::Event::Run => self.run_pending(),
        }
    }
}

//! The session's life: connecting, the cache under it, and switching context.

use super::*;

impl App {
    /// A connected app on what `kind` names: a feature page, or a kind's table resolved with
    /// the catalog's `lookup`. Two things are errors rather than an empty screen: a term the
    /// catalog does not know, which reports the nearest candidates, and a kind that resolves
    /// but cannot be opened here, which reports `{display}: {reason}` - `NICs: open from
    /// Virtual Machines` for a child kind, `Virtual Machines: namespace not served` for one
    /// this Prism Central lacks.
    pub fn open(
        session: Session,
        contexts: Box<dyn Contexts>,
        kind: &str,
        config: Config,
    ) -> anyhow::Result<App> {
        let home = resolve_home(kind)?;
        let mut app = App::bare(contexts, home, config, Mode::Table);
        app.attach(session);
        // The rows are read once here, as `disconnected` reads them: the palette completes
        // `:ctx ` from them, and must not have to wait for the Contexts screen to be opened.
        app.reload_contexts();
        match app.reopen(home) {
            Open::Opened => Ok(app),
            Open::Greyed(reason) => Err(anyhow::anyhow!("{}: {reason}", home.label())),
        }
    }

    /// An app with no session: the Contexts screen. `message` is what startup found (a connect
    /// that failed, `no context named X`), shown above whatever reading the rows had to say;
    /// `select` puts the cursor on the row that failed.
    ///
    /// The two go together on purpose. Reading the rows is what clears the message, so a
    /// caller that set one and then moved the cursor would have to set it a second time; here
    /// the rows are read once, before either.
    pub fn disconnected(
        contexts: Box<dyn Contexts>,
        kind: &str,
        message: Option<String>,
        select: Option<&str>,
        config: Config,
    ) -> App {
        // An unknown kind is not worth refusing to start over when there is no session yet:
        // the screen is about contexts, and the command line is corrected from the palette.
        //
        // The binary resolves the kind before it gets here, so today nothing reaches the
        // fallback. It stays because this is a public constructor taking a `&str`: the check
        // belongs with the caller that can report it on the command line, and a caller that
        // skips it must still get a screen rather than a panic.
        let home = resolve_home(kind).unwrap_or_else(|_| {
            Home::Kind(nutsh_catalog::kind("vmm.ahv.config.Vm").expect("catalog has VMs"))
        });
        let mut app = App::bare(contexts, home, config, Mode::Contexts);
        app.reload_contexts();
        // Rule 1 alone here: there is no session, so `statuses()` is empty and rule 2 greys
        // and drops nothing. An unknown answer hides nothing, ever.
        app.apply_nav();
        if let Some(name) = select {
            app.screen.select(name);
        }
        // Beside, not over, whatever reading the rows had to say: a config file that could not
        // be read is the more useful half of "could not connect" when both are true.
        app.screen.message = message;
        app
    }

    /// The fields both constructors share; neither the session nor the rows are loaded yet.
    pub(super) fn bare(contexts: Box<dyn Contexts>, home: Home, config: Config, mode: Mode) -> App {
        let (poll_tx, poll_rx) = mpsc::channel(64);
        let (connect_tx, connect_rx) = mpsc::channel(4);
        let one_frame_run = config.snapshot;
        // Read before `contexts` is moved into the struct below.
        let nav_hide = contexts.nav_hidden();
        let nav_hide_unserved = contexts.nav_hide_unserved();
        let refresh_cfg = contexts.refresh();
        App {
            live: None,
            mode,
            status: None,
            dirty: true,
            now: config.now,
            palette: None,
            picker: None,
            detail: None,
            skins: None,
            activity_view: None,
            ladder: None,
            settings: None,
            menu: None,
            confirm: None,
            form: None,
            journal_view: None,
            search: None,
            pending: None,
            guardrails: config.guardrails,
            sidebar: crate::sidebar::Sidebar::default(),
            focus: Focus::Body,
            width: std::cell::Cell::new(REFERENCE_WIDTH),
            hits: std::cell::RefCell::new(crate::mouse::Hits::default()),
            mouse_on: config.mouse,
            header: config.header,
            // The pane's interior in the reference frame: the menu takes 24 cells and the
            // block's own borders two more.
            detail_width: std::cell::Cell::new(REFERENCE_WIDTH - 26),
            one_frame_run,
            screen: Screen::default(),
            contexts,
            config_path: config.config_path,
            help_from: mode,
            palette_from: mode,
            skins_from: mode,
            journal_from: mode,
            activity_from: mode,
            search_from: mode,
            settings_from: mode,
            home,
            poll_tx,
            poll_rx,
            session_generation: 0,
            cache_dir: None,
            history: nutsh_core::history::History::default(),
            cache_written: 0,
            cache_dirty: false,
            stats_seen: false,
            names_seen: false,
            sample_seen: false,
            connect_tx,
            connect_rx,
            pending_peers: Vec::new(),
            joining: Vec::new(),
            last_input: std::time::Instant::now(),
            nav_hide,
            nav_hide_unserved,
            refresh_cfg,
            refresh_session: HashMap::new(),
            settings_open: std::collections::BTreeSet::new(),
            nav_all: false,
            quit: false,
        }
    }

    /// Replace the session: a fresh store and scheduler, the stack emptied, every modal
    /// closed. The modals matter as much as the stack: subscription ids restart with the new
    /// scheduler, so a detail pane left over from the old session would claim the messages of
    /// whichever view took its id.
    pub(crate) fn attach(&mut self, session: Session) {
        // Before anything else: `self.live` is about to be dropped, and with it the store this
        // write reads. The new context's directory is set by `connected`, which knows its name.
        if self.cache_dirty {
            let _ = self.write_cache_now();
        }
        self.cache_dir = None;
        self.cache_dirty = false;
        // With the directory, because the two are one switch: until `enable_cache` names the
        // incoming context's directory this session has a history it never writes down.
        self.history = nutsh_core::history::History::default();
        self.close_modals();
        self.session_generation += 1;
        // The new session's pollers have not reported yet, whatever the old ones' had done.
        self.stats_seen = false;
        self.names_seen = false;
        self.sample_seen = false;
        let scheduler = Scheduler::new(session.client.clone(), self.poll_tx.clone());
        let stats = nutsh_core::stats::spawn(
            session.client.clone(),
            self.poll_tx.clone(),
            self.session_generation,
            session.cluster.as_ref().map(|c| c.ext_id.clone()),
        );
        let names = nutsh_core::names::spawn(
            session.client.clone(),
            self.poll_tx.clone(),
            self.session_generation,
            // A page of every warmed kind a cycle, for a cache nobody is reading a reference
            // out of: the warm-up is not a `Subscription` either, so it consults the flag
            // itself rather than through a task of the scheduler's.
            scheduler.idle_flag(),
        );
        self.live = Some(Live {
            session,
            store: Store::default(),
            scheduler,
            peers: Vec::new(),
            stack: Vec::new(),
            stats,
            names,
            journal: Journal::default(),
            tasks: TaskIndex::default(),
            can_i: CanICell::default(),
            quit_asked: false,
        });
        self.mode = Mode::Table;
        // After the new `Live`, not before: a context switch is somebody being there, and the
        // scheduler the pause is cleared on has to be the one about to poll.
        self.woke();
        // Last, and after the new `Live`: rule 2 reads this session's negotiation, so the set
        // has to be computed against the client that answered it.
        self.apply_nav();
        self.dirty = true;
    }

    /// Give this session a cache directory and paint whatever was in it. Called between
    /// `App::open` and the first frame, so the rows are there before the event loop starts.
    pub fn enable_cache(
        &mut self,
        dir: std::path::PathBuf,
        restored: Option<nutsh_core::cache::Restored>,
    ) {
        // The Prism Central identified itself as a different one: everything restored is
        // wrong, the store stays empty, and the directory goes. The path stays: this session
        // writes its own inventory into the same place.
        let rejected = self
            .live
            .as_ref()
            .is_some_and(|l| l.session.cache == nutsh_core::session::CacheOutcome::Rejected);
        if rejected {
            let _ = nutsh_core::cache::remove(&dir);
        }
        self.cache_dir = Some(dir);
        // The same directory, so one switch governs both and `nutsh cache clear` removes the
        // history with everything else. Nothing has to be flushed first: every accepted line
        // was written when it was accepted.
        self.history = nutsh_core::history::History::load(self.cache_dir.as_deref());
        let Some(r) = restored.filter(|_| !rejected) else {
            return;
        };
        let Some(live) = self.live.as_mut() else {
            return;
        };
        live.store.apply_names(r.names);
        if let Some(s) = r.stats {
            // Drawn stale: a restored count is by construction not a live one, and the stats
            // poller - which the idle pause never touches - replaces it within thirty seconds.
            live.store.set_stats(nutsh_core::stats::Stats {
                clusters: s.clusters,
                hosts: s.hosts,
                vms: s.vms,
                vms_on: s.vms_on,
                alerts_critical: s.alerts_critical,
                alerts_warning: s.alerts_warning,
                tasks_running: s.tasks_running,
                stale: true,
            });
        }
        if let Some(s) = r.sample {
            live.store
                .set_sample(nutsh_core::sampler::ProtectionSample {
                    policies: s.policies,
                    plans: s.plans,
                    recovery_points: s.recovery_points,
                    jobs: s.jobs,
                    sampled: s.sampled,
                    in_sync: s.in_sync,
                    syncing: s.syncing,
                    out_of_sync: s.out_of_sync,
                    available: s.available,
                });
        }
        if let Some(c) = r.can_i.filter(|c| {
            c.username == live.session.username
                && nutsh_core::cache::now_secs().saturating_sub(c.at)
                    <= nutsh_core::cache::CAN_I_TTL
        }) {
            live.can_i
                .restore(nutsh_core::can_i::CanIndex::with_roles(c.roles));
        }
        let mut painted = 0usize;
        for t in r.tables {
            let key = TableKey {
                kind: t.kind,
                parents: t.parents,
                filter: t.filter.map(std::sync::Arc::from),
                context: None,
            };
            let rows: Vec<nutsh_prism::Entity> = t
                .rows
                .into_iter()
                // Through the *current* catalog: a `name_path` curated since the write is
                // honoured instead of a stale derivation being resurrected.
                .map(|v| nutsh_prism::Entity::new(t.kind, v, None))
                .collect();
            painted += live.store.restore(&key, rows, t.total, t.at);
        }
        tracing::debug!(rows = painted, "cache restored");
        self.cache_written = nutsh_core::cache::now_secs();
        self.dirty = true;
    }

    /// Build the snapshot and hand it to a blocking thread. A frame is never blocked on the
    /// filesystem: the clone costs roughly the serialized size - 750 KB for a 100-VM table -
    /// once a minute, which is much cheaper than a dropped frame.
    pub fn write_cache_now(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let (Some(dir), Some(live)) = (self.cache_dir.clone(), self.live.as_ref()) else {
            return None;
        };
        let now = nutsh_core::cache::now_secs();
        let snapshot = cache_snapshot(live, now);
        self.cache_written = now;
        self.cache_dirty = false;
        // The handle is returned rather than dropped so a test - and the exit path - can wait
        // for the bytes to land. Every other caller ignores it, which is the point: a frame is
        // never blocked on the filesystem.
        Some(tokio::task::spawn_blocking(move || {
            if let Err(e) = nutsh_core::cache::write(&dir, snapshot, now) {
                tracing::debug!(error = %e, "cache write failed");
            }
        }))
    }

    /// Re-read the rows; keep the selection on the same name where possible. A config file
    /// that cannot be read leaves the rows empty and puts the error in the message area:
    /// "no contexts" and "your config file is broken" are different answers, and only one of
    /// them is fixed by adding a context.
    pub(crate) fn reload_contexts(&mut self) {
        let keep = self.screen.current().map(|r| r.name.clone());
        // A reload answers whatever the last one said - a config file since fixed, a row since
        // removed - so it starts by dropping it. Callers with something to say say it after.
        self.screen.message = None;
        self.screen.list_error = None;
        self.screen.rows = match self.contexts.list() {
            Ok(rows) => rows,
            Err(e) => {
                self.screen.list_error = Some(format!("{e:#}"));
                Vec::new()
            }
        };
        self.screen.selected = keep
            .and_then(|name| self.screen.rows.iter().position(|r| r.name == name))
            .or_else(|| self.screen.rows.iter().position(|r| r.current))
            .unwrap_or(0);
    }

    /// Open the Contexts screen (from `:ctx`, or at startup). `row` preselects a name.
    pub(crate) fn show_contexts(&mut self, row: Option<&str>) {
        self.reload_contexts();
        self.sync_joined();
        if let Some(name) = row {
            self.screen.select(name);
        }
        self.mode = Mode::Contexts;
        self.dirty = true;
    }

    /// Spawn the connect and show what it is doing. The UI thread never waits on the network:
    /// the result comes back on a channel, like a poll. An open form or login field stays open
    /// until the result is in, so a rejected password costs the user nothing they typed.
    pub(super) fn start_connect(&mut self, request: ConnectRequest, host: &str) {
        // `handle` runs inside the event loop's runtime; anything else calling it (a test with
        // no runtime, say) is a bug that must not take the terminal down with it. It reports
        // like any other failed connect, so it lands wherever the user is looking.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.connect_failed("no async runtime: cannot connect from here".into());
            return;
        };
        let future = self.contexts.connect(request);
        let tx = self.connect_tx.clone();
        runtime.spawn(async move {
            let _ = tx
                .send(ConnectOutcome {
                    peer: None,
                    result: future.await,
                })
                .await;
        });
        self.screen.pending = true;
        self.screen.message = Some(format!("connecting to {host}…"));
        self.mode = if self.screen.prompt.is_some() {
            Mode::Form
        } else {
            Mode::Contexts
        };
    }

    /// A connect that came back on the channel: the primary's, or a peer's.
    pub fn connect_outcome(&mut self, outcome: ConnectOutcome) {
        match outcome.peer {
            None => self.connected(outcome.result),
            Some(name) => self.joined(name, outcome.result),
        }
    }

    /// Join `name` beside the current session: connect it and, when that answers, list the
    /// top table in view from it too. `true` when a connect was started; otherwise the status
    /// line says why not, and nothing will answer on the connect channel.
    pub(super) fn start_join(&mut self, name: &str) -> bool {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.status = Some("no async runtime: cannot connect from here".into());
            return false;
        };
        if let Some(live) = self.live.as_ref() {
            if live.primary_name() == name {
                self.status = Some(format!("{name} is the session"));
                return false;
            }
            if live.peers.iter().any(|p| &*p.name == name) {
                self.status = Some(format!("{name} is already joined"));
                return false;
            }
        }
        // One presentation of a credential per peer: a second ask while the first is in
        // flight would put the password on the wire twice.
        if self.joining.iter().any(|n| n == name) {
            self.status = Some(format!("joining {name}…"));
            return false;
        }
        self.reload_contexts();
        let Some(row) = self.screen.rows.iter().find(|r| r.name == name).cloned() else {
            self.status = Some(format!("no context named {name}"));
            return false;
        };
        if !row.has_password {
            self.status = Some(format!(
                "{name}: no stored password; run `nutsh ctx login {name}` first"
            ));
            return false;
        }
        self.joining.push(name.to_string());
        let future = self.contexts.connect(screen::request(name, None));
        let tx = self.connect_tx.clone();
        let peer: std::sync::Arc<str> = std::sync::Arc::from(name);
        runtime.spawn(async move {
            let _ = tx
                .send(ConnectOutcome {
                    peer: Some(peer),
                    result: future.await,
                })
                .await;
        });
        self.status = Some(format!("joining {name}…"));
        true
    }

    /// `space` on the Contexts screen: join this context beside the session, or drop it.
    pub(super) fn toggle_peer(&mut self, name: &str) {
        if self.live.is_none() {
            self.screen.message = Some("connect to a context first".into());
            return;
        }
        let joined = self
            .live
            .as_ref()
            .is_some_and(|l| l.peers.iter().any(|p| &*p.name == name));
        if joined {
            let keep: Vec<String> = self
                .live
                .as_ref()
                .map(|l| {
                    l.peers
                        .iter()
                        .filter(|p| &*p.name != name)
                        .map(|p| p.name.to_string())
                        .collect()
                })
                .unwrap_or_default();
            self.keep_peers(&keep);
            self.screen.message = Some(format!("{name} dropped"));
        } else {
            self.start_join(name);
            self.screen.message = self.status.clone();
        }
        self.sync_joined();
    }

    /// The screen's `+` marks, from the session's peers.
    pub(super) fn sync_joined(&mut self) {
        self.screen.joined = self
            .live
            .as_ref()
            .map(|l| l.peers.iter().map(|p| p.name.to_string()).collect())
            .unwrap_or_default();
    }

    /// Join every context named, one after another, and wait for each: what `-c a,b` asks
    /// for before the first frame. A peer that fails to connect says so in the status line
    /// and the rest still join.
    pub async fn join(&mut self, names: &[String]) {
        for name in names {
            // Only a started connect answers; waiting on a refused one would wait forever.
            if self.start_join(name)
                && let Some(outcome) = self.connect_rx.recv().await
            {
                self.connect_outcome(outcome);
            }
        }
    }

    /// A peer's connect came back: it gets a scheduler of its own, and the top table in
    /// view is listed from it. Nothing of the primary session moves.
    pub fn joined(&mut self, name: std::sync::Arc<str>, result: anyhow::Result<Connected>) {
        self.dirty = true;
        self.joining.retain(|n| **n != *name);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match result {
            Ok(connected) => {
                let scheduler =
                    Scheduler::new(connected.session.client.clone(), self.poll_tx.clone());
                // The peer's references resolve through the shared name cache, so its rows
                // read as names too.
                let names = nutsh_core::names::spawn(
                    connected.session.client.clone(),
                    self.poll_tx.clone(),
                    self.session_generation,
                    scheduler.idle_flag(),
                );
                live.forget_peer(&name);
                live.peers.push(Peer {
                    name: name.clone(),
                    session: connected.session,
                    scheduler,
                    names,
                });
                live.subscribe_peers();
                self.status = Some(format!("joined {name}"));
            }
            Err(e) => self.status = Some(format!("could not join {name}: {e:#}")),
        }
        self.sync_joined();
        if self.mode == Mode::Contexts {
            self.screen.message = self.status.clone();
        }
    }

    /// Drop every peer not in `keep`: their subscriptions end with their schedulers, and the
    /// rows they listed stay in the store until the view is reopened.
    pub(super) fn keep_peers(&mut self, keep: &[String]) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let dropping: Vec<std::sync::Arc<str>> = live
            .peers
            .iter()
            .filter(|p| !keep.iter().any(|k| **k == *p.name))
            .map(|p| p.name.clone())
            .collect();
        if dropping.is_empty() {
            return;
        }
        for name in &dropping {
            live.forget_peer(name);
        }
        self.sync_joined();
        self.dirty = true;
    }

    /// The outcome of a connect the screen started. Warnings (a password that could not be
    /// stored, say) stay in the message area behind the table; a failure goes back to whatever
    /// box asked for the connect, with what it says still on screen.
    pub fn connected(&mut self, result: anyhow::Result<Connected>) {
        self.screen.pending = false;
        self.dirty = true;
        match result {
            Ok(connected) => self.switch_to(connected),
            Err(e) => self.connect_failed(format!("{e:#}")),
        }
    }

    /// The session is real: it replaces whatever was there, and the boxes that asked for it
    /// close. What could not be done on the way is said next to it, not instead of it.
    pub(super) fn switch_to(
        &mut self,
        Connected {
            session,
            warnings,
            cache_dir,
            restored,
        }: Connected,
    ) {
        // What was open before the switch, so `:ctx` comes back to the same screen.
        let was = match self.live.as_ref().and_then(|live| live.stack.last()) {
            Some(View::Table(view)) => Home::Kind(view.key.kind),
            Some(View::Page(page)) => Home::Page(page.def),
            None => self.home,
        };
        // `attach` writes the outgoing context's cache and clears the directory; this is the
        // incoming one, read before the connection that adopted its pins.
        self.attach(session);
        if let Some(dir) = cache_dir {
            self.enable_cache(dir, restored);
        }
        self.screen.prompt = None;
        self.reload_contexts();
        let mut notes = warnings;
        // The new Prism Central may not serve what the old one did.
        if let Open::Greyed(reason) = self.reopen(was) {
            notes.push(format!("{}: {reason}", was.label()));
            if let Open::Greyed(reason) = self.reopen(self.home) {
                // Nothing left to open: the screen that can pick another Prism Central is a
                // better place to sit than an empty table with no way back to one.
                notes.push(format!("{}: {reason}", self.home.label()));
                self.mode = Mode::Contexts;
            }
        }
        if !notes.is_empty() {
            self.screen.message = Some(notes.join("\n"));
            self.status = Some(notes.join("; "));
        }
        // The peers `:ctx a b` named after the primary, now that there is a session to join.
        for name in std::mem::take(&mut self.pending_peers) {
            self.start_join(&name);
        }
    }

    /// Open whichever of the two a `Home` names, as the root view.
    pub(super) fn reopen(&mut self, home: Home) -> Open {
        match home {
            Home::Kind(kind) => self.open_root(kind),
            Home::Page(def) => self.open_page(def),
        }
    }

    /// Back to the box that asked for the connect, so what was typed is still there to be
    /// corrected; the rows themselves when nothing was open.
    pub(super) fn connect_failed(&mut self, error: String) {
        // The peers named beside a session that never came: nothing to join them to.
        self.pending_peers.clear();
        // Either way it replaces the `connecting to …` it answers.
        self.screen.message = None;
        match self.screen.prompt.as_mut() {
            Some(prompt) => {
                prompt.set_error(error);
                self.mode = Mode::Form;
            }
            None => {
                self.screen.message = Some(error);
                self.mode = Mode::Contexts;
            }
        }
    }

    /// `:ctx`, with no name, one, or several. The first is the session; every name after it is
    /// a peer read beside it, and a peer not named again is dropped.
    pub(crate) fn ctx_command(&mut self, names: Vec<String>) {
        let mut names = names.into_iter();
        let Some(name) = names.next() else {
            self.show_contexts(None);
            return;
        };
        let peers: Vec<String> = names.filter(|n| *n != name).collect();
        // The same session: only the peers move.
        if self
            .live
            .as_ref()
            .is_some_and(|l| l.session.context.as_deref() == Some(name.as_str()))
        {
            self.keep_peers(&peers);
            for peer in peers {
                let joined = self
                    .live
                    .as_ref()
                    .is_some_and(|l| l.peers.iter().any(|p| *p.name == *peer));
                if !joined {
                    self.start_join(&peer);
                }
            }
            return;
        }
        self.pending_peers = peers;
        self.reload_contexts();
        let Some(row) = self.screen.rows.iter().find(|r| r.name == name).cloned() else {
            // Nothing opened, so nothing to go back from: the palette already restored the
            // mode it was opened over.
            self.pending_peers.clear();
            self.status = Some(format!("no context named {name}"));
            return;
        };
        self.screen.select(&name);
        if row.has_password {
            self.start_connect(screen::request(&name, None), &row.host);
        } else {
            // Nothing to connect with: the screen asks for the password rather than failing,
            // and says why inside the box, which covers the rows the reason would go under.
            let why = nutsh_core::contexts::no_stored_password(&name);
            self.screen.open_login(name, Some(why));
            self.mode = Mode::Form;
        }
    }

    /// The context is gone once `remove` returns `Ok`, so a secret store that could not be
    /// reached is reported next to the removal rather than instead of it.
    pub(super) fn remove_context(&mut self, name: &str) {
        let message = match self.contexts.remove(name) {
            Ok(warnings) if warnings.is_empty() => format!("removed {name}"),
            Ok(warnings) => format!("removed {name}; {}", warnings.join("; ")),
            Err(e) => format!("{e:#}"),
        };
        let was = self.screen.selected;
        self.reload_contexts();
        // The row under the cursor is the one that went: staying where it was leaves the
        // cursor on its neighbour, which is where the eye already is.
        self.screen.selected = was.min(self.screen.rows.len().saturating_sub(1));
        self.screen.message = Some(message);
    }
}

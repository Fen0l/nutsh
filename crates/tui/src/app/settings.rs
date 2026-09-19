//! The settings screen, and the schedules and switches it writes.

use super::*;

impl App {
    /// How often this kind's views poll, and where that came from: the one function, over the
    /// file this app read and the overrides this run has made.
    pub(crate) fn refresh_of(
        &self,
        kind: &'static Kind,
    ) -> (Option<std::time::Duration>, nutsh_core::contexts::Source) {
        nutsh_core::refresh::effective(
            kind,
            &self.refresh_cfg,
            self.refresh_session.get(kind.id).copied(),
        )
    }

    /// `ctrl-t`: one rung down the ladder for the kind of the view in front of the user, for
    /// this session, and every pausable subscription over it retimed on the same keystroke.
    pub(super) fn step_refresh(&mut self) {
        let Some(kind) = self.current_kind() else {
            return;
        };
        let next = nutsh_core::refresh::step(self.refresh_of(kind).0);
        self.refresh_session.insert(kind.id, next);
        let (every, _) = self.refresh_of(kind);
        if let Some(live) = self.live.as_ref() {
            for s in live.schedulers() {
                s.set_interval_kind(kind.id, every);
            }
        }
        // The flash names the gesture that keeps it: this is the one place the verb is taught.
        self.status = Some(match every {
            // `cell::span` takes whole seconds and is the one spelling of a length of time this
            // program has, which is why `:refresh 60` comes back as `1m`.
            Some(d) => format!(
                "refresh: every {} · :refresh {} to keep it",
                nutsh_core::cell::span(d.as_secs()),
                d.as_secs()
            ),
            None => "refresh: off · ^r still refreshes once".to_string(),
        });
        self.dirty = true;
    }

    /// `:refresh <auto|off|N>`: the same value, kept in `[refresh.kinds]`. With no argument it
    /// re-opens the palette on the verb, where the ladder is offered.
    pub(super) fn refresh_command(&mut self, args: &[String]) {
        let Some(kind) = self.current_kind() else {
            return;
        };
        match args.first().map(|w| nutsh_core::refresh::parse(w)) {
            None => self.open_palette_with("refresh "),
            Some(Err(why)) => self.status = Some(format!("refresh: {why}")),
            Some(Ok(v)) => {
                // The file is now where this value lives, so the session override goes: the
                // screen's source column has to say `config file`, not `this session`.
                self.refresh_session.remove(kind.id);
                self.status = Some(
                    match self
                        .contexts
                        .set_interval(nutsh_core::contexts::Schedule::Kind(kind.id), v)
                    {
                        Ok(()) => {
                            self.refresh_cfg = self.contexts.refresh();
                            format!(
                                "refresh: {} kept for {}",
                                nutsh_core::refresh::show(v),
                                kind.display
                            )
                        }
                        Err(e) => format!("refresh not saved: {e:#}"),
                    },
                );
                let (every, _) = self.refresh_of(kind);
                if let Some(live) = self.live.as_ref() {
                    for s in live.schedulers() {
                        s.set_interval_kind(kind.id, every);
                    }
                }
            }
        }
    }

    /// `:skin NAME` applies straight away; bare `:skin` opens the list.
    pub(super) fn skin_command(&mut self, argument: Option<String>) {
        match argument {
            Some(name) => self.apply_skin(&name),
            None => {
                self.skins = Some(Skins::open());
                self.skins_from = self.mode;
                self.mode = Mode::Skins;
            }
        }
    }

    /// Apply live, then persist. A failed write is a status line, not a refusal: the skin is
    /// already on screen and telling the user it did not happen would be a lie.
    pub(super) fn apply_skin(&mut self, name: &str) {
        self.status = Some(match crate::theme::apply_named(name) {
            Err(reason) => reason,
            // The canonical name: what the list marks, and the spelling the config gets.
            Ok(name) => match self.contexts.set_skin(name) {
                Ok(()) => format!("skin {name}"),
                Err(e) => format!("skin {name}; not saved: {e:#}"),
            },
        });
        self.dirty = true;
    }

    /// `:header` walks the three settings and writes the one it lands on to the config file,
    /// the way `:mouse` writes its toggle. A cycle rather than an argument because there are
    /// three settings and two of them are the interesting ones: `auto` is where a session
    /// starts, and the two overrides are one keystroke apart from it in either direction.
    ///
    /// A failed write is a red status line, not a refusal: the header is already drawn the new
    /// way, and saying so twice would be the only honest alternative to saying it once.
    pub(super) fn header_command(&mut self) {
        self.header = match self.header {
            Header::Auto => Header::Compact,
            Header::Compact => Header::Full,
            Header::Full => Header::Auto,
        };
        let state = match self.header {
            Header::Auto => "auto",
            Header::Compact => "compact",
            Header::Full => "full",
        };
        self.status = Some(match self.contexts.set_header(self.header) {
            Ok(()) => format!("header {state}"),
            Err(e) => format!("header {state} for this session: {e}"),
        });
    }

    /// `:mouse` toggles capture and writes the answer to the config file, the way `:skin` writes
    /// `[skin].name`. A failed write is a red status line, not a refusal: the mouse is already
    /// on or off and telling the user it did not happen would be a lie.
    pub(super) fn mouse_command(&mut self) {
        self.mouse_on = !self.mouse_on;
        let state = if self.mouse_on { "on" } else { "off" };
        self.status = Some(match self.contexts.set_mouse(self.mouse_on) {
            Ok(()) => format!("mouse {state}"),
            Err(e) => format!("mouse {state}; not saved: {e:#}"),
        });
        self.dirty = true;
    }

    /// `:log [level]` moves what this session writes down and writes the answer to the config
    /// file, the way `:mouse` and `:header` write theirs.
    ///
    /// Bare it is a switch between `off` and `debug`, and not a walk through six rungs the way
    /// `:header` walks three: this is the command somebody reaches for while something is
    /// already going wrong, and `error` and `warn` are not what they are reaching for. A level
    /// named outright still sets exactly that one.
    ///
    /// The level moves first and is saved second, and a save that failed is a status line
    /// rather than a refusal - the lines are already being written, and saying they are not
    /// would be the lie. A run that installed no subscriber at all says so instead of
    /// pretending: there is nothing listening for it to have changed.
    pub(super) fn log_command(&mut self, args: &[String]) {
        let now = nutsh_core::log::state();
        let next = match args.first() {
            Some(word) => match nutsh_core::contexts::LogLevel::parse(word) {
                Some(level) => level,
                None => {
                    self.status = Some(format!(
                        "log: {word} is not a level; try off, error, warn, info, debug or trace"
                    ));
                    return;
                }
            },
            None if now.is_some_and(|l| l.on()) => nutsh_core::contexts::LogLevel::Off,
            None => nutsh_core::contexts::LogLevel::Debug,
        };
        self.status = Some(match nutsh_core::log::set(next) {
            Err(e) => format!("log {next}: {e:#}"),
            Ok(state) => {
                let where_to = match (&state.path, state.on()) {
                    (Some(path), true) => format!(" to {}", path.display()),
                    _ => String::new(),
                };
                match self.contexts.set_log(next) {
                    Ok(()) => format!("log {next}{where_to}"),
                    Err(e) => format!("log {next}{where_to}; not saved: {e:#}"),
                }
            }
        });
    }

    pub(super) fn handle_skins(&mut self, key: Key) {
        let Some(list) = self.skins.as_mut() else {
            return;
        };
        match list.key(key) {
            skins::Action::Moved | skins::Action::Ignored => {}
            skins::Action::Cancelled => {
                self.skins = None;
                self.mode = self.skins_from;
            }
            skins::Action::Chose(name) => {
                self.skins = None;
                self.mode = self.skins_from;
                self.apply_skin(name);
            }
        }
        // Opened from the settings screen, so come back to it with the skin row showing the
        // skin that was just applied rather than the one that was showing when it opened.
        if self.mode == Mode::Settings {
            self.reopen_settings();
        }
    }

    /// The settings screen, over whatever is on screen. Its own mode, like the skin list, with
    /// the mode behind it remembered so `esc` goes back where it came from.
    pub(crate) fn show_settings(&mut self) {
        // The three reads finish before the field is assigned, so `self` is never borrowed
        // immutably and mutably at once.
        let rows = self.contexts.settings();
        let skin = crate::theme::current_name();
        let refresh = self.refresh_rows();
        let screen = crate::settings::Settings::open(&rows, &skin, &refresh, &self.nav_hide);
        self.settings = Some(screen);
        self.settings_from = self.mode;
        self.mode = Mode::Settings;
        self.dirty = true;
    }

    /// The schedule rows: `[refresh] default` always, and the view in front of the user when
    /// there is one. The kind's row carries its display name, because `refresh` alone over two
    /// rows would say nothing about which of them is which.
    pub(super) fn refresh_rows(&self) -> Vec<crate::settings::Row> {
        use nutsh_core::contexts::{SettingId, Source};
        let global = self.refresh_cfg.default;
        let ages = self.poll_ages();
        let mut out = vec![crate::settings::Row::Setting {
            id: SettingId::Refresh,
            label: "every kind".to_string(),
            value: global.map_or_else(|| "auto".to_string(), nutsh_core::refresh::show),
            source: if global.is_some() {
                Source::File.label()
            } else {
                Source::Default.label()
            },
            fixed: None,
            kind: None,
            age: None,
        }];

        // By namespace, because two hundred and thirty-two rows of which two hundred and
        // twenty-six say the same thing is a list nobody reads. A namespace opens onto its own
        // kinds, and the ones somebody has set are counted on its row whether it is open or not.
        for ns in nutsh_catalog::NAMESPACES {
            let kinds: Vec<&'static Kind> = nutsh_catalog::KINDS
                .iter()
                .filter(|k| k.namespace == ns.name)
                .collect();
            if kinds.is_empty() {
                continue;
            }
            let own = self.refresh_cfg.namespaces.get(ns.name).copied();
            let custom = kinds
                .iter()
                .filter(|k| self.refresh_cfg.kinds.contains_key(k.id))
                .count();
            let open = self.settings_open.contains(ns.name);
            out.push(crate::settings::Row::Namespace {
                name: ns.name,
                value: own.map_or_else(|| "-".to_string(), nutsh_core::refresh::show),
                source: own.map_or_else(
                    || Source::Default.label(),
                    |_| Source::Namespace(ns.name).label(),
                ),
                open,
                custom,
            });
            if !open {
                continue;
            }
            let mut rows: Vec<(Option<std::time::Duration>, Source, &'static Kind)> = kinds
                .into_iter()
                .map(|k| {
                    let (every, source) = self.refresh_of(k);
                    (every, source, k)
                })
                .collect();
            // Soonest first inside the namespace: `off` is not a very long interval, it is no
            // schedule, so it sorts last.
            rows.sort_by(|a, b| {
                let key = |d: &Option<std::time::Duration>| d.map_or(u64::MAX, |d| d.as_secs());
                key(&a.0).cmp(&key(&b.0)).then(a.2.display.cmp(b.2.display))
            });
            out.extend(
                rows.into_iter()
                    .map(|(every, source, k)| crate::settings::Row::Setting {
                        id: SettingId::RefreshKind,
                        label: format!("  {}", k.display),
                        value: every.map_or_else(
                            || "off".to_string(),
                            |d| nutsh_core::cell::span(d.as_secs()),
                        ),
                        source: source.label(),
                        fixed: None,
                        kind: Some(k.id),
                        age: ages.get(k.id).cloned(),
                    }),
            );
        }
        out
    }

    /// How long ago each kind's table last completed a cycle, rendered. A kind with no table
    /// open is absent: it has not refreshed, which is not the same as having refreshed long ago.
    pub(super) fn poll_ages(&self) -> std::collections::HashMap<&'static str, String> {
        let now = std::time::Instant::now();
        let mut out = std::collections::HashMap::new();
        let Some(live) = self.live.as_ref() else {
            return out;
        };
        for (key, table) in live.store.tables() {
            let Some(at) = table.last_poll else { continue };
            let secs = now.saturating_duration_since(at).as_secs();
            out.entry(key.kind.id)
                .or_insert_with(|| nutsh_core::cell::span(secs));
        }
        out
    }

    pub(super) fn close_settings(&mut self) {
        self.settings = None;
        self.mode = self.settings_from;
        self.dirty = true;
    }

    /// Rebuild the rows from the seam, keeping the cursor where it was: a toggle changes a
    /// value and a `d` changes the list, and both have to be visible without closing the screen.
    ///
    /// `settings_from` is restored around the rebuild - `show_settings` records the mode it was
    /// called from, and here that mode is `Settings` itself, which would make `esc` reopen the
    /// screen it just closed.
    pub(super) fn reopen_settings(&mut self) {
        let selected = self.settings.as_ref().map_or(0, |s| s.selected);
        let from = self.settings_from;
        self.show_settings();
        self.settings_from = from;
        if let Some(screen) = self.settings.as_mut() {
            screen.selected = selected.min(screen.rows.len().saturating_sub(1));
        }
    }

    /// One writer for every level of `[refresh]`: the file, then the pollers it retimes, then
    /// the screen. A `ctrl-t` override for a kind goes when its rung is written, so the source
    /// column reads `config file` and not `this session`.
    pub(super) fn set_schedule(
        &mut self,
        at: nutsh_core::contexts::Schedule<'static>,
        v: nutsh_core::contexts::Interval,
    ) {
        use nutsh_core::contexts::Schedule;
        if let Schedule::Kind(id) = at {
            self.refresh_session.remove(id);
        }
        let shown = nutsh_core::refresh::show(v);
        self.status = Some(match self.contexts.set_interval(at, v) {
            Ok(()) => {
                self.refresh_cfg = self.contexts.refresh();
                match at {
                    Schedule::Namespace(ns) => format!("{ns}: {shown}"),
                    _ => format!("refresh: {shown}"),
                }
            }
            Err(e) => format!("refresh not saved: {e:#}"),
        });
        if let Some(live) = self.live.as_ref() {
            let touched = nutsh_catalog::KINDS.iter().filter(|k| match at {
                Schedule::Everything => true,
                Schedule::Namespace(ns) => k.namespace == ns,
                Schedule::Kind(id) => k.id == id,
            });
            for k in touched {
                let every = self.refresh_of(k).0;
                for s in live.schedulers() {
                    s.set_interval_kind(k.id, every);
                }
            }
        }
        self.reopen_settings();
    }

    pub(super) fn handle_ladder(&mut self, key: Key) {
        let Some(list) = self.ladder.as_mut() else {
            return;
        };
        match list.key(key) {
            crate::ladder::Action::Moved | crate::ladder::Action::Ignored => {}
            crate::ladder::Action::Cancelled => {
                self.ladder = None;
                self.mode = Mode::Settings;
            }
            crate::ladder::Action::Chose(v) => {
                let at = list.at;
                self.ladder = None;
                self.mode = Mode::Settings;
                self.set_schedule(at, v);
            }
        }
        self.dirty = true;
    }

    pub(super) fn handle_settings(&mut self, key: Key) {
        let Some(screen) = self.settings.as_mut() else {
            return;
        };
        match screen.key(key) {
            crate::settings::Action::Moved | crate::settings::Action::Ignored => {}
            crate::settings::Action::Cancelled => self.close_settings(),
            crate::settings::Action::Fixed(why) => self.status = Some(why),
            crate::settings::Action::Skins => {
                // Over the settings screen, not instead of it: `esc` in the skin list comes
                // back here, which is where the user was.
                self.skins = Some(Skins::open());
                self.skins_from = Mode::Settings;
                self.mode = Mode::Skins;
            }
            crate::settings::Action::Add => {
                self.close_settings();
                // The verb the palette already completes: one ranking, one writer.
                self.open_palette_with("hide ");
            }
            crate::settings::Action::Remove(name) => {
                self.nav_command(Some(name), false);
                self.reopen_settings();
            }
            crate::settings::Action::RefreshAll => {
                let n = self.live.as_mut().map_or(0, |live| {
                    live.schedulers_mut().map(|s| s.refresh_everything()).sum()
                });
                self.status = Some(match n {
                    0 => "nothing subscribed to refresh".to_string(),
                    1 => "refreshing 1 view".to_string(),
                    n => format!("refreshing {n} views"),
                });
            }
            crate::settings::Action::Expand(ns) => {
                if !self.settings_open.remove(ns) {
                    self.settings_open.insert(ns);
                }
                self.reopen_settings();
            }
            crate::settings::Action::Pick(row_kind, label) => {
                let kind = row_kind.and_then(nutsh_catalog::kind);
                let (at, current) = match kind {
                    Some(k) => (
                        nutsh_core::contexts::Schedule::Kind(k.id),
                        self.refresh_cfg.kinds.get(k.id).copied(),
                    ),
                    None => (
                        nutsh_core::contexts::Schedule::Everything,
                        self.refresh_cfg.default,
                    ),
                };
                self.ladder = Some(crate::ladder::Ladder::open(at, label, current));
                self.mode = Mode::Ladder;
            }
            crate::settings::Action::CycledNamespace(ns) => {
                // The rung after whatever the namespace itself says, not after what its kinds
                // resolve to: those differ from each other, and a step has to start somewhere.
                let now = self
                    .refresh_cfg
                    .namespaces
                    .get(ns)
                    .copied()
                    .or(self.refresh_cfg.default);
                let next = nutsh_core::refresh::step_from(now);
                self.set_schedule(nutsh_core::contexts::Schedule::Namespace(ns), next);
            }
            crate::settings::Action::Cycled(_, row_kind) => {
                let kind = row_kind.and_then(nutsh_catalog::kind);
                match kind {
                    // A kind's row steps from the length of time it is actually polling at, so
                    // a 3 s kind does not spend a press on the 5 s it is nearly already at.
                    Some(k) => {
                        let next = nutsh_core::refresh::step(self.refresh_of(k).0);
                        self.set_schedule(nutsh_core::contexts::Schedule::Kind(k.id), next);
                    }
                    // The global row names a rung and stands for no kind in particular, so it
                    // walks the ladder by position instead.
                    None => {
                        let next = nutsh_core::refresh::step_from(self.refresh_cfg.default);
                        self.set_schedule(nutsh_core::contexts::Schedule::Everything, next);
                    }
                }
            }
            crate::settings::Action::Cycle(id) => {
                match id {
                    nutsh_core::contexts::SettingId::Mouse => self.mouse_command(),
                    nutsh_core::contexts::SettingId::Log => self.log_command(&[]),
                    _ => self.header_command(),
                }
                self.reopen_settings();
            }
            crate::settings::Action::Toggled(id, on) => {
                self.status = Some(match self.contexts.set_flag(id, on) {
                    Ok(()) => format!("{} {}", id.label(), if on { "on" } else { "off" }),
                    Err(e) => format!("{} not saved: {e:#}", id.label()),
                });
                // The menu is downstream of one of these switches, so recompute it here rather
                // than making every caller remember which.
                if id == nutsh_core::contexts::SettingId::HideUnserved {
                    self.nav_hide_unserved = on;
                    self.apply_nav();
                }
                self.reopen_settings();
            }
        }
    }
}

//! The `:` palette and the commands it runs.

use super::*;

impl App {
    /// `:try <kind>`. The version refusal is a prediction the catalog makes before the server
    /// has had a vote; this is where a person overrules it.
    ///
    /// One request, and the answer stands: a 404 greys the kind again on what the server said,
    /// which is a better reason than the one it replaced, and a 200 is the kind working for
    /// the rest of the session. Nothing retries it and nothing calls it on the user's behalf -
    /// the whole point is that a person decided to spend the request.
    pub(super) fn try_command(&mut self, args: &[String]) {
        let Some(term) = args.first() else {
            self.status = Some(":try takes a kind".into());
            return;
        };
        let Some(kind) = lookup(term).first().copied() else {
            self.status = Some(format!("no kind matching {term}"));
            return;
        };
        let Some(live) = self.live.as_ref() else {
            self.status = Some("not connected".into());
            return;
        };
        // Only the version prediction is a guess. A namespace that answered at no version and
        // a 404 this session already paid for are answers, and `:try` has nothing to add to
        // either - it would spend a request to be told again.
        if !matches!(
            live.session.client.availability(kind),
            nutsh_prism::Availability::NotServedAtPin { .. }
        ) {
            self.status = Some(match unavailable_reason(&live.session, kind) {
                Some(reason) => format!("{}: {reason}", kind.display),
                None => format!("{} is served: open it by name", kind.display),
            });
            return;
        }
        live.session.client.ask_anyway(kind);
        if let Open::Greyed(reason) = self.open_root(kind) {
            self.status = Some(reason);
            return;
        }
        self.status = Some(format!(
            "asking this Prism Central for {} at {} anyway",
            kind.display, kind.since
        ));
    }

    /// `:can-i <action> <kind>`. It renders exactly what the menu greys a row with, so the two
    /// cannot disagree by construction.
    pub(super) fn can_i_command(&mut self, args: &[String]) {
        self.start_can_i();
        let [action_name, kind_term] = args else {
            self.status = Some(":can-i takes an action and a kind".into());
            return;
        };
        let Some(kind) = lookup(kind_term).first().copied() else {
            self.status = Some(format!("no kind matching {kind_term}"));
            return;
        };
        let Some(action) = kind.action_target().action(action_name) else {
            self.status = Some(format!("no action {action_name} on {}", kind.display));
            return;
        };
        let Some(live) = self.live.as_ref() else {
            self.status = Some("not connected".into());
            return;
        };
        // The cursor's row answers for the table it is in and for no other kind: a VM row
        // cannot supply a NIC's parents, and pretending it could is the one thing `:can-i`
        // must not do. Asked about another kind, the question is about the kind alone.
        let (parents, entity) = match self.cursor_table() {
            Some(cursor) if cursor.key.kind.id == kind.id => {
                (cursor.key.parents.clone(), self.selected_entity())
            }
            _ => (Vec::new(), None),
        };
        let can_i = live.can_i.get();
        let verdict = nutsh_core::actions::reason(
            Policy {
                session: &live.session,
                guardrails: &self.guardrails,
                can_i: &can_i,
            },
            kind,
            action,
            &parents,
            entity,
            1,
        );
        // `Some` is the same string the menu shows; `None` reports *why it was allowed*, which
        // is the honest answer and the one `:can-i` exists to give. `No` cannot occur here - it
        // would have made `reason` return `Some`.
        let text = match verdict {
            Some(reason) => reason,
            None => match can_i.can(action) {
                CanI::Yes { via } => format!("allowed (via {via})"),
                CanI::Unknown(why) => format!("unknown - {why}"),
                CanI::No { missing } => format!("not permitted: needs {}", missing.join(", ")),
            },
        };
        self.status = Some(format!("{action_name} on {}: {text}", kind.display));
    }

    pub(super) fn open_palette(&mut self) {
        self.open_palette_with("");
    }

    /// The palette, opened with `prefix` already typed. One caller so far - the settings
    /// screen's `a`, which lands on a ranked, completing `:hide ` rather than on a second list
    /// that screen would have to keep correct.
    pub(super) fn open_palette_with(&mut self, prefix: &str) {
        let mut palette = Palette::with_history(self.history.entries().to_vec());
        for c in prefix.chars() {
            palette.key(Key::Char(c));
        }
        self.palette = Some(palette);
        self.refresh_palette();
        self.palette_from = self.mode;
        self.mode = Mode::Command;
    }

    pub(super) fn handle_palette(&mut self, key: Key) {
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        let action = palette.key(key);
        // The line at the moment `Chose` was produced, trimmed, read before the palette is
        // dropped - and only then: a cancelled line and an `enter` with nothing under the
        // cursor are not things the user committed to.
        let line = matches!(action, palette::Action::Chose(..))
            .then(|| palette.input.as_str().trim().to_string());
        match action {
            // An edit re-ranks, and so does a cursor move: `Palette::refresh` is a function of
            // the cursor, so a cursor that crossed a word boundary is completing a different
            // word. `refresh` only clamps `selected` and never resets it, so re-ranking on the
            // `Moved` that `↑`/`↓` also produce keeps the selection where the user put it. An
            // accepted ghost changed the line, so it re-ranks for the same reason an edit does.
            palette::Action::Edited | palette::Action::Moved | palette::Action::Completed => {
                self.refresh_palette()
            }
            palette::Action::Ignored => {}
            palette::Action::Cancelled => {
                self.palette = None;
                self.mode = self.palette_from;
            }
            palette::Action::Chose(entry, args) => {
                self.palette = None;
                self.mode = self.palette_from;
                // Recorded before the command runs: `:ctx other` swaps the session and with it
                // the history, so a line written after the switch would land in the wrong file.
                if let Some(line) = line {
                    self.history.push(&line);
                }
                self.choose(entry, args);
            }
        }
    }

    /// What `enter` in the palette does with the entry under the cursor. `args` is every word
    /// after the command word: one command takes two of them.
    pub(super) fn choose(&mut self, entry: Entry, args: Vec<String>) {
        // Before anything else. `args.first()` alone is a silent drop: `:ctx a b` would
        // connect to `a` and say nothing about `b`.
        // The message is built from the command's label and its slot count, so a command added
        // later gets its refusal for free.
        if let Some(command) = entry_command(&entry) {
            let slots = command.slots().len();
            // A nav name is display text: `Compute & Storage` is three words in one slot, so
            // the last slot of `:hide`/`:show` swallows the rest of the line.
            if args.len() > slots && !command.rest_of_line() {
                self.status = Some(arity_refusal(command, slots));
                return;
            }
        }
        match entry {
            Entry::Command(Command::Quit) => self.quit = true,
            Entry::Command(Command::Help) => {
                self.help_from = self.mode;
                self.mode = Mode::Help;
            }
            Entry::Command(Command::Ctx) => self.ctx_command(args),
            Entry::Command(Command::Skin) => self.skin_command(args.into_iter().next()),
            Entry::Command(Command::Journal) => self.journal_command(),
            Entry::Command(Command::Activity) => self.activity_command(),
            // Joined, not `first()`: the term is the rest of the line, spaces and all.
            Entry::Command(Command::Search) => self.search_command(&args.join(" ")),
            Entry::Command(Command::Export) => self.export_command(&args),
            Entry::Command(Command::CanI) => self.can_i_command(&args),
            Entry::Command(Command::Try) => self.try_command(&args),
            Entry::Command(Command::Mouse) => self.mouse_command(),
            Entry::Command(Command::Header) => self.header_command(),
            Entry::Command(Command::Log) => self.log_command(&args),
            Entry::Command(Command::Settings) => self.show_settings(),
            Entry::Command(Command::Refresh) => self.refresh_command(&args),
            Entry::Command(Command::All) => {
                self.nav_all = !self.nav_all;
                self.apply_nav();
                // `:all` suspends rule 2 and nothing else, so it may not claim to be showing
                // every kind while rule 1 - an instruction, not a heuristic - keeps one out.
                // The count is of `[nav] hide` entries, which is what the user typed.
                self.status = Some(match (self.nav_all, self.nav_hide_unserved) {
                    (true, _) if self.nav_hide.is_empty() => {
                        "showing every kind this session".into()
                    }
                    (true, _) => format!(
                        "showing every kind this session except the {} you hid",
                        self.nav_hide.len()
                    ),
                    (false, true) => "hiding what this Prism Central does not serve".into(),
                    (false, false) => "nothing is hidden but what you asked for".into(),
                });
            }
            // Joined, not `first()`: the words are the name, and the menu spells it with spaces.
            Entry::Command(Command::Hide) => self.nav_command(joined(args), true),
            Entry::Command(Command::Show) => self.nav_command(joined(args), false),
            // A completed argument runs the command it belongs to.
            Entry::Value { text, tag } => match tag {
                palette::Tag::Context => self.ctx_command(vec![text]),
                palette::Tag::Skin => self.apply_skin(&text),
                palette::Tag::Nav { hide } => self.nav_command(Some(text), hide),
                palette::Tag::Interval => self.refresh_command(&[text]),
                palette::Tag::Level => self.log_command(&[text]),
                palette::Tag::Format => self.export_command(&[text]),
            },
            Entry::Kind {
                kind,
                reason: Some(reason),
            } => match reach(kind) {
                // A child kind cannot be a root, but its parent can: open that, and say which
                // row to press enter on.
                Reach::FromParent(parent) => {
                    self.status = Some(match self.open_root(parent) {
                        Open::Opened => format!(
                            "open {} from a row of {}: pick one and press enter",
                            kind.display, parent.display
                        ),
                        Open::Greyed(reason) => reason,
                    });
                }
                // Nothing to open: the reason is all this key can offer.
                Reach::Direct | Reach::NeedsParameter => self.status = Some(reason),
            },
            Entry::Kind { kind, reason: None } => {
                if let Open::Greyed(reason) = self.open_root(kind) {
                    self.status = Some(reason);
                }
            }
            Entry::Page(def) => {
                if let Open::Greyed(reason) = self.open_page(def) {
                    self.status = Some(reason);
                }
            }
            // Nothing to open: the note is all this key can offer, and saying it is the whole
            // reason the row is on the list at all.
            Entry::Missing(item) => {
                self.status = Some(match item.note {
                    Some(note) => format!("{}: {note}", item.label),
                    None => format!("{}: nothing to open", item.label),
                });
            }
        }
    }

    /// Recompute the palette for its input: `live`, `screen` and `palette` are borrowed as
    /// the separate fields they are, so the ranking can read the session and the context rows
    /// while the palette is borrowed mutably from the same `self`.
    ///
    /// `:ctx ` completes from the rows the Contexts screen shows, not from a fresh `list()`:
    /// that reads and parses the config file and probes every secret store for every context
    /// (on a keyring build, possibly with a dialog), and this runs on every keystroke. The
    /// rows are read at startup and by everything that changes them; a context added by
    /// another process is completed after the same `ctrl-r` that shows it on the screen.
    pub(super) fn refresh_palette(&mut self) {
        // Nothing to refresh, nothing to build: the vocabulary walks the view and allocates, and
        // this runs on every palette keystroke.
        if self.palette.is_none() {
            return;
        }
        // Owned before the palette is borrowed mutably: `a.name` is `&'static str`, so this
        // borrows nothing of `self` past the end of the statement.
        let actions: Vec<&'static str> = self
            .view()
            .map(|v| {
                v.key
                    .kind
                    .action_target()
                    .actions
                    .iter()
                    .map(|a| a.name)
                    .collect()
            })
            .unwrap_or_default();
        let live = self.live.as_ref();
        let unavailable = move |kind: &Kind| unavailable_reason(&live?.session, kind);
        let contexts: Vec<&str> = self.screen.rows.iter().map(|r| r.name.as_str()).collect();
        let hidden: Vec<&str> = self.nav_hide.iter().map(String::as_str).collect();
        let vocab = palette::Vocabulary {
            unavailable: &unavailable,
            contexts,
            actions,
            hidden,
        };
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        palette.refresh(&vocab);
    }
}

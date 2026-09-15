//! `:` palette: a text input over a ranked list of kinds and commands. It owns its input and
//! its scroll window; the app applies the effects a key asks for.

use std::collections::HashSet;

use nutsh_catalog::{Kind, NavItem, NavTarget, PageDef, Reach, lookup, reach};

use crate::key::Key;
use crate::text::{Edit, Input};

/// The entries of the palette that are not kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Ctx,
    Quit,
    Help,
    Skin,
    /// `:journal` - what was attempted this session.
    Journal,
    /// `:activity` - every request this session made, and every table it holds.
    Activity,
    /// `:search <term>` - every kind already loaded, looked at for one term.
    Search,
    /// `:can-i <action> <kind>` - the one refusal string, asked about any kind.
    CanI,
    /// `:try <kind>` - ask this Prism Central for a kind the catalog says its pinned API
    /// version does not have. One request, and whatever comes back stands.
    Try,
    /// `:mouse` - capture on or off, written back to the config file.
    Mouse,
    /// `:header` - auto, compact or full, written back to the config file.
    Header,
    /// `:log [level]` - what this session writes down, and where. Bare it turns logging on at
    /// `debug` or off again, because the point of it is to be reached for when something is
    /// already wrong; a level names one exactly.
    Log,
    /// `:settings` - every setting, its value and where that value came from.
    Settings,
    /// `:refresh <auto|off|N>` - how often the current view's kind polls, kept in `[refresh]`.
    Refresh,
    /// Show everything rule 2 hid, for this session. Rule 1 is an instruction, not a heuristic,
    /// and is not lifted.
    All,
    /// `:hide <id|group>` and `:show <id>`: rule 1's list, written through the same seam
    /// `:skin` writes through, so the file's other sections do not move.
    Hide,
    Show,
}

impl Command {
    /// What the user types and reads.
    pub fn label(self) -> &'static str {
        match self {
            Command::Ctx => "ctx",
            Command::Quit => "quit",
            Command::Help => "help",
            Command::Skin => "skin",
            Command::Journal => "journal",
            Command::Activity => "activity",
            Command::Search => "search",
            Command::CanI => "can-i",
            Command::Try => "try",
            Command::Mouse => "mouse",
            Command::Header => "header",
            Command::Log => "log",
            Command::Settings => "settings",
            Command::Refresh => "refresh",
            Command::All => "all",
            Command::Hide => "hide",
            Command::Show => "show",
        }
    }

    /// Whether the last slot swallows every remaining word rather than one.
    ///
    /// A nav name is display text and `Compute & Storage` is three of them, so `:hide` and
    /// `:show` join what follows instead of overflowing their one slot - which is what
    /// `App::choose`'s arity check would otherwise refuse, and it would be refusing the
    /// spelling the menu itself shows.
    ///
    /// `:search` is the same answer for the opposite reason: it has no slot at all, because
    /// nothing completes free text, and without this the arity check would refuse every term
    /// typed after it.
    pub fn rest_of_line(self) -> bool {
        matches!(self, Command::Hide | Command::Show | Command::Search)
    }

    /// One slot per argument position. Total on purpose - no wildcard arm - so a `Command`
    /// variant added by another plan cannot land without declaring its grammar.
    pub fn slots(self) -> &'static [Slot] {
        match self {
            Command::Ctx => &[Slot::Contexts],
            Command::Skin => &[Slot::Skins],
            Command::CanI => &[Slot::Actions, Slot::Kinds],
            Command::Try => &[Slot::Kinds],
            Command::Hide => &[Slot::Hide],
            Command::Show => &[Slot::Show],
            // A suggestion, not a constraint: `Action::Chose` hands the app every word after
            // the command, so `:refresh 45` reaches the parser without the slot having to carry
            // every number a person might type.
            Command::Refresh => &[Slot::Intervals],
            // Likewise a suggestion: the six words are the whole vocabulary, and a bare `:log`
            // with no argument is the gesture this command is mostly typed as.
            Command::Log => &[Slot::Levels],
            // `:search` takes a term and offers no completion for it: a slot draws on a fixed
            // vocabulary and an address is not in one. The popup collapses to the `search` row,
            // `rest_of_line` keeps the words, and the app reads them.
            Command::Quit
            | Command::Help
            | Command::Journal
            | Command::Activity
            | Command::Search
            | Command::Mouse
            | Command::Header
            // The screen is the completion: every setting it can change is a row on it.
            | Command::Settings
            | Command::All => &[],
        }
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::Ctx,
    Command::Quit,
    Command::Help,
    Command::Skin,
    Command::Journal,
    Command::Activity,
    Command::Search,
    Command::CanI,
    Command::Try,
    Command::Mouse,
    Command::Header,
    Command::Log,
    Command::Settings,
    Command::Refresh,
    Command::All,
    Command::Hide,
    Command::Show,
];

/// Which fixed set an argument position draws on. Declarative, so adding `:can-i` was two lines
/// in `Command` and one arm here rather than a third branch in `refresh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Contexts,
    Skins,
    Actions,
    Kinds,
    /// A kind id, a page id or a group name: what `:hide` takes.
    Hide,
    /// The rungs of the refresh ladder: what `:refresh` takes.
    Intervals,
    /// The six log levels: what `:log` takes.
    Levels,
    /// The same, narrowed to what is hidden: what `:show` takes. Two slots rather than one
    /// because the vocabularies differ and because the verb has to reach the row - a group
    /// hidden whole leaves nothing on screen, so `:show ` is the only place its name still is.
    Show,
}

impl Slot {
    /// What the popup calls the rows it is showing. At the head the popup says
    /// `kinds & commands`, which is not a slot and lives in `ui`.
    ///
    /// The noun is the title's, so it names what is being completed rather than whatever the
    /// list happens to hold: a slot that offers no row is still that slot.
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Slot::Contexts => "contexts",
            Slot::Skins => "skins",
            Slot::Actions => "actions",
            Slot::Kinds => "kinds",
            Slot::Hide => "menu items",
            Slot::Show => "hidden items",
            Slot::Intervals => "intervals",
            Slot::Levels => "levels",
        }
    }
}

/// What a completed argument is, drawn as a coloured tag at the popup's right edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Context,
    Skin,
    /// A menu item, page or group, named by `:hide` (`hide: true`) or by `:show`. The verb
    /// travels with the row because the two slots offer different vocabularies - everything the
    /// menu holds, and only what is hidden - and `enter` has to know which of the two it runs.
    Nav {
        hide: bool,
    },
    /// A rung of the refresh ladder, named by `:refresh`.
    Interval,
    /// A log level, named by `:log`.
    Level,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A kind, with the reason it cannot be opened from here when there is one: the catalog's
    /// (`open from Virtual Machines`, `needs a parameter`) or the session's.
    Kind {
        kind: &'static Kind,
        reason: Option<String>,
    },
    Command(Command),
    /// A feature page, matched on its title or its id: `:disaster` is how the Disaster
    /// Recovery page is reached without the menu.
    Page(&'static PageDef),
    /// An argument for the command already typed: a context name after `:ctx `, a skin name
    /// after `:skin `. `enter` runs the command with it.
    Value {
        text: String,
        tag: Tag,
    },
    /// A nav item with nothing behind it: greyed, with its `note` as the reason. Three rows exist
    /// in the whole catalog and `menu.rs:9-11` already argues the case - a greyed row beats a
    /// vanished one, because "there is no such thing" is an answer and an empty list is not.
    Missing(&'static NavItem),
}

/// What a key did, for the app to act on: the palette cannot reach the session or the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The input changed; the ranking has to be recomputed against the session.
    Edited,
    Moved,
    /// `enter` on this entry, with every word after the command word.
    Chose(Entry, Vec<String>),
    /// The ghost was accepted: the input changed, so the list has to be recomputed. Separate
    /// from `Edited` only so the app can skip the history reset it does not need; both
    /// re-`refresh`.
    Completed,
    Cancelled,
    Ignored,
}

/// Kinds listed at once. Well past a screenful, so the list is bounded without the ranking
/// ever being cut where the user can see it.
const MAX_KINDS: usize = 30;

/// The maximal runs of non-space characters, with the byte index each starts at.
///
/// Only `' '` separates. `Key::Tab` is a key in this app and never a character, so a tab cannot
/// be in the line to begin with. [`Palette::head`] and [`Palette::args`] read these same runs
/// rather than `str::split_whitespace`, so the grammar and the words handed to the app cannot
/// disagree about where a word ends: `split_whitespace` splits on every `char::is_whitespace`,
/// including the U+00A0 a paste can carry, and this splits on `' '` alone.
fn word_runs(input: &str) -> Vec<(usize, &str)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, c) in input.char_indices() {
        match (c == ' ', start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                runs.push((s, &input[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, &input[s..]));
    }
    runs
}

/// The run the cursor touches and its position: a cursor immediately after a run belongs to that
/// run, and a cursor in a gap starts an empty word at the position after the runs behind it.
/// The **position** of a word is the number of runs strictly before it (design §5.1).
fn word_at<'a>(runs: &[(usize, &'a str)], cursor: usize) -> (usize, &'a str) {
    for (i, (start, word)) in runs.iter().enumerate() {
        if cursor >= *start && cursor <= start + word.len() {
            return (i, word);
        }
    }
    (
        runs.iter().filter(|(s, w)| s + w.len() < cursor).count(),
        "",
    )
}

/// The completion position and the word at it, after §5.1's clamp.
///
/// **A trailing empty word never advances the completion position.** The synthetic word exists so
/// that `:ctx ` opens the context list, and for nothing else. Past the last slot of the head
/// command it is clamped to the last slot and the word being completed stays the last **non-empty**
/// one - which is what keeps `a_space_after_ctx_completes_the_context_name` green, because that
/// test asserts `:ctx EU ` *keeps* its one-row value list. Only a trailing *empty* word is
/// clamped; a real extra argument is a real extra argument, and `App::choose` refuses it.
fn completing(input: &str, cursor: usize, slots: usize) -> (usize, &str) {
    let runs = word_runs(input);
    let (position, word) = word_at(&runs, cursor);
    if word.is_empty() && position > slots {
        // The last run **at its own position**, not at the last slot: `:ctx a b ` completes `b`
        // at position 2, exactly as `:ctx a b` does, and §5.2's collapse arm handles a position
        // with no slot. Clamping to the slot instead would offer contexts for `b` and then have
        // `App::choose` refuse the row it had just offered.
        let last = runs.len().saturating_sub(1);
        return (last, runs.last().map_or("", |(_, w)| *w));
    }
    (position, word)
}

/// The command a head **word** names exactly, case-insensitively. `None` for a kind, a page or
/// anything nothing knows - all of which have no arguments, so all of which have no slots.
///
/// It is given one word and never a line: [`Palette::head`] is the one place a line is split.
fn head_command(head_word: &str) -> Option<Command> {
    COMMANDS
        .iter()
        .copied()
        .find(|c| c.label().eq_ignore_ascii_case(head_word))
}

/// What the app can offer without a network round trip, rebuilt on every keystroke.
///
/// One struct rather than three parameters because the list only grows: `refresh` is called from
/// one place, and a fourth vocabulary should not be a fourth argument at every call site and in
/// every test.
pub struct Vocabulary<'a> {
    /// The session's reason for a kind this Prism Central does not serve; `None` when served.
    pub unavailable: &'a dyn Fn(&Kind) -> Option<String>,
    /// What `:ctx ` completes from: the rows the Contexts screen shows, never a fresh `list()`
    /// - that reads the config file and probes every secret store, and this runs per keystroke.
    pub contexts: Vec<&'a str>,
    /// The action names of the table under the palette, for `Slot::Actions`. Nothing reads them
    /// yet. The seam is where the vocabulary is *built*.
    pub actions: Vec<&'a str>,
    /// What `:show ` completes from: the `[nav] hide` list as it stands. It has to come from the
    /// app because it is the user's, not the catalog's - and it is the only in-TUI way back from
    /// a group hidden whole, which leaves no header, no count and no name anywhere on screen.
    pub hidden: Vec<&'a str>,
}

fn nothing_unavailable(_: &Kind) -> Option<String> {
    None
}

impl<'a> Vocabulary<'a> {
    /// Nothing unavailable, no actions, and the context names given: what a unit test ranks
    /// against.
    pub fn of(contexts: &'a [&'a str]) -> Vocabulary<'a> {
        static NONE: fn(&Kind) -> Option<String> = nothing_unavailable;
        Vocabulary {
            unavailable: &NONE,
            contexts: contexts.to_vec(),
            actions: Vec::new(),
            hidden: Vec::new(),
        }
    }
}

impl Vocabulary<'static> {
    /// `of(&[])`: the whole catalog, nothing refused, no names.
    pub fn empty() -> Vocabulary<'static> {
        Vocabulary::of(&[])
    }
}

#[derive(Debug, Default)]
pub struct Palette {
    pub input: Input,
    pub selected: usize,
    pub entries: Vec<Entry>,
    /// The slot being completed, for the popup's title. `None` at the head, and `None` when a
    /// slot completed nothing and the list collapsed to the command row.
    pub slot: Option<Slot>,
    /// The lines this context has accepted, newest last, loaded once when the palette opens.
    history: Vec<String>,
    /// `None` while editing, `Some(i)` while showing `history[i]`.
    history_pos: Option<usize>,
    /// The line that was being typed when the first `ctrl-p` fired.
    stash: Option<String>,
}

impl Entry {
    /// The strings that, typed in full, would select this row.
    ///
    /// A kind is reachable by its id, its display and any alias - `Subnets` is a row you reach
    /// by typing `subnets`, even though the id is `networking.config.Subnet`, because `subnets`
    /// is one of that kind's curated aliases.
    ///
    /// The nav labels are deliberately **not** here. A label is matching material - `head_ranking`
    /// walks `NAV` so `:policies` finds `Routing Policies` - but of the 220 labels in the catalog
    /// 150 carry a space, which [`completion`](Self::completion) refuses, and all 70 that do not
    /// are already the id, the display or an alias of their own kind. Folding them in would walk
    /// `NAV` per row per frame to add nothing.
    fn candidates(&self) -> Vec<&str> {
        match self {
            Entry::Command(c) => vec![c.label()],
            Entry::Kind { kind, .. } => {
                let mut all = vec![kind.id, kind.display];
                all.extend(kind.aliases.iter().copied());
                all
            }
            Entry::Page(def) => vec![def.id, def.title],
            Entry::Value { text, .. } => vec![text.as_str()],
            Entry::Missing(item) => vec![item.label],
        }
    }

    /// The shortest candidate `word` prefixes, case-insensitively, ties broken alphabetically.
    ///
    /// **A candidate containing a space is not a candidate.** The grammar is space-separated, so
    /// a ghost of `Recovery Points` would produce a two-word input whose second word is parsed
    /// as an argument to an unknown head. A two-word `display` is therefore matching material
    /// but not completion material; ids and aliases carry the ghost. That is what makes
    /// `:policies` show a full popup and no ghost - correct, and visibly so.
    pub(crate) fn completion(&self, word: &str) -> Option<&str> {
        self.candidates()
            .into_iter()
            .filter(|c| !c.contains(' ') && starts_ci(c, word))
            .min_by(|a, b| (a.len(), *a).cmp(&(b.len(), *b)))
    }
}

impl Palette {
    /// A palette that remembers what this context accepted. `Palette::default()` is the same
    /// thing with an empty history, which is what a session with no cache directory has.
    pub fn with_history(history: Vec<String>) -> Palette {
        Palette {
            history,
            ..Palette::default()
        }
    }

    pub fn key(&mut self, key: Key) -> Action {
        match key {
            Key::Esc => Action::Cancelled,
            // `tab` always has a job: it completes when there is something to complete, and
            // otherwise it moves the selection down exactly as it always has, so no existing
            // test loses its key.
            Key::Tab => {
                if self.accept_ghost() {
                    Action::Completed
                } else {
                    self.down();
                    Action::Moved
                }
            }
            // `→` at the end of the input is the only place a ghost can be; anywhere else it is
            // an ordinary cursor move and falls through to `Input::key` below.
            Key::Right if self.input.at_end() => {
                if self.accept_ghost() {
                    Action::Completed
                } else {
                    Action::Ignored
                }
            }
            Key::Down => {
                self.down();
                Action::Moved
            }
            Key::Up | Key::BackTab => {
                self.selected = self.selected.saturating_sub(1);
                Action::Moved
            }
            Key::Enter => match self.current() {
                Some(entry) => Action::Chose(
                    entry.clone(),
                    self.args().into_iter().map(str::to_string).collect(),
                ),
                None => Action::Ignored,
            },
            // Above the editing catch-all, and reachable only because `Input::key` answers
            // `None` for both: the other three surfaces over that one key map are untouched.
            Key::Ctrl('p') => self.recall_back(),
            Key::Ctrl('n') => self.recall_forward(),
            // Every editing key, from the one implementation `text.rs` owns.
            key => match self.input.key(key) {
                Some(Edit::Changed) => self.edited(),
                Some(Edit::Moved) => Action::Moved,
                Some(Edit::Ignored) | None => Action::Ignored,
            },
        }
    }

    /// What every edit does beyond the edit itself: the ranking is about to change, so the
    /// selection goes back to the top row - and the recall is over, because a recalled line
    /// that has been edited is the line being typed.
    fn edited(&mut self) -> Action {
        self.history_pos = None;
        self.stash = None;
        self.selected = 0;
        Action::Edited
    }

    /// First press stashes the line being typed and shows the newest entry; later presses walk
    /// back. At the oldest entry it stops.
    fn recall_back(&mut self) -> Action {
        let next = match self.history_pos {
            None if self.history.is_empty() => return Action::Ignored,
            None => {
                self.stash = Some(self.input.as_str().to_string());
                self.history.len() - 1
            }
            Some(0) => return Action::Ignored,
            Some(i) => i - 1,
        };
        self.history_pos = Some(next);
        self.input.set(self.history[next].clone());
        self.selected = 0;
        Action::Edited
    }

    /// Walks forward; past the newest it restores the stash, clears it and goes back to
    /// editing.
    fn recall_forward(&mut self) -> Action {
        let Some(i) = self.history_pos else {
            return Action::Ignored;
        };
        match self.history.get(i + 1) {
            Some(line) => {
                self.history_pos = Some(i + 1);
                self.input.set(line.clone());
            }
            None => {
                self.history_pos = None;
                self.input.set(self.stash.take().unwrap_or_default());
            }
        }
        self.selected = 0;
        Action::Edited
    }

    /// One row down, and never past the last one. Two keys move the selection down - `↓` always,
    /// `tab` when there is no ghost to take - and one clamp is one place to get the saturating
    /// arithmetic right when the list grows a header or a footer row.
    fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
    }

    /// The head command, its slots, the completion position and the word at it: the grammar's
    /// reading of the line as it stands, after §5.1's clamp.
    ///
    /// One derivation, two readers. [`refresh`](Self::refresh) ranks that word and
    /// [`ghost`](Self::ghost) completes it, and the two reading the line differently is what
    /// has the prompt offer the rest of a row the popup is not showing.
    fn completing_at_cursor(&self) -> (Option<Command>, &'static [Slot], usize, &str) {
        let head = head_command(self.head());
        let slots = head.map_or(&[][..], Command::slots);
        let (position, word) = completing(self.input.as_str(), self.input.cursor(), slots.len());
        (head, slots, position, word)
    }

    /// Recompute the list for the current input, against what the app can offer without a
    /// network round trip.
    ///
    /// One rule, two outcomes. The **position** of the word under the cursor decides which
    /// vocabulary it draws on: position 0 is the head - commands, then the kinds `lookup` finds,
    /// then the pages, in one ranking - and a position from 1 on is
    /// `head_command.slots()[position - 1]`, when the head resolves to a command with that many
    /// slots. A word with no slot completes nothing: the single row is the head command itself,
    /// and `enter` hands the app every word so the app can produce the exact refusal. A head
    /// that is no command has no row of its own to collapse to, so it keeps the ranking its own
    /// word earns: `:disaster recovery` offers the Disaster Recovery page just as `:disaster`
    /// does.
    ///
    /// A function of the cursor as much as of the text - the word being completed is the word
    /// the cursor stands in - which is why the app re-ranks on `Action::Moved` as well as on
    /// `Action::Edited`.
    ///
    /// `refresh`'s three fields are assigned from locals rather than in place, so the borrow
    /// that `word` holds is over before any of them is written.
    pub fn refresh(&mut self, vocab: &Vocabulary<'_>) {
        let (head, slots, position, word) = self.completing_at_cursor();
        let (slot, entries) = if position == 0 {
            (None, head_ranking(word, vocab))
        } else {
            // `position >= 1` here, so the slot is the one this argument position names.
            let slot = slots.get(position - 1).copied();
            let rows = slot.map(|s| vocabulary(s, word, vocab)).unwrap_or_default();
            if rows.is_empty() {
                // What nothing completes is still the command as typed: `:ctx nope` has to reach
                // the app to be told there is no such context, and `:skin` with an alias the list
                // does not spell has to reach it to be resolved. With no command to collapse to,
                // the head is ranked instead - an empty popup would be neither an offer nor a
                // refusal.
                let row = head.map_or_else(
                    || head_ranking(self.head(), vocab),
                    |c| vec![Entry::Command(c)],
                );
                (None, row)
            } else {
                (slot, rows)
            }
        };
        self.slot = slot;
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }

    /// The `rows` entries to draw and the index the first of them has, always framing the
    /// selection: a list longer than the box would otherwise hide the cursor.
    pub fn window(&self, rows: usize) -> (usize, &[Entry]) {
        window(&self.entries, self.selected, rows)
    }

    /// Every word after the head word: the one of `ctx NAME` and `skin NAME`, the two of
    /// `can-i ACTION KIND`. One accessor for all of them - the app reads `first()` where a
    /// command takes one - so a command that grows a second argument changes no signature.
    ///
    /// Reads [`word_runs`], which the grammar reads too: the app is handed exactly the words
    /// `refresh` ranked, and a trailing space adds none of its own.
    pub fn args(&self) -> Vec<&str> {
        word_runs(self.input.as_str())
            .into_iter()
            .skip(1)
            .map(|(_, w)| w)
            .collect()
    }

    /// The head word, position 0: what decides which grammar the rest of the line follows.
    pub fn head(&self) -> &str {
        word_runs(self.input.as_str())
            .into_iter()
            .next()
            .map_or("", |(_, w)| w)
    }

    /// The word the cursor is in, and its position - before §5.1's clamp, which is
    /// `refresh`'s business and not a caller's. `pub` rather than `pub(crate)` because its only
    /// caller today is a test, and `pub(crate)` with no caller in the crate is `dead_code`,
    /// which `-D warnings` denies.
    pub fn word_at_cursor(&self) -> (usize, &str) {
        let runs = word_runs(self.input.as_str());
        word_at(&runs, self.input.cursor())
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    /// The selected row's remainder after the word under the cursor, drawn dim after the block.
    ///
    /// The ghost is the **selected** row's completion, so the prompt and the list can never point
    /// at different things: `key` sets `selected = 0` on every edit, so before the user moves,
    /// "selected" *is* "top match", and after they move the two follow each other.
    ///
    /// `None` in five cases, each of which would otherwise offer a remainder that does not belong
    /// where it is drawn:
    ///
    /// - **The cursor is not at the end of the input**, so a ghost and a cursor can never fight
    ///   and there is no such thing as a ghost mid-string that `→` would have to walk through.
    /// - **The line ends in a space.** `completing`'s clamp deliberately hands back the last
    ///   *non-empty* run there, so the word ends before the cursor: `:skin gruv ` would draw
    ///   `box-dark` a space away from the `gruv` it completes and `accept_ghost` would insert it
    ///   there. With `at_end()` already required, "ends in a space" is exactly "the clamped word
    ///   does not end at the cursor".
    /// - **The word is empty.**
    /// - **The rows are not about that word.** `refresh` sets `slot` exactly when an argument
    ///   vocabulary produced the rows; it is `None` at the head (position 0, where the rows *are*
    ///   that word's ranking) and `None` again in §5.2's collapse branch, where a slot completed
    ///   nothing and the rows describe the *head* instead. `:subnet s` ranks `Subnets` from
    ///   `subnet` and would then complete `s` against it - `:subnet s█ubnet`, the rest of a row
    ///   that is not about the word it is drawn against.
    /// - **The remainder is empty** (**R2**). The common case rather than a corner: `:vm` matches
    ///   the alias `vm` exactly.
    pub(crate) fn ghost(&self) -> Option<&str> {
        if !self.input.at_end() || self.input.as_str().ends_with(' ') {
            return None;
        }
        let (_, _, position, word) = self.completing_at_cursor();
        if word.is_empty() || (position > 0 && self.slot.is_none()) {
            return None;
        }
        let candidate = self.current()?.completion(word)?;
        // By characters, not bytes: a context name may not be ASCII. `starts_ci` folds ASCII
        // only, so the matched prefix is byte-identical either way, but the offset is taken from
        // `char_indices` so it can never land inside a character.
        let skip = candidate
            .char_indices()
            .nth(word.chars().count())
            .map_or(candidate.len(), |(i, _)| i);
        let rest = &candidate[skip..];
        (!rest.is_empty()).then_some(rest)
    }

    /// Insert the ghost at the cursor, and a single space after it when the accepted row is a
    /// command with at least one slot - so `:ct` + `tab` becomes `:ctx ` and the context list
    /// opens in the same keystroke. `quit` and `help` have no slots and get no space.
    ///
    /// The ghost is only ever *appended*: typed characters keep their own casing, and accepting
    /// `SUBN` + `et` yields `SUBNet`, which still resolves because every vocabulary is matched
    /// case-insensitively.
    fn accept_ghost(&mut self) -> bool {
        let Some(ghost) = self.ghost().map(str::to_string) else {
            return false;
        };
        let command = match self.current() {
            Some(Entry::Command(c)) => Some(*c),
            _ => None,
        };
        self.input.insert_str(&ghost);
        if command.is_some_and(|c| !c.slots().is_empty()) {
            self.input.insert(' ');
        }
        // An accepted ghost changed the line, so the recall is over for the reason an edit
        // ends it: what is on the prompt is being typed, not being remembered.
        self.history_pos = None;
        self.stash = None;
        self.selected = 0;
        true
    }
}

/// The members of a fixed set that contain what was typed, case-insensitively.
fn values<'a>(all: impl IntoIterator<Item = &'a str>, typed: &str, tag: Tag) -> Vec<Entry> {
    all.into_iter()
        .filter(|v| contains_ci(v, typed))
        .map(|v| Entry::Value {
            text: v.to_string(),
            tag,
        })
        .collect()
}

/// The built-ins whose canonical name **or alias** contains what was typed, offered under the
/// canonical name - the spelling `enter` applies, the config records and the `:skin` list marks.
///
/// `theme::canonical` has always resolved `onedark` to `one-dark`, but `BUILTIN_NAMES` holds only
/// the seventeen canonical spellings, so `:skin onedark` completed nothing while still working on
/// `enter`: a list that lied about what it accepts. The four hyphen-free aliases -
/// `tokyonight`, `onedark`, `rosepine`, `rosepinedawn` - are the ones that were unreachable; the
/// other seven happen to be substrings of their own canonical names and always worked.
///
/// The row offered is canonical, so there is no new drawn shape and usually no ghost: `one-dark`
/// does not start with `onedark`, and the never-rewrite rule declines to invent one.
fn skins(typed: &str) -> Vec<Entry> {
    let matches = |s: &str| contains_ci(s, typed);
    crate::theme::BUILTIN_NAMES
        .iter()
        .filter(|name| {
            matches(name)
                || crate::theme::SKIN_ALIASES
                    .iter()
                    .any(|(alias, canon)| *canon == **name && matches(alias))
        })
        .map(|name| Entry::Value {
            text: (*name).to_string(),
            tag: Tag::Skin,
        })
        .collect()
}

/// The rows a slot offers for the word being completed.
///
/// `Actions` and `Kinds` are declared with a noun and no vocabulary in this phase (design §15):
/// the administration design owns the field an action is invoked by and the `:can-i` arm, so a
/// word in one of those slots completes nothing and falls into §5.2's last row - which is
/// exactly what `:can-i power-off vm` does today, so no behaviour changes and no test moves.
fn vocabulary(slot: Slot, word: &str, vocab: &Vocabulary<'_>) -> Vec<Entry> {
    match slot {
        Slot::Contexts => values(vocab.contexts.iter().copied(), word, Tag::Context),
        Slot::Skins => skins(word),
        Slot::Hide => hideable(word),
        Slot::Show => values(vocab.hidden.iter().copied(), word, Tag::Nav { hide: false }),
        // Rendered through `refresh::show`, so a rung is spelled once wherever it appears.
        Slot::Intervals => intervals(word),
        Slot::Levels => levels(word),
        Slot::Actions | Slot::Kinds => Vec::new(),
    }
}

/// The refresh ladder, drawn through `nutsh_core::refresh::show` so a rung is spelled in one
/// place: `auto`, `5s`, `10s`, `30s`, `1m`, `5m`, `off`. The rows are a suggestion - `:refresh`
/// also takes any number the parser accepts - so nothing here has to enumerate the seconds.
///
/// Matched on the **prefix**, not anywhere in the row, which is the one place this file departs
/// from [`contains_ci`]. A rung is a number and a substring match reads a digit out of the
/// middle of one: `0` would offer `10s` and `30s`, and `enter` would then run a rung the person
/// never typed instead of letting `0` reach the parser and be refused by the floor.
fn intervals(typed: &str) -> Vec<Entry> {
    let typed = typed.to_ascii_lowercase();
    nutsh_core::refresh::LADDER
        .iter()
        .copied()
        .map(nutsh_core::refresh::show)
        .filter(|v| v.to_ascii_lowercase().starts_with(&typed))
        .map(|text| Entry::Value {
            text,
            tag: Tag::Interval,
        })
        .collect()
}

/// The six levels, spelled by `LogLevel::word` so the palette, the config file and `NUTSH_LOG`
/// cannot disagree about them. Prefix-matched for the reason [`intervals`] is: the words share
/// letters, and a substring match would offer `error` for the `r` in `trace`.
fn levels(typed: &str) -> Vec<Entry> {
    let typed = typed.to_ascii_lowercase();
    [
        nutsh_core::contexts::LogLevel::Off,
        nutsh_core::contexts::LogLevel::Error,
        nutsh_core::contexts::LogLevel::Warn,
        nutsh_core::contexts::LogLevel::Info,
        nutsh_core::contexts::LogLevel::Debug,
        nutsh_core::contexts::LogLevel::Trace,
    ]
    .into_iter()
    .map(|l| l.word())
    .filter(|w| w.starts_with(&typed))
    .map(|text| Entry::Value {
        text: text.to_string(),
        tag: Tag::Level,
    })
    .collect()
}

/// What `:hide` names: every group the menu draws, then the page or kind behind each of its
/// items, each offered once and in the order the menu draws them.
///
/// Matched on the id **or** on the label beside it, so `Volume Groups` finds
/// `volumes.config.VolumeGroup` - the same courtesy [`skins`] does an alias, and the row offered
/// is the spelling `enter` applies. The Dashboard is left out: it cannot be hidden, and offering
/// a row that `enter` refuses would be worse than offering none.
fn hideable(typed: &str) -> Vec<Entry> {
    let mut seen: HashSet<&'static str> = HashSet::new();
    let mut out = Vec::new();
    for group in nutsh_catalog::NAV {
        let items = group.items.iter().filter_map(|i| match i.target {
            NavTarget::Kind(id) | NavTarget::Page(id) => Some((id, i.label)),
            // Neither screen is ever hidden, and a `Missing` note has nothing behind it.
            NavTarget::Contexts | NavTarget::Settings | NavTarget::Missing => None,
        });
        for (id, label) in std::iter::once((group.name, group.name)).chain(items) {
            if crate::sidebar::protected(id) || !seen.insert(id) {
                continue;
            }
            if contains_ci(id, typed) || contains_ci(label, typed) {
                out.push(Entry::Value {
                    text: id.to_string(),
                    tag: Tag::Nav { hide: true },
                });
            }
        }
    }
    out
}

/// A candidate row before the sort. Every field is a number or a `&'static str` out of the
/// catalog, so the several hundred rows a broad word like `:e` builds per keystroke own nothing
/// between them. The one `String` a row can carry - the reason a kind is greyed - is filled in
/// after the sort, for the thirty rows that survive it.
struct Row {
    rank: u8,
    sink: u8,
    class: u8,
    route: u8,
    /// A command's index in [`COMMANDS`], which breaks the tie inside a command's rank before
    /// `key` alphabetises it: `:` then `enter` opens the Contexts screen, not `:can-i`.
    /// [`SEQ_NONE`] for every other class, where `key` decides on its own.
    seq: u16,
    key: &'static str,
    entry: Entry,
}

/// A command is a verb the user typed on purpose, so it leads its rank.
const CLASS_COMMAND: u8 = 0;
// Class 1 is missing on purpose: it is where `Entry::Value` would sit. Values are never ranked
// - an argument list is filtered in its vocabulary's own order and never merged with anything -
// so the constant would be dead code.
const CLASS_KIND: u8 = 2;
/// A page and a `Missing` nav item share the last class; `sink` has already separated them.
const CLASS_PAGE: u8 = 3;
/// A `Missing` row sinks below `NeedsParameter`, which is as far down as `sink` goes.
const SINK_MISSING: u8 = 3;
/// A row with no declaration order to keep, which is every row that is not a command.
const SEQ_NONE: u16 = u16::MAX;
/// Where a row's rank came from, which breaks the tie inside a rank (design deviation 1).
const ROUTE_CATALOG: u8 = 0;
const ROUTE_NAV_LABEL: u8 = 1;
const ROUTE_NAV_WORD: u8 = 2;

/// `haystack` starts with `needle`, ASCII case folded.
///
/// The catalog has exactly this function and keeps it private (`lib.rs:850`), and this plan does
/// not modify `crates/catalog`, so the palette carries its own two lines rather than the catalog
/// growing an export for one caller.
fn starts_ci(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .get(..needle.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(needle.as_bytes()))
}

/// `haystack` contains `needle`, ASCII case folded. An empty needle is contained by everything.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// The rank `lookup` gave this kind for `word`, re-derived - `lookup` returns its list in rank
/// order but not the ranks, and one merged sort needs the number - or `None` for a kind none of
/// `lookup`'s five tests matches.
///
/// Total against those five on purpose: a sixth rank added to `lookup` answers `None` here
/// rather than folding silently into rank 4, which is what
/// `the_derived_rank_agrees_with_lookups_order` catches.
fn lookup_rank(kind: &Kind, word: &str) -> Option<u8> {
    Some(if kind.id.eq_ignore_ascii_case(word) {
        0
    } else if kind.aliases.iter().any(|a| a.eq_ignore_ascii_case(word)) {
        if kind.curated { 1 } else { 2 }
    } else if starts_ci(kind.display, word) {
        3
    } else if contains_ci(kind.id, word) {
        4
    } else {
        return None;
    })
}

/// How a nav label matches, and how strongly. A label is another display name for a target the
/// catalog already has, so it borrows the rank `lookup` would have given the same match on
/// `display`; `route` records which of the two tests at rank 3 it was.
///
/// `lookup` itself is unchanged. Adding a display-substring rank 5 was considered and rejected:
/// it would add ~200 rows to a broad word like `:s` without adding one reachable kind, because
/// the 220 nav labels already name every curated target with a better label than `display`.
fn nav_rank(label: &str, word: &str) -> Option<(u8, u8)> {
    if label.eq_ignore_ascii_case(word) {
        return Some((0, ROUTE_NAV_LABEL));
    }
    if starts_ci(label, word) {
        return Some((3, ROUTE_NAV_LABEL));
    }
    if label
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| !w.is_empty() && starts_ci(w, word))
    {
        return Some((3, ROUTE_NAV_WORD));
    }
    if contains_ci(label, word) {
        return Some((4, ROUTE_NAV_WORD));
    }
    None
}

fn kind_row(kind: &'static Kind, rank: u8, route: u8) -> Row {
    Row {
        rank,
        // 0/1/2 reproduce `lookup`'s `(needs_param, from_parent)` pair exactly:
        // `(false,false) < (false,true) < (true,false)` is `0 < 1 < 2`.
        sink: match reach(kind) {
            Reach::Direct => 0,
            Reach::FromParent(_) => 1,
            Reach::NeedsParameter => 2,
        },
        class: CLASS_KIND,
        route,
        seq: SEQ_NONE,
        key: kind.id,
        // `reason: None` until the sort is over: the session's half of it takes the client's
        // status lock, and a broad word ranks hundreds of kinds to show thirty.
        entry: Entry::Kind { kind, reason: None },
    }
}

fn page_row(def: &'static PageDef, rank: u8, route: u8) -> Row {
    Row {
        rank,
        sink: 0,
        class: CLASS_PAGE,
        route,
        seq: SEQ_NONE,
        key: def.id,
        entry: Entry::Page(def),
    }
}

/// A command row, from the command vocabulary or from the `Contexts` nav item that resolves to
/// one, with `COMMANDS`' declaration order kept as `seq`.
fn command_row(command: Command, rank: u8, route: u8) -> Row {
    Row {
        rank,
        sink: 0,
        class: CLASS_COMMAND,
        route,
        seq: COMMANDS
            .iter()
            .position(|c| *c == command)
            .map_or(SEQ_NONE, |i| i as u16),
        key: command.label(),
        entry: Entry::Command(command),
    }
}

/// The target a row resolves to. Two routes to the same table are one row, at the better rank -
/// which is load-bearing for a kind matched by `lookup` *and* by its nav label, for the
/// `Contexts` nav item resolving to `Command::Ctx`, and for a page matched by its title and by a
/// nav label pointing at the same `PageDef`.
///
/// `'static` rather than borrowed from the row, so the set of what has been seen outlives the
/// rows it was read from and a surviving entry is moved into the result rather than cloned.
fn target(row: &Row) -> (u8, &'static str) {
    match row.entry {
        Entry::Command(c) => (0, c.label()),
        Entry::Kind { kind, .. } => (2, kind.id),
        Entry::Page(def) => (3, def.id),
        Entry::Missing(item) => (4, item.label),
        // `head_ranking` builds no value row - an argument list is filtered in its vocabulary's
        // own order and never merged with anything - and a row's `key` is its own name in every
        // arm above, so this one is total rather than unreachable.
        Entry::Value { .. } => (1, row.key),
    }
}

/// The head's ranking: commands, the kinds `lookup` finds, the 220 nav labels and the 2 pages,
/// merged into one list sorted by `(rank, sink, class, route, seq, key)` and deduplicated on the
/// resolved target.
///
/// Pages rank on the same scale rather than being appended last: a title or id **prefix** is
/// rank 3, the strength a display prefix has, and a **substring** is rank 4, the strength an id
/// substring has. `MAX_KINDS` is a cap on the merged kinds, applied after the sort, so the cut is
/// never visible.
fn head_ranking(word: &str, vocab: &Vocabulary<'_>) -> Vec<Entry> {
    let mut rows: Vec<Row> = Vec::new();
    // An exact label is rank 0, so `:ctx` outranks `:can-i` for the word `ctx`, and a prefix is
    // rank 3, the strength a display prefix has. The case folding is what the lowercased needle
    // this replaced already did - every label is lowercase ASCII - so `:CTX` matches as before,
    // and with an empty word every command is rank 3 and `seq` keeps `COMMANDS`' own order.
    for c in COMMANDS {
        let label = c.label();
        let rank = if label.eq_ignore_ascii_case(word) {
            0
        } else if word.is_empty() || starts_ci(label, word) {
            3
        } else {
            continue;
        };
        rows.push(command_row(*c, rank, ROUTE_CATALOG));
    }
    if !word.is_empty() {
        for kind in lookup(word) {
            // `lookup` returned it, so it matched something; a rank this function cannot derive
            // is one `lookup` grew and the palette has not learned, and it sorts under
            // everything rather than being dropped.
            let rank = lookup_rank(kind, word).unwrap_or(u8::MAX);
            rows.push(kind_row(kind, rank, ROUTE_CATALOG));
        }
        for def in nutsh_catalog::PAGES {
            let rank = if starts_ci(def.title, word) || starts_ci(def.id, word) {
                3
            } else if contains_ci(def.title, word) || contains_ci(def.id, word) {
                4
            } else {
                continue;
            };
            rows.push(page_row(def, rank, ROUTE_CATALOG));
        }
        for group in nutsh_catalog::NAV {
            for item in group.items {
                let Some((rank, route)) = nav_rank(item.label, word) else {
                    continue;
                };
                match item.target {
                    NavTarget::Kind(id) => {
                        if let Some(kind) = nutsh_catalog::kind(id) {
                            rows.push(kind_row(kind, rank, route));
                        }
                    }
                    NavTarget::Page(id) => {
                        if let Some(def) = nutsh_catalog::page(id) {
                            rows.push(page_row(def, rank, route));
                        }
                    }
                    NavTarget::Contexts => rows.push(command_row(Command::Ctx, rank, route)),
                    NavTarget::Settings => rows.push(command_row(Command::Settings, rank, route)),
                    NavTarget::Missing => rows.push(Row {
                        rank,
                        sink: SINK_MISSING,
                        class: CLASS_PAGE,
                        route,
                        seq: SEQ_NONE,
                        key: item.label,
                        entry: Entry::Missing(item),
                    }),
                }
            }
        }
    }
    rows.sort_by(|a, b| {
        (a.rank, a.sink, a.class, a.route, a.seq, a.key)
            .cmp(&(b.rank, b.sink, b.class, b.route, b.seq, b.key))
    });
    let mut seen: HashSet<(u8, &'static str)> = HashSet::new();
    let mut out: Vec<Entry> = Vec::new();
    let mut kinds = 0usize;
    for row in rows {
        if !seen.insert(target(&row)) {
            continue;
        }
        let mut entry = row.entry;
        if let Entry::Kind { kind, reason } = &mut entry {
            kinds += 1;
            if kinds > MAX_KINDS {
                continue;
            }
            // The reason the row is greyed, asked only of the rows that reached the list:
            // `unavailable` takes the client's status lock, and a word as broad as `:e` ranks
            // hundreds of kinds to show thirty of them.
            let k: &'static Kind = kind;
            *reason = reach(k).reason().or_else(|| (vocab.unavailable)(k));
        }
        out.push(entry);
    }
    out
}

/// The `rows`-long slice of `entries` that contains `selected`, and where it starts. The
/// selection sits at the bottom edge while it moves down and at the top while it moves up,
/// which is what a list that scrolls one line at a time looks like.
pub(crate) fn window<T>(entries: &[T], selected: usize, rows: usize) -> (usize, &[T]) {
    if rows == 0 {
        return (0, &[]);
    }
    let offset = (selected + 1).saturating_sub(rows);
    let end = (offset + rows).min(entries.len());
    (offset, &entries[offset.min(end)..end])
}

/// The `rows`-long slice of `entries` that keeps `selected` in the middle of the pane, and
/// where it starts. Rides an edge only at the two ends of the list.
///
/// Used by the sidebar and by nothing else. A table keeps [`window`]: with a hundred VM rows,
/// centring would make the first `j` scroll the whole table under the cursor, which is a
/// different and worse feeling - and it would re-record every table snapshot for no gain.
pub(crate) fn centred<T>(entries: &[T], selected: usize, rows: usize) -> (usize, &[T]) {
    if rows == 0 {
        return (0, &[]);
    }
    if entries.len() <= rows {
        return (0, entries);
    }
    let half = (rows - 1) / 2;
    let offset = selected.saturating_sub(half).min(entries.len() - rows);
    (offset, &entries[offset..offset + rows])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_frames_the_selection() {
        let entries: Vec<usize> = (0..20).collect();
        assert_eq!(window(&entries, 0, 5), (0, &entries[0..5]));
        assert_eq!(window(&entries, 4, 5), (0, &entries[0..5]));
        assert_eq!(window(&entries, 5, 5), (1, &entries[1..6]));
        assert_eq!(window(&entries, 19, 5), (15, &entries[15..20]));
        assert_eq!(window(&entries, 0, 0), (0, &entries[0..0]));
        let empty: [usize; 0] = [];
        assert_eq!(window(&empty, 0, 5), (0, &empty[..]));
    }

    /// The menu centres its selection: it is a list you live in, not a popup you scan, and
    /// "when we go down only the last goes down" is what `window` feels like in one.
    ///
    /// `half = (rows - 1) / 2` rather than `rows / 2`: for an odd pane the selection is the
    /// exact middle row, and for an even pane it is the upper of the two middle rows, which
    /// leaves one more row visible below the cursor - the direction you are usually
    /// travelling.
    #[test]
    fn the_menu_centres_its_selection() {
        let entries: Vec<usize> = (0..20).collect();
        // rows = 5, half = 2.
        assert_eq!(
            centred(&entries, 0, 5),
            (0, &entries[0..5]),
            "rides the top"
        );
        assert_eq!(centred(&entries, 1, 5), (0, &entries[0..5]));
        assert_eq!(
            centred(&entries, 2, 5),
            (0, &entries[0..5]),
            "first centred"
        );
        assert_eq!(centred(&entries, 3, 5), (1, &entries[1..6]));
        assert_eq!(centred(&entries, 10, 5), (8, &entries[8..13]), "middle");
        assert_eq!(centred(&entries, 18, 5), (15, &entries[15..20]), "clamped");
        assert_eq!(
            centred(&entries, 19, 5),
            (15, &entries[15..20]),
            "rides the bottom"
        );
        // rows = 6, half = 2: the upper of the two middle rows.
        assert_eq!(centred(&entries, 10, 6), (8, &entries[8..14]));
        // A pane with no room; nothing indexes into the window.
        assert_eq!(centred(&entries, 3, 0), (0, &entries[0..0]));
        // The whole tree fits: no artificial centring, the cursor sits where it sits.
        let short: Vec<usize> = (0..4).collect();
        assert_eq!(centred(&short, 3, 10), (0, &short[..]));
        assert_eq!(centred(&short, 0, 4), (0, &short[..]));
        // The `/` filter can empty the list; `Sidebar::refresh` already clamps `selected`.
        let empty: [usize; 0] = [];
        assert_eq!(centred(&empty, 0, 5), (0, &empty[..]));
        // One row: half = 0, so the one visible row is the selected one.
        assert_eq!(centred(&entries, 7, 1), (7, &entries[7..8]));
    }

    #[test]
    fn q_ranks_the_quit_command_first() {
        let mut p = Palette {
            input: "q".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert_eq!(p.current(), Some(&Entry::Command(Command::Quit)));
    }

    /// `ctx` alone is the command; `ctx ` with a space after it is its argument, completed
    /// from the names given, narrowed by what follows, and `enter` carries the whole name.
    #[test]
    fn a_space_after_ctx_completes_the_context_name() {
        let names = ["lab", "prod-eu"];
        let vocab = Vocabulary::of(&names);
        let mut p = Palette {
            input: "ctx".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert_eq!(p.current(), Some(&Entry::Command(Command::Ctx)));
        p.input.insert(' ');
        p.refresh(&vocab);
        assert_eq!(p.entries.len(), 2, "{:?}", p.entries);
        p.input.insert_str("EU");
        p.refresh(&vocab);
        let chosen = Entry::Value {
            text: "prod-eu".into(),
            tag: Tag::Context,
        };
        assert_eq!(
            p.entries,
            vec![chosen.clone()],
            "case-insensitive substring"
        );
        // A space after the name, while the user pauses, is not a second argument.
        p.input.insert(' ');
        p.refresh(&vocab);
        assert_eq!(
            p.entries,
            vec![chosen.clone()],
            "a trailing space keeps the list"
        );
        p.input.backspace();
        assert_eq!(
            p.key(Key::Enter),
            Action::Chose(chosen, vec!["EU".to_string()])
        );
        // Nothing completes `EUzz`, so the row is the command as typed and `enter` hands the
        // app the name to refuse.
        p.input.insert_str("zz");
        p.refresh(&vocab);
        assert_eq!(p.entries, vec![Entry::Command(Command::Ctx)]);
        assert_eq!(
            p.key(Key::Enter),
            Action::Chose(Entry::Command(Command::Ctx), vec!["EUzz".to_string()])
        );
    }

    /// A page is reached by a word its name spells, and it ranks below the kinds the same word
    /// spells: `disaster` names nothing but the page, while `recovery` names recovery plans
    /// first and the Disaster Recovery page after them.
    #[test]
    fn a_page_is_offered_by_name_and_ranks_below_the_kinds() {
        let mut p = Palette {
            input: "disaster".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        let pages: Vec<&Entry> = p
            .entries
            .iter()
            .filter(|e| matches!(e, Entry::Page(_)))
            .collect();
        assert_eq!(pages.len(), 1, "{:?}", p.entries);
        assert!(
            matches!(p.current(), Some(Entry::Page(def)) if def.id == "disaster-recovery"),
            "{:?}",
            p.entries
        );

        p.input = "recovery".into();
        p.refresh(&Vocabulary::empty());
        assert!(
            matches!(p.entries.first(), Some(Entry::Kind { .. })),
            "a kind leads: {:?}",
            p.entries
        );
        assert!(
            p.entries
                .iter()
                .any(|e| matches!(e, Entry::Page(def) if def.id == "disaster-recovery")),
            "the page is still offered: {:?}",
            p.entries
        );
    }

    /// A character lands where the cursor stands, not at the end: the whole point of the type.
    #[test]
    fn the_cursor_inserts_where_it_stands() {
        let mut p = Palette {
            input: "subnet".into(),
            ..Palette::default()
        };
        assert_eq!(p.key(Key::Left), Action::Moved);
        assert_eq!(p.key(Key::Left), Action::Moved);
        assert_eq!(p.key(Key::Left), Action::Moved);
        assert_eq!(p.key(Key::Char('X')), Action::Edited);
        assert_eq!(p.input.as_str(), "subXnet");
        // `home` and `end` are the two ends of the same line, and `ctrl-a`/`ctrl-e` are aliases.
        assert_eq!(p.key(Key::Home), Action::Moved);
        assert_eq!(p.input.cursor(), 0);
        assert_eq!(p.key(Key::Left), Action::Ignored, "nothing to the left");
        assert_eq!(p.key(Key::Ctrl('e')), Action::Moved);
        assert!(p.input.at_end());
        assert_eq!(
            p.key(Key::Right),
            Action::Ignored,
            "nothing to the right, and this palette was never ranked, so no ghost either"
        );
        assert_eq!(p.key(Key::Ctrl('a')), Action::Moved);
        assert_eq!(p.input.cursor(), 0);

        // `→` at the end of the input is no longer only a mover: it takes the ghost. The edge
        // rule is therefore pinned on a palette that *has* been ranked and still has nothing to
        // offer - `vmm.ahv.config.Vm` carries the alias `vm`, so the word spells its match.
        let mut p = Palette {
            input: "vm".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert_eq!(p.ghost(), None);
        assert!(p.input.at_end());
        assert_eq!(p.key(Key::Right), Action::Ignored, "no ghost to take");
    }

    /// `ctrl-w` and `ctrl-u`, which is what makes a mistyped host cost a keystroke rather than a
    /// field. Both are edits, so both reset the selection and ask the app to re-rank.
    #[test]
    fn ctrl_w_deletes_the_word_before_the_cursor() {
        let mut p = Palette {
            input: "ctx prod-eu".into(),
            selected: 3,
            ..Palette::default()
        };
        assert_eq!(p.key(Key::Ctrl('w')), Action::Edited);
        assert_eq!(p.input.as_str(), "ctx ");
        assert_eq!(p.selected, 0, "an edit puts the cursor back on the top row");
        assert_eq!(p.key(Key::Ctrl('w')), Action::Edited);
        assert_eq!(p.input.as_str(), "");

        let mut p = Palette {
            input: "skin gruvbox-dark".into(),
            ..Palette::default()
        };
        assert_eq!(p.key(Key::Ctrl('u')), Action::Edited);
        assert_eq!(p.input.as_str(), "");
    }

    /// R1, in three shapes. The synthetic empty word exists so that `:ctx ` opens the context
    /// list, and for nothing else: past the last slot it is clamped back to it and the word
    /// being completed stays the last non-empty one. Without the clamp `:ctx EU ` would collapse
    /// to the bare command row, which is what `a_space_after_ctx_completes_the_context_name`
    /// already asserts must not happen.
    #[test]
    fn a_trailing_space_does_not_advance_the_slot() {
        let names = ["lab", "prod-eu"];
        let vocab = Vocabulary::of(&names);

        let mut p = Palette {
            input: "ctx EU ".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert_eq!(
            p.entries,
            vec![Entry::Value {
                text: "prod-eu".into(),
                tag: Tag::Context,
            }],
            "clamped to slot 1, still completing `EU`"
        );
        assert_eq!(p.slot, Some(Slot::Contexts), "and titled for it");

        // A kind head has no slots at all, so a trailing space clamps to position 0 and the head
        // is ranked on its own word rather than the list being emptied.
        let mut p = Palette {
            input: "vm ".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert!(
            matches!(p.entries.first(), Some(Entry::Kind { kind, .. })
                     if kind.id == "vmm.ahv.config.Vm"),
            "{:?}",
            p.entries
        );
        assert_eq!(p.slot, None, "the head is not a slot");

        // A real second argument is a real second argument: nothing completes it, so the row is
        // the command as typed and `enter` hands the app both words (§5.2's last row).
        let mut p = Palette {
            input: "ctx a b".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert_eq!(p.entries, vec![Entry::Command(Command::Ctx)]);
        assert_eq!(p.args(), vec!["a", "b"], "both reach the app to be refused");
    }

    /// `refresh` is a function of the cursor as much as of the text: the word being completed is
    /// the word the cursor stands in, so a cursor moved back onto the head ranks the head again.
    /// The app re-ranks on `Action::Moved` for exactly this reason - without it the popup would
    /// keep offering contexts for a cursor that is no longer in the context word, and `enter`
    /// would run a row the grammar no longer selects.
    #[test]
    fn the_cursor_decides_which_vocabulary_is_ranked() {
        let names = ["lab", "prod-eu"];
        let vocab = Vocabulary::of(&names);
        let mut p = Palette {
            input: "ctx lab".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert_eq!(
            p.entries,
            vec![Entry::Value {
                text: "lab".into(),
                tag: Tag::Context,
            }],
            "at the end of the line, the name is what is being completed"
        );
        assert_eq!(p.slot, Some(Slot::Contexts));

        p.input.home();
        p.refresh(&vocab);
        assert_eq!(
            p.entries.first(),
            Some(&Entry::Command(Command::Ctx)),
            "position 0 ranks commands: {:?}",
            p.entries
        );
        assert!(
            !p.entries.iter().any(|e| matches!(e, Entry::Value { .. })),
            "and no context value at all: {:?}",
            p.entries
        );
        assert_eq!(p.slot, None, "the head is not a slot");
    }

    /// A head that is no command has no row of its own to collapse to, so a word after it does
    /// not empty the list: the head keeps the ranking its own word earns, and `enter` opens what
    /// it opened before the word was typed.
    #[test]
    fn a_word_after_a_head_that_is_no_command_still_ranks_the_head() {
        let mut p = Palette {
            input: "disaster recovery".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert!(
            matches!(p.current(), Some(Entry::Page(def)) if def.id == "disaster-recovery"),
            "{:?}",
            p.entries
        );
        assert_eq!(p.slot, None, "nothing was being completed");
        assert_eq!(
            p.args(),
            vec!["recovery"],
            "and the word still reaches the app"
        );
    }

    /// The four aliases the popup could not reach before, each pinned to the canonical row it
    /// now offers - the spelling `enter` applies, the config records and the `:skin` list marks.
    /// `contains_ci` alone finds the other seven, so these four are the ones that prove `skins`
    /// consults `SKIN_ALIASES` at all.
    #[test]
    fn a_hyphen_free_alias_offers_its_canonical_skin() {
        for (alias, canonical) in [
            ("tokyonight", "tokyo-night"),
            ("onedark", "one-dark"),
            ("rosepine", "rose-pine"),
            ("rosepinedawn", "rose-pine-dawn"),
        ] {
            let mut p = Palette {
                input: format!("skin {alias}").into(),
                ..Palette::default()
            };
            p.refresh(&Vocabulary::empty());
            assert_eq!(p.slot, Some(Slot::Skins), "{alias}");
            assert_eq!(
                p.current(),
                Some(&Entry::Value {
                    text: canonical.into(),
                    tag: Tag::Skin,
                }),
                "{alias}: {:?}",
                p.entries
            );
        }
    }

    /// `:hide` offers the menu's own names, matched on the id or on the label beside it, and
    /// never the Dashboard - a row `enter` refuses is worse than no row. `:show` offers what is
    /// hidden and nothing else, because a group hidden whole leaves its name nowhere on screen.
    #[test]
    fn hide_and_show_complete_from_different_vocabularies() {
        let hidden = ["Data Protection"];
        let vocab = Vocabulary {
            hidden: hidden.to_vec(),
            ..Vocabulary::of(&[])
        };
        let entries = |line: &str| {
            let mut p = Palette {
                input: line.into(),
                ..Palette::default()
            };
            p.refresh(&vocab);
            (p.slot, p.entries)
        };

        let (slot, rows) = entries("show Data");
        assert_eq!(slot, Some(Slot::Show));
        assert_eq!(
            rows,
            vec![Entry::Value {
                text: "Data Protection".into(),
                tag: Tag::Nav { hide: false },
            }]
        );
        let (slot, rows) = entries("show Hardware");
        assert_eq!(slot, None, "nothing has hidden Hardware: {rows:?}");

        // The label is matched but the id is what is offered, as `skins` does for an alias:
        // `Groups` is in `Volume Groups` and in no id at all.
        let (slot, rows) = entries("hide Groups");
        assert_eq!(slot, Some(Slot::Hide));
        assert!(
            rows.contains(&Entry::Value {
                text: "volumes.config.VolumeGroup".into(),
                tag: Tag::Nav { hide: true },
            }),
            "{rows:?}"
        );
        // Each name once, however many groups hold the kind.
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|e| match e {
                Entry::Value { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(ids.len(), unique.len(), "{ids:?}");

        let (_, rows) = entries("hide dash");
        assert_eq!(rows, vec![Entry::Command(Command::Hide)], "{rows:?}");
    }

    /// The grammar is data, so a command that grows an argument changes no signature. Pinned so
    /// that a `Command` variant added by another plan cannot quietly get an empty grammar.
    #[test]
    fn every_command_declares_its_grammar() {
        assert_eq!(Command::Ctx.slots(), &[Slot::Contexts]);
        assert_eq!(Command::Skin.slots(), &[Slot::Skins]);
        assert_eq!(Command::Quit.slots(), &[]);
        assert_eq!(Command::Help.slots(), &[]);
        assert_eq!(Command::Journal.slots(), &[]);
        assert_eq!(Command::Activity.slots(), &[]);
        assert_eq!(Command::CanI.slots(), &[Slot::Actions, Slot::Kinds]);
        assert_eq!(Command::Hide.slots(), &[Slot::Hide]);
        assert_eq!(Command::Show.slots(), &[Slot::Show]);
        // Every command's label is reachable as a head, whatever its case.
        for c in COMMANDS {
            assert_eq!(head_command(&c.label().to_ascii_uppercase()), Some(*c));
        }
    }

    /// The word the cursor is in, and its position. A cursor immediately after a run belongs to
    /// that run; a cursor in a gap starts an empty word at the position after the runs behind it.
    #[test]
    fn the_word_under_the_cursor_is_the_run_it_touches() {
        let at = |text: &str, cursor: usize| {
            let mut p = Palette {
                input: text.into(),
                ..Palette::default()
            };
            p.input.home();
            for _ in 0..cursor {
                p.input.right();
            }
            let (position, word) = p.word_at_cursor();
            (position, word.to_string())
        };
        assert_eq!(at("ctx lab", 7), (1, "lab".into()));
        assert_eq!(
            at("ctx lab", 4),
            (1, "lab".into()),
            "at its first character"
        );
        assert_eq!(
            at("ctx lab", 3),
            (0, "ctx".into()),
            "immediately after a run"
        );
        assert_eq!(
            at("ctx ", 4),
            (1, String::new()),
            "a gap starts an empty word"
        );
        assert_eq!(
            at("ctx  lab", 5),
            (1, "lab".into()),
            "double spaces collapse"
        );
        assert_eq!(at("ctx a b", 7), (2, "b".into()));
        assert_eq!(at("", 0), (0, String::new()));
        assert_eq!(at("vm", 0), (0, "vm".into()), "the head, from its start");
    }

    /// The 220 nav labels are alternative display names for targets the catalog already has, so
    /// they are matched with the three tests `lookup` applies to `display` and borrow the rank
    /// each would have given. What that fixes: ten targets `lookup` finds at no rank at all for
    /// `policies`, because their ids contain neither the word nor a display prefix of it.
    #[test]
    fn nav_labels_borrow_lookups_ranks() {
        fn kinds(p: &Palette) -> Vec<&'static str> {
            p.entries
                .iter()
                .filter_map(|e| match e {
                    Entry::Kind { kind, .. } => Some(kind.id),
                    _ => None,
                })
                .collect()
        }
        assert!(
            !lookup("policies")
                .iter()
                .any(|k| k.id == "networking.config.RoutingPolicy"),
            "lookup misses Routing Policies at every rank, which is the whole point"
        );

        let mut p = Palette {
            input: "policies".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        let found = kinds(&p);
        for rescued in [
            "networking.config.RoutingPolicy",
            "security.management.ApprovalPolicy",
            "iam.authz.AuthorizationPolicy",
            "files.config.ReplicationPolicy",
            "monitoring.serviceability.UserDefinedPolicy",
        ] {
            assert!(found.contains(&rescued), "{rescued} missing from {found:?}");
        }
        // An exact nav label is as strong as an exact id, so the kind actually displayed
        // `Policies` still leads.
        assert!(
            matches!(p.current(), Some(Entry::Kind { kind, .. })
                     if kind.id == "microseg.config.policies"),
            "{:?}",
            p.current()
        );
        // And the rank-4 id noise - every `datapolicies.*` kind, because the namespace segment
        // contains the word - stays under all of it.
        let routing = found
            .iter()
            .position(|id| *id == "networking.config.RoutingPolicy")
            .expect("rescued above");
        let noise = found
            .iter()
            .position(|id| *id == "datapolicies.config.ConsistencyRule");
        assert!(
            noise.is_none_or(|n| routing < n),
            "a nav word prefix beats an id substring: {found:?}"
        );

        // Deviation 1: inside a rank, the catalog's own match leads, then a nav label the word
        // prefixes, then a word inside a nav label. That is what keeps `:subn` on `Subnets`
        // rather than on `Aws Subnets`, whose id would otherwise sort first.
        let mut p = Palette {
            input: "subn".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert_eq!(
            kinds(&p),
            [
                "networking.config.Subnet",
                "networking.config.Layer2Stretch",
                "networking.aws.config.AwsSubnet",
                "networking.config.RemoteSubnet",
            ],
            "catalog display prefix, nav label prefix, nav word prefix, id substring"
        );
    }

    /// R3, in the case a dedup on kinds alone would have missed. `NavItem { label: "Contexts",
    /// target: NavTarget::Contexts }` resolves to `Command::Ctx`, which the command vocabulary
    /// already offered for `c` - with an identical rank, sink and class - so without dedup on
    /// the resolved target the popup would show `:ctx` twice.
    #[test]
    fn the_contexts_nav_item_does_not_double_the_ctx_command() {
        let mut p = Palette {
            input: "c".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        let ctx = p
            .entries
            .iter()
            .filter(|e| **e == Entry::Command(Command::Ctx))
            .count();
        assert_eq!(ctx, 1, "{:?}", p.entries);

        // A nav item with nothing behind it is offered, greyed, rather than vanishing - and it
        // sinks to the bottom of its rank, under every kind that can actually be opened.
        let missing = p
            .entries
            .iter()
            .position(|e| matches!(e, Entry::Missing(i) if i.label == "Catalog Items"))
            .unwrap_or_else(|| panic!("{:?}", p.entries));
        let last_kind = p
            .entries
            .iter()
            .rposition(|e| matches!(e, Entry::Kind { .. }))
            .expect("thirty kinds match `c`");
        assert!(missing > last_kind, "{:?}", p.entries);

        // `:catalog` names one thing in the whole catalog, and it is a hole in the v4 API.
        let mut p = Palette {
            input: "catalog".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert!(
            matches!(p.current(), Some(Entry::Missing(i))
                     if i.label == "Catalog Items" && i.note == Some("not in the v4 API")),
            "{:?}",
            p.entries
        );
    }

    /// The merged sort needs the number `lookup` ranked a kind with, and `lookup` returns its
    /// list in rank order without the ranks. This is the drift guard on the re-derivation, in
    /// two halves: every kind `lookup` returns is one `lookup_rank` can rank, which fails the
    /// moment `lookup` grows a test the palette does not know about; and along `lookup`'s own
    /// output the derived rank is non-decreasing, which fails the moment it reorders its five.
    #[test]
    fn the_derived_rank_agrees_with_lookups_order() {
        for word in [
            "vm", "v", "subn", "policies", "c", "recovery", "q", "cluster", "task", "image",
        ] {
            let mut last = 0;
            for kind in lookup(word) {
                let rank = lookup_rank(kind, word).unwrap_or_else(|| {
                    panic!(
                        "{word}: {} matched a rank the palette cannot derive",
                        kind.id
                    )
                });
                assert!(
                    rank >= last,
                    "{word}: {} ranked {rank} after {last}",
                    kind.id
                );
                last = rank;
            }
        }
    }

    /// `COMMANDS`' declaration order is the tiebreak inside a command's rank, not the
    /// alphabetical one `key` would give: `:` then `enter` - the shortest path through the
    /// palette - opens the Contexts screen rather than running `:can-i` with no arguments.
    #[test]
    fn the_commands_keep_their_declared_order() {
        let mut p = Palette::default();
        p.refresh(&Vocabulary::empty());
        assert_eq!(
            p.current(),
            Some(&Entry::Command(Command::Ctx)),
            "{:?}",
            p.entries
        );
        assert_eq!(
            p.entries,
            COMMANDS
                .iter()
                .copied()
                .map(Entry::Command)
                .collect::<Vec<_>>(),
            "an empty word is every command, in the order they are declared"
        );

        // One letter in, the same tiebreak: `c` prefixes `ctx` and `can-i`, and `ctx` is
        // declared first.
        let mut p = Palette {
            input: "c".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert_eq!(
            p.current(),
            Some(&Entry::Command(Command::Ctx)),
            "{:?}",
            p.entries
        );
    }

    /// §11 Case A. `Subnets` carries the aliases `subnet, subnets, net, network, networks`; the
    /// shortest space-free candidate starting with `subn` is `subnet`, so the ghost is `et` -
    /// not the id, which does not start with the word, and not the display, which is also
    /// `Subnets` but seven characters rather than six.
    #[test]
    fn the_ghost_is_the_selected_rows_shortest_prefixing_candidate() {
        let mut p = Palette {
            input: "subn".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert!(
            matches!(p.current(), Some(Entry::Kind { kind, .. })
                     if kind.id == "networking.config.Subnet"),
            "{:?}",
            p.current()
        );
        assert_eq!(p.ghost(), Some("et"), "the alias `subnet`, not the id");
        assert_eq!(p.key(Key::Tab), Action::Completed);
        assert_eq!(p.input.as_str(), "subnet");
        assert!(p.input.at_end());

        p.refresh(&Vocabulary::empty());
        assert!(
            matches!(p.current(), Some(Entry::Kind { kind, .. })
                     if kind.id == "networking.config.Subnet"),
            "the curated alias is an exact match: {:?}",
            p.current()
        );
        // R2 again: the word now spells the candidate in full.
        assert_eq!(p.ghost(), None);
        // And a ghost never exists mid-string, so `→` never has to walk through one.
        assert_eq!(p.key(Key::Left), Action::Moved);
        assert_eq!(p.ghost(), None, "hidden by the cursor");
        assert_eq!(p.key(Key::End), Action::Moved);
        assert_eq!(p.ghost(), None, "and there was nothing to bring back");
    }

    /// §11 Case C, and deviation 3. `:policies` is where the nav fold shows and the ghost
    /// declines: the rows that would add something all carry a space, and a space cannot be
    /// ghosted - accepting `Routing Policies` would produce a two-word input whose second word
    /// is parsed as an argument to an unknown head.
    #[test]
    fn a_match_that_is_not_a_prefix_has_no_ghost() {
        let mut p = Palette {
            input: "policies".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert!(
            p.entries
                .iter()
                .any(|e| matches!(e, Entry::Kind { kind, .. }
                    if kind.id == "networking.config.RoutingPolicy")),
            "the nav label finds what the id misses: {:?}",
            p.entries
        );
        assert_eq!(
            p.ghost(),
            None,
            "`Routing Policies` has a space; `Policies` adds nothing"
        );
        // With no ghost, tab keeps its old job.
        assert_eq!(p.key(Key::Tab), Action::Moved);
        assert_eq!(p.selected, 1);
    }

    /// R2, spelled out on the input the suggestion-popup test types. `vmm.ahv.config.Vm` carries
    /// the alias `vm`, so for `:vm` the shortest prefixing candidate is the word itself. With
    /// `Some("")` here, `tab` would answer `Completed` having changed nothing and
    /// `navigation__palette_vm.snap`'s prompt line would stop being `:vm█`.
    #[test]
    fn a_word_that_spells_its_match_has_no_ghost() {
        let mut p = Palette {
            input: "vm".into(),
            ..Palette::default()
        };
        p.refresh(&Vocabulary::empty());
        assert!(
            matches!(p.current(), Some(Entry::Kind { kind, .. })
                     if kind.id == "vmm.ahv.config.Vm"),
            "{:?}",
            p.current()
        );
        assert_eq!(p.ghost(), None);
        assert_eq!(p.key(Key::Tab), Action::Moved, "tab keeps its old job");
        assert_eq!(p.input.as_str(), "vm", "and changed nothing");
    }

    /// A command with at least one slot brings a space with its ghost, so `:ct` + `tab` is
    /// `:ctx ` and the context list opens in the same keystroke. A command with no slots gets
    /// none, and `→` at the end of the input accepts a ghost exactly as `tab` does.
    #[test]
    fn a_command_ghost_brings_its_space() {
        let names = ["lab", "prod-eu"];
        let vocab = Vocabulary::of(&names);
        let mut p = Palette {
            input: "ct".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert_eq!(p.current(), Some(&Entry::Command(Command::Ctx)));
        assert_eq!(p.ghost(), Some("x"));
        assert_eq!(p.key(Key::Tab), Action::Completed);
        assert_eq!(p.input.as_str(), "ctx ");
        p.refresh(&vocab);
        assert_eq!(
            p.entries.len(),
            2,
            "the context list, in the same keystroke: {:?}",
            p.entries
        );

        let mut p = Palette {
            input: "qui".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert_eq!(p.current(), Some(&Entry::Command(Command::Quit)));
        assert_eq!(p.ghost(), Some("t"));
        assert_eq!(p.key(Key::Right), Action::Completed, "→ takes it too");
        assert_eq!(p.input.as_str(), "quit", "no slots, no space");
    }

    /// §5.2's collapse branch shows the **head**, so there is nothing to complete the argument
    /// word with. `:subnet s` ranks `Subnets` from the word `subnet` and `:ctx c` collapses to
    /// the `ctx` row because no context contains a `c`; asking either row to finish the `s` or
    /// the `c` would draw `:subnet s█ubnet` and `:ctx c█tx`, the rest of a row that is not about
    /// the word it sits against - and `tab` would write it into a line `App::choose` refuses.
    #[test]
    fn a_word_the_list_is_not_about_has_no_ghost() {
        let names = ["lab", "prod-eu"];
        let vocab = Vocabulary::of(&names);

        // A head that is no command: the ranking is the head's own, at position 1.
        let mut p = Palette {
            input: "subnet s".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert!(
            matches!(p.current(), Some(Entry::Kind { kind, .. })
                     if kind.id == "networking.config.Subnet"),
            "the head, ranked: {:?}",
            p.current()
        );
        assert_eq!(p.slot, None, "no vocabulary produced these rows");
        assert_eq!(p.ghost(), None, "`Subnets` is not about the word `s`");
        assert_eq!(p.key(Key::Tab), Action::Moved, "tab keeps its old job");
        assert_eq!(p.input.as_str(), "subnet s", "and changed nothing");

        // A head that is a command whose slot completed nothing: the row is the command itself.
        let mut p = Palette {
            input: "ctx c".into(),
            ..Palette::default()
        };
        p.refresh(&vocab);
        assert_eq!(p.current(), Some(&Entry::Command(Command::Ctx)));
        assert_eq!(p.slot, None, "no context contains a `c`");
        assert_eq!(p.ghost(), None, "`ctx` is not about the word `c`");
        assert_eq!(p.key(Key::Tab), Action::Moved);
        assert_eq!(p.input.as_str(), "ctx c", "no second `ctx`, and no space");
    }

    /// A trailing space is an ordinary keystroke, and it puts the end of the word one byte
    /// before the cursor: [`completing`]'s clamp hands back the last *non-empty* run, so
    /// `:skin gruv ` still ranks `gruvbox-dark`. Drawing its remainder at the cursor would put
    /// `box-dark` a space away from the `gruv` it completes, and `tab` would insert it there.
    #[test]
    fn a_trailing_space_has_no_ghost() {
        let vocab = Vocabulary::empty();
        for (input, without) in [("subn ", "subn"), ("skin gruv ", "skin gruv")] {
            let mut p = Palette {
                input: input.into(),
                ..Palette::default()
            };
            p.refresh(&vocab);
            assert!(p.input.at_end(), "{input:?}");
            assert_eq!(p.ghost(), None, "{input:?}");
            assert_eq!(p.key(Key::Tab), Action::Moved, "{input:?}");
            assert_eq!(p.input.as_str(), input, "{input:?}");

            // The same line without the space is exactly where the ghost belongs.
            let mut p = Palette {
                input: without.into(),
                ..Palette::default()
            };
            p.refresh(&vocab);
            assert!(p.ghost().is_some(), "{without:?}: {:?}", p.current());
        }
    }

    /// `ctrl-p` stashes what was being typed and walks back; `ctrl-n` walks forward and, past
    /// the newest, gives the stash back.
    ///
    /// `↑`/`↓` are untouched and keep moving the popup. That is the whole reason history is on
    /// `ctrl-p`/`ctrl-n`: at every input state, empty included, the popup has rows, so the
    /// arrows are never free to mean something else.
    #[test]
    fn history_restores_the_stashed_line() {
        let vocab = Vocabulary::empty();
        let mut p = Palette::with_history(vec!["vm".into(), "ctx lab".into(), "skin nord".into()]);
        p.input.insert_str("half");
        assert_eq!(p.key(Key::Ctrl('p')), Action::Edited);
        assert_eq!(p.input.as_str(), "skin nord", "newest first");
        assert!(p.input.at_end(), "and the cursor goes to the end");
        p.key(Key::Ctrl('p'));
        assert_eq!(p.input.as_str(), "ctx lab");
        p.key(Key::Ctrl('p'));
        assert_eq!(p.input.as_str(), "vm");
        assert_eq!(
            p.key(Key::Ctrl('p')),
            Action::Ignored,
            "the oldest is the end"
        );

        p.key(Key::Ctrl('n'));
        assert_eq!(p.input.as_str(), "ctx lab");
        p.key(Key::Ctrl('n'));
        assert_eq!(p.input.as_str(), "skin nord");
        assert_eq!(p.key(Key::Ctrl('n')), Action::Edited);
        assert_eq!(
            p.input.as_str(),
            "half",
            "past the newest is what was being typed"
        );
        assert_eq!(
            p.key(Key::Ctrl('n')),
            Action::Ignored,
            "and there is no more"
        );

        // Any edit drops the recall: the recalled line becomes the line being typed.
        p.key(Key::Ctrl('p'));
        assert_eq!(p.input.as_str(), "skin nord");
        p.key(Key::Char('!'));
        assert_eq!(p.input.as_str(), "skin nord!");
        assert_eq!(
            p.key(Key::Ctrl('n')),
            Action::Ignored,
            "nowhere to go forward to"
        );

        // And the arrows still move the popup rather than the history.
        p.refresh(&vocab);
        let line = p.input.as_str().to_string();
        p.key(Key::Down);
        assert_eq!(p.input.as_str(), line);

        // An empty history has nothing to recall and says so.
        let mut p = Palette::default();
        assert_eq!(p.key(Key::Ctrl('p')), Action::Ignored);
        assert_eq!(p.key(Key::Ctrl('n')), Action::Ignored);
    }
}

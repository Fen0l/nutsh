//! Where a click landed, and what it does there.

use super::*;

impl App {
    /// A pointer event. Sets `dirty` only when something moved: the wheel over a list already at
    /// its end changes nothing, and redrawing for it would burn a frame.
    pub fn mouse(&mut self, m: Mouse) {
        // A pointer is somebody being there, exactly as a key is: a session driven by the mouse
        // alone must not drift into the idle pause while it is being used.
        self.woke();
        let landed = self.landed(m);
        if self.act(landed) {
            self.dirty = true;
        }
    }

    /// Whether the event loop should capture the mouse.
    pub fn mouse_enabled(&self) -> bool {
        self.mouse_on
    }

    /// Where `m` fell, read off the hit map and nothing else.
    pub(super) fn landed(&self, m: Mouse) -> Landed {
        use crate::mouse::{MouseKind, contains};
        let hits = self.hits.borrow();
        // A modal owns the pointer while it is up. Everything outside its list is inert: a
        // stray click must not throw away a half-typed `:` command, and it must not reach the
        // table underneath either. `Mode::Detail`, `Mode::Help`, `Mode::Form` and
        // `Mode::Contexts` record no modal list at all and so answer `Nothing` for every event.
        if self.mode != Mode::Table {
            return match (m.kind, hits.modal.as_ref()) {
                (MouseKind::Click, Some(list)) => {
                    list.index_at(m).map_or(Landed::Nothing, Landed::Modal)
                }
                (MouseKind::WheelDown, Some(list)) if contains(list.rows, m) => {
                    Landed::ModalWheel(true)
                }
                (MouseKind::WheelUp, Some(list)) if contains(list.rows, m) => {
                    Landed::ModalWheel(false)
                }
                _ => Landed::Nothing,
            };
        }
        if m.kind == MouseKind::Click && hits.version.is_some_and(|r| contains(r, m)) {
            return Landed::Version;
        }
        if let Some(list) = hits.sidebar.as_ref()
            && contains(list.rows, m)
        {
            return match m.kind {
                MouseKind::Click => list.index_at(m).map_or(Landed::Nothing, Landed::Menu),
                MouseKind::WheelDown => Landed::MenuWheel(true),
                MouseKind::WheelUp => Landed::MenuWheel(false),
                _ => Landed::Nothing,
            };
        }
        for t in hits.tables.iter() {
            if !contains(t.block, m) {
                continue;
            }
            let target = t.pane.map_or(Target::Table, Target::Pane);
            return match m.kind {
                MouseKind::Click if contains(t.header, m) => match t.column_at(m) {
                    Some(column) => Landed::Header { target, column },
                    None => Landed::Block { target },
                },
                MouseKind::Click => match t.index_at(m) {
                    Some(row) => Landed::Row { target, row },
                    // A pane's border, its title, its empty area, or the space below its last
                    // row: focus it and select nothing.
                    None => Landed::Block { target },
                },
                MouseKind::WheelDown => Landed::Wheel { target, down: true },
                MouseKind::WheelUp => Landed::Wheel {
                    target,
                    down: false,
                },
                MouseKind::WheelLeft if target == Target::Table => Landed::Columns { right: false },
                MouseKind::WheelRight if target == Target::Table => Landed::Columns { right: true },
                MouseKind::WheelLeft | MouseKind::WheelRight => Landed::Nothing,
            };
        }
        // The header box, the stats block, the prompt line, the status line.
        Landed::Nothing
    }

    /// Act on it, and say whether anything moved.
    pub(super) fn act(&mut self, landed: Landed) -> bool {
        match landed {
            Landed::Nothing => false,
            Landed::Version => {
                self.show_settings();
                true
            }
            Landed::Modal(i) => self.select_in_modal(i),
            Landed::ModalWheel(down) => {
                let selected = self.modal_selected();
                let next = step(selected, down, WHEEL_ROWS, self.modal_len());
                self.select_in_modal(next)
            }
            Landed::Menu(i) => {
                self.focus_sidebar();
                // A click on the caption between the curated menu and the namespaces selects
                // nothing and opens nothing; the focus still moves, so the click is not lost.
                if self.sidebar.select(i, false) {
                    // What `enter` does on that row: open an item, toggle a group.
                    let action = self.sidebar.key(Key::Enter);
                    self.sidebar_action(action);
                }
                true
            }
            Landed::MenuWheel(down) => {
                let next = step(
                    self.sidebar.selected,
                    down,
                    WHEEL_ROWS,
                    self.sidebar.rows().len(),
                );
                let moved = next != self.sidebar.selected;
                self.sidebar.select(next, true);
                moved
            }
            Landed::Row { target, row } => {
                let focused = self.focus_table(target);
                // Not short-circuited: the selection moves whether or not the focus did.
                self.set_selection(target, row) || focused
            }
            Landed::Header { target, column } => {
                self.focus_table(target);
                // A pane sorts nothing: it has no `S` either.
                if target != Target::Table {
                    return true;
                }
                self.sort_by(column);
                true
            }
            Landed::Block { target } => self.focus_table(target),
            Landed::Wheel { target, down } => {
                // Deliberately no `focus_table`: scrolling a pane you are only glancing at
                // should not steal the keyboard.
                let selected = self.selection(target);
                let next = step(selected, down, WHEEL_ROWS, self.rows_len(target));
                self.set_selection(target, next)
            }
            Landed::Columns { right } => {
                self.handle_table(if right { Key::Right } else { Key::Left });
                true
            }
        }
    }

    /// Focus the body, and on a page the pane that was clicked. Says whether either moved.
    pub(super) fn focus_table(&mut self, target: Target) -> bool {
        let mut moved = self.focus != Focus::Body;
        self.focus = Focus::Body;
        if let Target::Pane(i) = target
            && let Some(live) = self.live.as_mut()
            && let Some(page) = live.page_mut()
            && page.focus != i
            && i < page.panes.len()
        {
            page.focus = i;
            moved = true;
        }
        moved
    }

    /// The cursor of the table view, or of the named pane on a page.
    pub(super) fn selection(&self, target: Target) -> usize {
        match target {
            Target::Pane(i) => self
                .page()
                .and_then(|page| page.panes.get(i))
                .map_or(0, |pane| pane.selected),
            Target::Table => self
                .live
                .as_ref()
                .and_then(Live::table)
                .map_or(0, |view| view.selected),
        }
    }

    /// How many rows it has, which is what a wheel is clamped to.
    pub(super) fn rows_len(&self, target: Target) -> usize {
        let Some(live) = self.live.as_ref() else {
            return 0;
        };
        let key = match target {
            Target::Pane(i) => match live.page().and_then(|page| page.panes.get(i)) {
                Some(pane) => &pane.key,
                None => return 0,
            },
            Target::Table => match live.table() {
                Some(view) => &view.key,
                None => return 0,
            },
        };
        live.store.table(key).rows.len()
    }

    /// Move it, and say whether it moved.
    pub(super) fn set_selection(&mut self, target: Target, row: usize) -> bool {
        let Some(live) = self.live.as_mut() else {
            return false;
        };
        match target {
            Target::Pane(i) => match live.page_mut().and_then(|page| page.panes.get_mut(i)) {
                Some(pane) if pane.selected != row => {
                    pane.selected = row;
                    true
                }
                _ => false,
            },
            Target::Table => match live.table_mut() {
                Some(view) if view.selected != row => {
                    view.selected = row;
                    true
                }
                _ => false,
            },
        }
    }

    /// The open modal's cursor, its length, and where to move it. All three dispatch on the
    /// mode rather than on which `Option` is `Some`, because the palette can be open behind the
    /// skin list and only one of them is taking keys.
    pub(super) fn modal_selected(&self) -> usize {
        match self.mode {
            Mode::Command => self.palette.as_ref().map_or(0, |p| p.selected),
            Mode::Picker => self.picker.as_ref().map_or(0, |p| p.selected),
            Mode::Skins => self.skins.as_ref().map_or(0, |s| s.selected),
            Mode::Ladder => self.ladder.as_ref().map_or(0, |l| l.selected),
            _ => 0,
        }
    }

    pub(super) fn modal_len(&self) -> usize {
        match self.mode {
            Mode::Command => self.palette.as_ref().map_or(0, |p| p.entries.len()),
            Mode::Picker => self.picker.as_ref().map_or(0, |p| p.entries.len()),
            Mode::Skins => crate::theme::BUILTIN_NAMES.len(),
            Mode::Ladder => nutsh_core::refresh::LADDER.len(),
            _ => 0,
        }
    }

    pub(super) fn select_in_modal(&mut self, i: usize) -> bool {
        if i >= self.modal_len() || i == self.modal_selected() {
            return false;
        }
        match self.mode {
            Mode::Command => {
                if let Some(p) = self.palette.as_mut() {
                    p.selected = i;
                }
            }
            // Through `Picker::select`, which refuses a caption: a click on the rule between
            // the related kinds and the actions selects nothing.
            Mode::Picker => {
                if !self.picker.as_mut().is_some_and(|p| p.select(i)) {
                    return false;
                }
            }
            Mode::Skins => {
                if let Some(s) = self.skins.as_mut() {
                    s.selected = i;
                }
            }
            Mode::Ladder => {
                if let Some(l) = self.ladder.as_mut() {
                    l.selected = i;
                }
            }
            _ => return false,
        }
        true
    }

    /// Sort by the column at `all_index` in the view's full column list: ascending if it was not
    /// the sort column, otherwise flipped. `S` keeps cycling through the columns as it does; a
    /// click names one.
    pub(crate) fn sort_by(&mut self, all_index: usize) {
        if let Some(live) = self.live.as_mut()
            && let Some(view) = live.table_mut()
        {
            view.sort = match view.sort {
                Some((col, reverse)) if col == all_index => Some((all_index, !reverse)),
                _ => Some((all_index, false)),
            };
        }
    }
}

//! A page in front of the user: its panes, and moving between them.

use super::*;

impl App {
    /// The keys of a page: `tab` between panes, `O` for the focused pane's kind as a full
    /// table, and the cursor keys inside the focused pane.
    pub(super) fn handle_page(&mut self, key: Key) {
        match key {
            Key::Tab => self.cycle_pane(true),
            Key::BackTab => self.cycle_pane(false),
            Key::Char('O') => self.open_focused_pane(),
            Key::Enter | Key::Char('y') => self.open_detail(),
            // A pane row is a table row: the whole action pipeline reads `App::cursor_table`,
            // which answers for a focused pane as it answers for a table view, so `a` here runs
            // on the same row `⏎` would open. The Dashboard is the launch view and every group
            // opens a page, so this is where most sessions meet an action at all.
            Key::Char('a') => self.open_menu(),
            Key::Esc => self.back(),
            // A page marks nothing and acts on nothing, but `q` means the same everywhere a
            // body is: a quit that depends on which view is open is one nobody trusts.
            Key::Char('q') => self.request_quit(),
            Key::Char(':') => self.open_palette(),
            Key::Char('?') => {
                self.help_from = self.mode;
                self.mode = Mode::Help;
            }
            _ => self.move_in_pane(key),
        }
    }

    /// `tab`/`shift-tab` between the panes of the open page.
    pub(super) fn cycle_pane(&mut self, forward: bool) {
        if let Some(live) = self.live.as_mut()
            && let Some(page) = live.page_mut()
        {
            page.cycle(forward);
        }
    }

    /// The keys of a pane that only move its cursor or ask for a poll now.
    pub(super) fn move_in_pane(&mut self, key: Key) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let Live { store, stack, .. } = live;
        let Some(View::Page(page)) = stack.last_mut() else {
            return;
        };
        let Some(pane) = page.focused_mut() else {
            return;
        };
        let last = store.table(&pane.key).rows.len().saturating_sub(1);
        let mut flash: Option<&'static str> = None;
        let mut retime = false;
        match key {
            Key::Char('j') | Key::Down => pane.selected = (pane.selected + 1).min(last),
            Key::Char('k') | Key::Up => pane.selected = pane.selected.saturating_sub(1),
            Key::Char('g') | Key::Home => pane.selected = 0,
            Key::Char('G') | Key::End => pane.selected = last,
            Key::PageDown => pane.selected = (pane.selected + 20).min(last),
            Key::PageUp => pane.selected = pane.selected.saturating_sub(20),
            Key::Ctrl('r') => {
                pane.stopped = false;
                if let Some(sub) = pane.sub {
                    live.scheduler.refresh(sub);
                }
            }
            Key::Ctrl('x') => {
                let stopped = pane.sub.is_some_and(|sub| live.scheduler.stop(sub));
                pane.stopped = stopped;
                flash = Some(if stopped {
                    "stopped"
                } else {
                    "nothing to stop"
                });
            }
            // Handled after the borrows end, because `step_refresh` needs `self` whole.
            Key::Ctrl('t') => retime = true,
            _ => {}
        }
        // After the borrows of `live`'s fields end, because both of these need `self` again.
        if let Some(text) = flash {
            self.status = Some(text.to_string());
        }
        if retime {
            self.step_refresh();
        }
    }

    /// `O`: the focused pane's kind as a full table, pushed over the page. The table is the
    /// unfiltered listing - the pane's filter is what made it a pane, not what the kind is.
    ///
    /// Refused for the same two reasons `open_root` refuses: a pane already saying why this
    /// Prism Central cannot serve it would otherwise open a table that polls and fails on a
    /// loop with nothing marking it, and a child kind has no listing of its own to open -
    /// `TableKey::under(kind, vec![])` would leave its parent placeholders unfilled.
    pub(super) fn open_focused_pane(&mut self) {
        let Some(pane) = self.page().and_then(|p| p.focused()) else {
            return;
        };
        let (kind, title) = (pane.key.kind, pane.def.title);
        let refused = pane.reason.clone().or_else(|| reach(kind).reason());
        if let Some(reason) = refused {
            self.status = Some(format!("{title}: {reason}"));
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        push_view(live, kind, Vec::new(), None, None);
    }
}

//! The left menu: every major Nutanix feature, and a way to it that needs no kind id.
//!
//! It owns its selection, its collapse set and its filter; the app applies the effects a key
//! asks for. Its scroll window is not state: `draw` frames the selection against the pane it
//! actually has, the way the palette and the picker do, so the highlight can never name a row
//! the frame is not showing. It is drawn beside the **body only**, between the header and the
//! prompt line - the header's hint grid needs 96 columns and a full-height menu would take
//! them, the header describes the session and the session is above the navigation rather than
//! beside it, and a full-width top bar over a left menu is what Prism Central itself does.

use std::collections::{HashMap, HashSet};

use nutsh_catalog::{CURATED_GROUPS, NAV, NavItem, NavTarget};

use crate::key::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Group {
        group: usize,
    },
    Item {
        group: usize,
        item: usize,
    },
    /// The caption between the curated menu and the namespaces under it. Not a place to stand:
    /// `j`/`k` step over it, `enter` on it does nothing and a click on it selects nothing - it
    /// says what the rows beneath it are, which is the leftovers of the catalog rather than more
    /// menu. Drawn only when a namespace group is actually going to follow it, so a filter that
    /// matches nothing down there leaves no heading over an empty stretch.
    Separator,
}

/// The Dashboard's two names: the group the menu draws, and the page id behind its only item.
/// `nav_id` resolves a page id exactly and a group name loosely, so a hide can arrive spelled
/// either way and both have to be refused - otherwise `:hide dashboard` empties the group that
/// `:hide Dashboard` is not allowed to remove, and leaves a header with nothing under it.
pub(crate) const DASHBOARD_GROUP: &str = "Dashboard";
pub(crate) const DASHBOARD_PAGE: &str = "dashboard";

/// Never hidden, whatever the user types: the Dashboard is the way back to a screen that works,
/// and it is what a bare launch opens onto.
pub(crate) fn protected(id: &str) -> bool {
    id == DASHBOARD_GROUP || id == DASHBOARD_PAGE
}

/// What a key did, for the app to act on: the sidebar cannot reach the session or the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Moved,
    Filtered,
    Toggled,
    /// `enter` on an item. The item itself, so its `note` travels with it: two groups may
    /// carry the same label and a lookup by label would find the wrong one.
    Open(&'static NavItem),
    /// `tab` or `esc`: the body takes the keys back.
    Blur,
    /// A key the menu does not claim - `:`, `?`, `^r` - which the body still binds and the
    /// prompt line still advertises while the menu has focus.
    Passthrough(Key),
    Ignored,
}

#[derive(Debug)]
pub struct Sidebar {
    /// The user's answer to "should there be a menu": `ctrl-b` flips it.
    pub visible: bool,
    /// Whether `ctrl-b` was ever pressed, which is what makes a narrow frame show the menu as
    /// an overlay rather than not at all.
    pub toggled: bool,
    pub selected: usize,
    /// Whether `/` is taking characters.
    pub filtering: bool,
    /// What the body is showing, so the menu can mark it.
    pub open: Option<NavTarget>,
    collapsed: HashSet<usize>,
    filter: String,
    /// The flattening of `NAV` under `collapsed` and `filter`. Cached rather than rebuilt on
    /// demand: one keystroke asks for it three or four times and every frame asks again, and
    /// each rebuild allocated a `Vec` and a lowercased `String` per item.
    rows: Vec<Row>,
    /// Ids and group names the app has computed as hidden: kind ids, page ids and group names,
    /// all `&'static str` out of the catalog.
    ///
    /// Rebuilt by [`Sidebar::set_hidden`] and nothing else, and only when the facts behind it
    /// change - at connect, on `:hide`/`:show`, on `:all`, and on a context switch. Nothing about
    /// it is asynchronous per poll, which is what makes an index-based `selected` safe here.
    hidden: HashSet<&'static str>,
    /// Why a kind cannot be opened on this Prism Central, by kind id: `unavailable_reason`'s
    /// sentence, which is why it is a `String` and not the `&'static str` a `NavItem::note` is.
    ///
    /// Greying changes no row, so this one never reflattens anything - it is read by the drawer
    /// and by nothing else. It is set from the same walk that computes the hidden set: one pass,
    /// two outputs.
    unserved: HashMap<&'static str, String>,
    /// Per group, how many of its items the last flattening dropped: what the header says.
    dropped: Vec<usize>,
}

impl Default for Sidebar {
    /// Every group starts collapsed: twenty-nine headers already fill the pane, and an
    /// expanded Compute & Storage would push the rest of the tree off the bottom before the
    /// user has asked for anything.
    fn default() -> Sidebar {
        let mut sidebar = Sidebar {
            visible: true,
            toggled: false,
            selected: 0,
            filtering: false,
            open: None,
            collapsed: (0..NAV.len()).collect(),
            filter: String::new(),
            rows: Vec::new(),
            hidden: HashSet::new(),
            unserved: HashMap::new(),
            dropped: vec![0; NAV.len()],
        };
        sidebar.refresh();
        sidebar
    }
}

impl Sidebar {
    /// The flattened rows: every group header, and the items of the groups that are expanded.
    /// A filter expands every group and keeps only the items whose label matches.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn is_collapsed(&self, group: usize) -> bool {
        self.collapsed.contains(&group)
    }

    /// Expand or collapse a group, and reflatten. The cursor is left where it is: the callers
    /// that have to move it - `fold`, `open_selected` - do so themselves.
    pub fn set_collapsed(&mut self, group: usize, collapsed: bool) {
        if collapsed {
            self.collapsed.insert(group);
        } else {
            self.collapsed.remove(&group);
        }
        self.refresh();
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter.clear();
        self.filter.push_str(filter);
        self.refresh();
    }

    /// Replace the hidden set and reflatten, re-anchoring the cursor by what it was on.
    pub fn set_hidden(&mut self, hidden: HashSet<&'static str>) {
        self.hidden = hidden;
        self.reanchor();
    }

    /// Replace the greyed set. No reflattening: nothing moves, the drawer just dims a row and
    /// says why beside it where the pane has room.
    pub fn set_unserved(&mut self, unserved: HashMap<&'static str, String>) {
        self.unserved = unserved;
    }

    /// Why this item cannot be opened, or `None` when it can. The item's own static `note` wins:
    /// a `NavTarget::Missing` is not in the v4 API at all, which no negotiation can change.
    pub fn reason(&self, group: usize, item: usize) -> Option<&str> {
        let it = &NAV[group].items[item];
        if let Some(note) = it.note {
            return Some(note);
        }
        match it.target {
            NavTarget::Kind(id) | NavTarget::Page(id) => self.unserved.get(id).map(String::as_str),
            NavTarget::Contexts | NavTarget::Settings | NavTarget::Missing => None,
        }
    }

    /// How many items of `group` the last flattening dropped; `0` when none, which is what the
    /// header draws nothing for.
    pub fn dropped(&self, group: usize) -> usize {
        self.dropped.get(group).copied().unwrap_or(0)
    }

    /// Whether this item is hidden and on screen only because the filter matched it: the drawer
    /// draws it dim, so a search never lies about the catalog.
    pub fn is_hidden(&self, group: usize, item: usize) -> bool {
        self.hides(group, &NAV[group].items[item])
    }

    /// Never hidden, whatever the rules say: the item currently open - hiding the view under the
    /// cursor is a trap, so it stays and is hidden again once the user leaves it - the
    /// `Contexts` item, which is the way back to a Prism Central that serves more, and the
    /// `Settings` item, which is the screen that turns hiding off again.
    fn hides(&self, group: usize, item: &NavItem) -> bool {
        if self.open == Some(item.target)
            || matches!(item.target, NavTarget::Contexts | NavTarget::Settings)
        {
            return false;
        }
        if self.hidden.contains(NAV[group].name) {
            return true;
        }
        match item.target {
            NavTarget::Kind(id) | NavTarget::Page(id) => self.hidden.contains(id),
            NavTarget::Contexts | NavTarget::Settings | NavTarget::Missing => false,
        }
    }

    /// Remember what the cursor was on, reflatten, and put it back. If the row is gone - it was
    /// just hidden, or `:hide` named the group it was in - the cursor moves to that group's
    /// header, and to row 0 if the group itself is gone. Never left dangling past the end.
    fn reanchor(&mut self) {
        let was = self.rows.get(self.selected).copied();
        self.refresh();
        let Some(was) = was else { return };
        if let Some(i) = self.rows.iter().position(|r| *r == was) {
            self.selected = i;
            return;
        }
        let group = match was {
            Row::Group { group } | Row::Item { group, .. } => group,
            // The caption is in the list whenever a namespace group is, so a cursor that was on
            // it either found it again above or has nothing to be anchored to.
            Row::Separator => return,
        };
        self.selected = self
            .rows
            .iter()
            .position(|r| *r == Row::Group { group })
            .unwrap_or(0);
    }

    /// Reflatten `NAV` under the current collapse set, filter and hidden set, then pull the
    /// cursor back onto the list it now describes.
    fn refresh(&mut self) {
        let needle = self.filter.to_ascii_lowercase();
        self.rows.clear();
        self.dropped = vec![0; NAV.len()];
        let mut captioned = false;
        for (g, group) in NAV.iter().enumerate() {
            let matches = |item: &NavItem| {
                needle.is_empty() || item.label.to_ascii_lowercase().contains(&needle)
            };
            // A group named in the hide set goes whole - header and all - unless it holds the
            // item that is open, which `hides` never hides.
            let all_hidden = group.items.iter().all(|i| self.hides(g, i));
            if self.hidden.contains(group.name) && all_hidden && needle.is_empty() {
                continue;
            }
            if !needle.is_empty() && !group.items.iter().any(matches) {
                continue;
            }
            // After every reason a group has not to be drawn, so the caption is written only
            // when something is actually going to follow it.
            if g >= CURATED_GROUPS && !captioned {
                self.rows.push(Row::Separator);
                captioned = true;
            }
            self.rows.push(Row::Group { group: g });
            if needle.is_empty() && self.collapsed.contains(&g) {
                // Counted even while collapsed: the header is the only thing on screen and it is
                // where the count belongs.
                self.dropped[g] = group.items.iter().filter(|i| self.hides(g, i)).count();
                continue;
            }
            for (item, def) in group.items.iter().enumerate() {
                if !matches(def) {
                    continue;
                }
                // A filter always shows a hidden item - drawn dim, so a search never lies about
                // the catalog; without one the item is dropped and counted.
                if self.hides(g, def) && needle.is_empty() {
                    self.dropped[g] += 1;
                    continue;
                }
                self.rows.push(Row::Item { group: g, item });
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.selected = self.past_separator(self.selected, false);
    }

    pub fn group_at(&self, index: usize) -> Option<usize> {
        match self.rows.get(index)? {
            Row::Group { group } | Row::Item { group, .. } => Some(*group),
            Row::Separator => None,
        }
    }

    /// Put the cursor on row `index`, for the mouse - which addresses a row by where it is on
    /// the frame rather than by stepping. `false` when there is no such row, or when it is the
    /// caption and `carry_on` is not set: a **click** on the caption selects nothing, while a
    /// **wheel notch** that lands on it (`carry_on`) goes one further the way it was going.
    pub fn select(&mut self, index: usize, carry_on: bool) -> bool {
        if index >= self.rows.len() {
            return false;
        }
        if matches!(self.rows[index], Row::Separator) && !carry_on {
            return false;
        }
        self.selected = self.past_separator(index, index >= self.selected);
        true
    }

    /// `index` unless it is the caption, in which case the next row the way the cursor was
    /// going, or - at the end of the list - the other way. There is only ever one caption, so
    /// one step is always enough.
    fn past_separator(&self, index: usize, down: bool) -> usize {
        if !matches!(self.rows.get(index), Some(Row::Separator)) {
            return index;
        }
        let last = self.rows.len().saturating_sub(1);
        let forward = (index + 1).min(last);
        let back = index.saturating_sub(1);
        let (first, second) = if down {
            (forward, back)
        } else {
            (back, forward)
        };
        [first, second]
            .into_iter()
            .find(|i| !matches!(self.rows.get(*i), Some(Row::Separator)))
            .unwrap_or(index)
    }

    /// Expand the group that holds `target`, put the cursor on it, and remember it as open -
    /// what a palette jump does to the menu.
    pub fn reveal(&mut self, target: &NavTarget) {
        self.open = Some(*target);
        let Some((g, i)) = NAV.iter().enumerate().find_map(|(g, group)| {
            group
                .items
                .iter()
                .position(|item| item.target == *target)
                .map(|i| (g, i))
        }) else {
            // A kind that is in no group: nothing to highlight, but `open` still changed, and
            // the item it was on a moment ago may be one the hidden set was only sparing
            // because it was open.
            self.refresh();
            return;
        };
        self.collapsed.remove(&g);
        self.filter.clear();
        self.filtering = false;
        self.refresh();
        self.selected = self
            .rows
            .iter()
            .position(|r| *r == Row::Item { group: g, item: i })
            .unwrap_or(self.selected);
    }

    /// `1`..`9` jump to a curated group, `0` to the first generated one.
    ///
    /// There are ten digits and ten curated groups, so the tenth - `Settings`, last in
    /// `nav.toml` - has no digit of its own: `0` keeps meaning the first generated group, which
    /// is the boundary a reader is actually looking for. `:settings` opens that screen from
    /// anywhere, and `j` from `Contexts` reaches the row.
    pub fn jump(&mut self, digit: u32) {
        let group = if digit == 0 {
            CURATED_GROUPS
        } else {
            (digit as usize - 1).min(CURATED_GROUPS - 1)
        };
        self.selected = self
            .rows
            .iter()
            .position(|r| *r == Row::Group { group })
            .unwrap_or(0);
    }

    pub fn key(&mut self, key: Key) -> Action {
        if self.filtering {
            return match key {
                Key::Esc => {
                    self.set_filter("");
                    self.filtering = false;
                    Action::Filtered
                }
                Key::Enter => {
                    self.filtering = false;
                    self.open_selected()
                }
                Key::Char(c) => {
                    let filter = format!("{}{c}", self.filter);
                    self.set_filter(&filter);
                    self.selected = 0;
                    Action::Filtered
                }
                Key::Backspace => {
                    let mut filter = self.filter.clone();
                    filter.pop();
                    self.set_filter(&filter);
                    self.selected = 0;
                    Action::Filtered
                }
                _ => self.move_key(key),
            };
        }
        match key {
            Key::Char('/') => {
                self.filtering = true;
                Action::Filtered
            }
            Key::Tab | Key::Esc => Action::Blur,
            Key::Enter => self.open_selected(),
            Key::Char('h') | Key::Left => self.fold(true),
            Key::Char('l') | Key::Right => self.fold(false),
            _ => self.move_key(key),
        }
    }

    /// The movement keys, and - for anything neither they nor `key` claims - a passthrough, so
    /// `:` and `?` still work with the keys in the menu.
    fn move_key(&mut self, key: Key) -> Action {
        let last = self.rows.len().saturating_sub(1);
        // The direction comes from the key, not from a comparison of indices: `g` and `G` name
        // an end, and the only way off the caption at an end is inwards.
        let (next, down) = match key {
            Key::Char('j') | Key::Down => ((self.selected + 1).min(last), true),
            Key::Char('k') | Key::Up => (self.selected.saturating_sub(1), false),
            Key::Char('g') | Key::Home => (0, true),
            Key::Char('G') | Key::End => (last, false),
            _ => return Action::Passthrough(key),
        };
        self.selected = self.past_separator(next, down);
        Action::Moved
    }

    /// `h`/`l` on a group header collapse or expand it; on an item, its group - so the gesture
    /// means the same thing wherever the cursor is.
    fn fold(&mut self, shut: bool) -> Action {
        let Some(group) = self.group_at(self.selected) else {
            return Action::Ignored;
        };
        self.set_collapsed(group, shut);
        if shut {
            // The cursor cannot sit on a row that is no longer there.
            self.selected = self
                .rows
                .iter()
                .position(|r| *r == Row::Group { group })
                .unwrap_or(0);
        }
        Action::Toggled
    }

    fn open_selected(&mut self) -> Action {
        match self.rows.get(self.selected) {
            Some(Row::Group { group }) => {
                let group = *group;
                self.set_collapsed(group, !self.collapsed.contains(&group));
                Action::Toggled
            }
            Some(Row::Item { group, item }) => Action::Open(&NAV[*group].items[*item]),
            Some(Row::Separator) | None => Action::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The label of a flattened row: what the tests read a selection back as.
    fn label(s: &Sidebar, index: usize) -> Option<&'static str> {
        match s.rows().get(index)? {
            Row::Group { group } => Some(NAV[*group].name),
            Row::Item { group, item } => Some(NAV[*group].items[*item].label),
            Row::Separator => Some("-"),
        }
    }

    /// Everything starts collapsed: the headers already fill the pane, and an expanded group
    /// would push the rest of the tree off the bottom before the user asked for anything.
    #[test]
    fn flattening_hides_a_collapsed_groups_items() {
        let mut s = Sidebar::default();
        assert_eq!(
            s.rows().len(),
            NAV.len() + 1,
            "one row per group, no items, and the caption"
        );
        s.set_collapsed(1, false);
        assert_eq!(s.rows().len(), NAV.len() + 1 + NAV[1].items.len());
        assert!(
            s.rows()
                .iter()
                .any(|r| matches!(r, Row::Item { group: 1, .. }))
        );
        s.set_collapsed(1, true);
        assert_eq!(s.rows().len(), NAV.len() + 1);
    }

    /// The caption divides the curated menu from the namespaces under it, appears once, and is
    /// not a place to stand: `j` steps over it, `enter` on it does nothing, a click refuses and
    /// only a wheel carries the cursor past it.
    #[test]
    fn the_caption_divides_the_menu_and_is_never_stood_on() {
        let mut s = Sidebar::default();
        let at = s
            .rows()
            .iter()
            .position(|r| matches!(r, Row::Separator))
            .expect("the caption");
        assert_eq!(
            at, CURATED_GROUPS,
            "after the curated groups and before the first namespace"
        );
        assert_eq!(s.rows().iter().filter(|r| **r == Row::Separator).count(), 1);
        assert_eq!(s.group_at(at), None);

        s.selected = at - 1;
        s.key(Key::Char('j'));
        assert_eq!(s.selected, at + 1, "`j` stepped over it");
        s.key(Key::Char('k'));
        assert_eq!(s.selected, at - 1, "and `k` stepped back over it");

        s.selected = at;
        assert_eq!(s.open_selected(), Action::Ignored, "`enter` does nothing");
        assert!(!s.select(at, false), "a click on it selects nothing");
        s.selected = at - 1;
        assert!(s.select(at, true), "a wheel notch carries on");
        assert_eq!(s.selected, at + 1);

        // A filter that matches nothing below leaves no heading over an empty stretch.
        s.set_filter("Catalog Items");
        assert!(
            !s.rows().iter().any(|r| matches!(r, Row::Separator)),
            "{:?}",
            s.rows()
        );
    }

    /// A palette jump expands the group that holds what was opened and puts the cursor on it.
    #[test]
    fn reveal_expands_the_group_and_selects_the_item() {
        let mut s = Sidebar::default();
        s.reveal(&NavTarget::Kind("networking.config.Subnet"));
        assert!(matches!(s.rows()[s.selected], Row::Item { .. }));
        assert_eq!(label(&s, s.selected), Some("Subnets"));
        assert_eq!(s.open, Some(NavTarget::Kind("networking.config.Subnet")));
        // A kind that is in no group leaves the cursor alone and highlights nothing.
        let before = s.selected;
        s.reveal(&NavTarget::Kind("vmm.ahv.config.Disk"));
        assert_eq!(s.selected, before);
    }

    /// `/` narrows to the items whose label matches, and expands every group that keeps one.
    #[test]
    fn the_filter_keeps_only_matching_items() {
        let mut s = Sidebar::default();
        s.set_filter("recovery");
        let labels: Vec<&str> = (0..s.rows().len()).filter_map(|i| label(&s, i)).collect();
        assert!(labels.contains(&"Recovery Plans"), "{labels:?}");
        assert!(!labels.contains(&"Subnets"), "{labels:?}");
        assert!(
            s.rows().iter().any(|r| matches!(r, Row::Group { .. })),
            "the groups that keep an item are still headed"
        );
        // A filter that matches nothing empties the list; the window and the counts below are
        // written to survive that rather than to index into it.
        let mut none = Sidebar::default();
        none.set_filter("zzzz");
        assert!(none.rows().is_empty());
        assert_eq!(none.selected, 0);
        assert_eq!(none.key(Key::Enter), Action::Ignored, "no row to open");
    }

    #[test]
    fn a_digit_jumps_to_a_curated_group_and_zero_to_the_generated_ones() {
        let mut s = Sidebar::default();
        s.jump(1);
        assert_eq!(s.group_at(s.selected), Some(0));
        s.jump(9);
        assert_eq!(s.group_at(s.selected), Some(8), "Contexts is the ninth");
        s.jump(0);
        assert_eq!(
            s.group_at(s.selected),
            Some(CURATED_GROUPS),
            "the first generated group"
        );
    }

    /// The scroll window is `palette::window` over the pane's real height, so the row it
    /// frames is the row `enter` opens whatever the frame is.
    #[test]
    fn the_window_frames_the_selection_at_the_real_height() {
        let mut s = Sidebar::default();
        // What the app does on the way there: opening VMs expands Compute & Storage, and its
        // twelve items push Hardware's own past the twelve rows a 23-row frame's pane has.
        s.reveal(&NavTarget::Kind("vmm.ahv.config.Vm"));
        s.reveal(&NavTarget::Kind("clustermgmt.config.Cluster"));
        let selected = s.selected;
        assert!(selected > 12, "Clusters is past a 12-row pane: {selected}");
        for rows in [1, 5, 12, 19, 200] {
            let (offset, window) = crate::palette::window(s.rows(), selected, rows);
            assert!(offset <= selected, "{rows}: {offset} > {selected}");
            assert!(
                selected - offset < window.len(),
                "{rows}: {selected} is outside a {}-row window at {offset}",
                window.len()
            );
        }
    }

    /// Keys the menu does not bind reach the body: the prompt line under it still says
    /// `:palette` and `?:help`.
    #[test]
    fn an_unclaimed_key_passes_through() {
        let mut s = Sidebar::default();
        assert_eq!(s.key(Key::Char(':')), Action::Passthrough(Key::Char(':')));
        assert_eq!(s.key(Key::Char('?')), Action::Passthrough(Key::Char('?')));
        assert_eq!(s.key(Key::Ctrl('r')), Action::Passthrough(Key::Ctrl('r')));
        assert_eq!(s.key(Key::Char('j')), Action::Moved, "and `j` still moves");
    }
}

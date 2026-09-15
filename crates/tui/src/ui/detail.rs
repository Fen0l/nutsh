//! The composed detail pane.

use super::*;

pub(super) fn draw_detail(app: &App, f: &mut Frame, body: Rect) {
    let Some(d) = &app.detail else {
        return;
    };
    // `None` on a payload pane, which is over no row at all, and on an entity pane whose row
    // has not landed yet; the pane words the two differently.
    let entity = app.detail_entity();
    // A payload pane reads nothing from the store, so it must not need a session to be drawn:
    // without one the cache is simply empty, and every reference renders as its own extId.
    let empty = Names::default();
    let names = app.live.as_ref().map_or(&empty, |l| l.store.names());
    // Recorded for `App::detail_last_line`, which measures the same lines and cannot reach the
    // layout: the pane's interior is its width less the two border columns.
    let width = body.width.saturating_sub(2);
    app.detail_width.set(width);
    // The one list every action surface draws, measured by `App::detail_last_line` off the same
    // call: a pane whose last line the scroll does not know about is one that cannot be read to
    // the end.
    let actions = app.detail_actions();
    let lines = d.lines(crate::detail::BodyView {
        entity,
        names,
        now: app.now,
        width,
        actions: &actions,
    });
    // Scrolling past the end would leave an empty box with no way to tell why.
    let scroll = d.scroll.min(Detail::last_line(&lines));
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(d.title(entity)))
            .scroll((scroll, 0)),
        body,
    );
}

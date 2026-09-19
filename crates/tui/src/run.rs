//! The only module that touches the terminal.

use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crossterm::ExecutableCommand;
use crossterm::cursor::Show;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::time::MissedTickBehavior;

use crate::app::App;
use crate::key::Key;
use crate::mouse::{Mouse, MouseKind};

fn key_of(event: KeyEvent) -> Option<Key> {
    Some(match (event.code, event.modifiers) {
        (KeyCode::Char(c), m) if m.contains(KeyModifiers::CONTROL) => {
            Key::Ctrl(c.to_ascii_lowercase())
        }
        (KeyCode::Char(c), _) => Key::Char(c),
        (KeyCode::Enter, _) => Key::Enter,
        (KeyCode::Esc, _) => Key::Esc,
        (KeyCode::Backspace, _) => Key::Backspace,
        (KeyCode::Tab, _) => Key::Tab,
        (KeyCode::BackTab, _) => Key::BackTab,
        (KeyCode::Up, _) => Key::Up,
        (KeyCode::Down, _) => Key::Down,
        (KeyCode::PageUp, _) => Key::PageUp,
        (KeyCode::PageDown, _) => Key::PageDown,
        (KeyCode::Home, _) => Key::Home,
        (KeyCode::End, _) => Key::End,
        (KeyCode::Left, _) => Key::Left,
        (KeyCode::Right, _) => Key::Right,
        _ => return None,
    })
}

/// The pointer gestures the app understands. Everything else is dropped here rather than in
/// `App`: any-event tracking reports a `Moved` per cell the mouse crosses, and a drag reports
/// one per cell too, so the filter belongs where the events arrive.
fn mouse_of(event: MouseEvent) -> Option<Mouse> {
    let kind = match event.kind {
        MouseEventKind::Down(MouseButton::Left) => MouseKind::Click,
        MouseEventKind::ScrollUp => MouseKind::WheelUp,
        MouseEventKind::ScrollDown => MouseKind::WheelDown,
        MouseEventKind::ScrollLeft => MouseKind::WheelLeft,
        MouseEventKind::ScrollRight => MouseKind::WheelRight,
        _ => return None,
    };
    Some(Mouse {
        kind,
        col: event.column,
        row: event.row,
    })
}

/// Undo everything [`run`] did to the terminal, in any state it may have got to. Every step
/// is attempted and none is trusted: this runs from the panic hook as well as from the exit
/// path, and showing the cursor is done here rather than left to `Terminal`'s `Drop`, which a
/// panic between `enable_raw_mode` and `Terminal::new` never reaches.
fn restore() {
    let _ = disable_raw_mode();
    // Before `LeaveAlternateScreen` and unconditionally. `restore` runs from the panic hook and
    // from the signal handler as well as from the clean exit, and sending the disable sequence
    // to a terminal that was never captured is a no-op. A terminal left in capture mode is the
    // one failure mode this feature must never have.
    let _ = stdout().execute(DisableMouseCapture);
    let _ = stdout().execute(LeaveAlternateScreen);
    let _ = stdout().execute(Show);
}

/// What a panic anywhere in the process left behind, for the loop to find. A panic in a
/// spawned task - a poller, the connect - unwinds that task alone: without this the hook would
/// tidy the terminal away and the loop would go on drawing into a screen that is no longer
/// there.
#[derive(Default)]
struct Panics(Mutex<Option<String>>);

impl Panics {
    fn set(&self, message: String) {
        if let Ok(mut slot) = self.0.lock() {
            slot.get_or_insert(message);
        }
    }

    /// The first panic's message, once.
    fn take(&self) -> Option<String> {
        self.0.lock().ok()?.take()
    }
}

/// What a panic said and where, on one line: `PanicHookInfo`'s own `Display` spans two.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let what = payload_message(info.payload());
    match info.location() {
        Some(at) => format!("{what} at {at}"),
        None => what.to_string(),
    }
}

/// The `&str` or `String` a panic carried; anything else has nothing to print.
fn payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panicked")
}

/// Hand the terminal back before a signal that would otherwise kill the process mid-frame.
/// Raw mode clears `ISIG`, so `ctrl-c` reaches the app as a key rather than as `SIGINT`; a
/// signal that does arrive comes from outside - `kill`, a closed terminal, a shutting-down
/// session - and there is no way back into the loop from one.
#[cfg(unix)]
fn restore_on_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    for kind in [
        SignalKind::terminate(),
        SignalKind::hangup(),
        SignalKind::interrupt(),
    ] {
        // A signal that cannot be listened for is not worth refusing to start over.
        let Ok(mut signals) = signal(kind) else {
            continue;
        };
        tokio::spawn(async move {
            if signals.recv().await.is_some() {
                restore();
                // The shell reads this as "killed by that signal", which is what happened,
                // and it costs no dependency on `libc` to re-raise it with.
                std::process::exit(128 + kind.as_raw_value());
            }
        });
    }
}

#[cfg(not(unix))]
fn restore_on_signal() {}

/// Runs `app` on the real terminal until it quits. The terminal is restored on every exit
/// path: a clean quit, an error, a signal, and a panic on any thread.
pub async fn run(mut app: App) -> anyhow::Result<()> {
    let panics = Arc::new(Panics::default());
    // Only the thread the loop runs on owns the terminal. A panic on a worker thread unwinds
    // that task and leaves the loop drawing, so restoring from there would blank the screen
    // under a program that is still running; the loop restores for itself when it sees the
    // flag. `run` is polled by `block_on`, so this is the loop's thread.
    let loop_thread = std::thread::current().id();
    let hook = std::panic::take_hook();
    let seen = panics.clone();
    std::panic::set_hook(Box::new(move |info| {
        seen.set(panic_message(info));
        if std::thread::current().id() == loop_thread {
            restore();
        }
        hook(info);
    }));
    restore_on_signal();
    enable_raw_mode()?;
    // Nothing between here and `restore` may use `?`: raw mode is on, so a terminal setup
    // that fails - `EnterAlternateScreen`, `Terminal::new` - has to leave through the same
    // door as a clean quit, or it hands the user back a shell that no longer echoes.
    let result = event_loop(&mut app, &panics).await;
    // A clean exit is worth a write: it is the difference between the next run painting this
    // session and painting the one before it. Awaited, because the process is about to end and
    // a `spawn_blocking` nobody waits for is a write that may not land.
    if let Some(handle) = app.write_cache_now() {
        let _ = handle.await;
    }
    restore();
    // Back to the default hook: ours writes escape sequences at a terminal this function has
    // just finished with.
    let _ = std::panic::take_hook();
    // One that landed after the loop's last look, so it is reported rather than swallowed.
    match panics.take() {
        Some(panic) => Err(anyhow::anyhow!("a background task panicked: {panic}")),
        None => result,
    }
}

async fn event_loop(app: &mut App, panics: &Panics) -> anyhow::Result<()> {
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // A tick is "one second has passed", not a quota to catch up on: after the process is
    // suspended, ticking once is right and ticking away the whole gap in a burst is not.
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The app owns the wish and the loop owns the terminal, reconciled once per iteration: that
    // is how `:mouse` and `ctrl-o` take effect on the next frame without `App` touching stdout,
    // and it is also the initial enable. A terminal that does not answer the sequence sends no
    // `Event::Mouse` and nothing else changes.
    let mut captured = false;
    loop {
        if app.mouse_enabled() != captured {
            captured = app.mouse_enabled();
            // Bound rather than written `stdout().execute(…)` twice: `execute` borrows the
            // handle and hands it back, so the `if` would outlive the temporary it borrowed.
            let mut out = stdout();
            let _ = if captured {
                out.execute(EnableMouseCapture)
            } else {
                out.execute(DisableMouseCapture)
            };
        }
        // A panic in a task the app spawned: the frame it was going to feed is never coming,
        // so the loop stops rather than drawing the same screen for ever. The tick bounds how
        // long a quiet terminal takes to notice.
        if let Some(panic) = panics.take() {
            break Err(anyhow::anyhow!("a background task panicked: {panic}"));
        }
        if app.dirty {
            if let Err(e) = terminal.draw(|f| crate::ui::draw(app, f)) {
                break Err(e.into());
            }
            app.dirty = false;
        }
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => {
                    if let Some(key) = key_of(k) {
                        app.handle(key);
                    }
                }
                // Guarded on the wish, not only on the capture: `ctrl-o` is read in the
                // iteration before the one that sends `DisableMouseCapture`, so an event
                // already in the stream would otherwise act after the user turned it off.
                // With the mouse off the app is the app it is over SSH - identical.
                Some(Ok(Event::Mouse(m))) if app.mouse_enabled() => {
                    if let Some(mouse) = mouse_of(m) {
                        app.mouse(mouse);
                    }
                }
                // A resize is a person moving a window, so it wakes the pause as a key does -
                // and the frame is redrawn either way, at the size the terminal now is.
                Some(Ok(Event::Resize(_, _))) => {
                    app.woke();
                    app.dirty = true;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => break Err(e.into()),
                None => break Ok(()),
            },
            msg = app.poll_rx.recv() => {
                if let Some(msg) = msg {
                    app.apply(msg);
                    // A paged list arrives as a message per page. Applying what is already
                    // queued costs one frame instead of one frame per page, and the frame it
                    // draws is the newer one.
                    while let Ok(msg) = app.poll_rx.try_recv() {
                        app.apply(msg);
                    }
                }
            }
            result = app.connect_rx.recv() => {
                if let Some(result) = result {
                    app.connect_outcome(result);
                }
            }
            // Both clocks: the wall one dates what is drawn, the monotonic one is what the
            // idle pause is measured on.
            _ = tick.tick() => app.tick(SystemTime::now(), std::time::Instant::now()),
        }
        if app.should_quit() {
            break Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two payloads a `panic!` can leave - a literal and a formatted string - reach the
    /// message the loop reports; anything else says only that something panicked.
    #[test]
    fn a_panic_payload_becomes_a_message() {
        assert_eq!(payload_message(&"boom"), "boom");
        assert_eq!(payload_message(&format!("boom {}", 1)), "boom 1");
        assert_eq!(payload_message(&42u8), "panicked");
    }

    /// `EnableMouseCapture` turns on any-event tracking (DECSET 1003), so a mouse crossing the
    /// terminal delivers an event per cell. An app that redrew on each would burn a frame per
    /// pixel of travel, so `Moved`, `Drag` and `Up` never become a `Mouse` at all - and neither
    /// do the middle and right buttons.
    #[test]
    fn only_a_left_click_and_the_wheel_become_input() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let ev = |kind| MouseEvent {
            kind,
            column: 4,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse_of(ev(MouseEventKind::Down(MouseButton::Left))),
            Some(Mouse {
                kind: MouseKind::Click,
                col: 4,
                row: 7
            })
        );
        for (kind, want) in [
            (MouseEventKind::ScrollUp, MouseKind::WheelUp),
            (MouseEventKind::ScrollDown, MouseKind::WheelDown),
            (MouseEventKind::ScrollLeft, MouseKind::WheelLeft),
            (MouseEventKind::ScrollRight, MouseKind::WheelRight),
        ] {
            assert_eq!(mouse_of(ev(kind)).map(|m| m.kind), Some(want));
        }
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
        ] {
            assert_eq!(mouse_of(ev(kind)), None, "{kind:?}");
        }
    }

    /// The first panic is the one worth reporting: the ones that follow are usually its
    /// consequences, and it is read once, by the loop.
    #[test]
    fn the_first_panic_is_kept_and_taken_once() {
        let panics = Panics::default();
        assert_eq!(panics.take(), None);
        panics.set("first".into());
        panics.set("second".into());
        assert_eq!(panics.take().as_deref(), Some("first"));
        assert_eq!(panics.take(), None);
    }
}

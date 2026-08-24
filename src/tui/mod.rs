mod actions;
mod app;
pub mod data;
mod editor;
mod event;
mod markdown;
mod preview;
mod ui;
mod watch;

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyEventKind, poll, read,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::store::sqlite::Store;
use crate::tui::app::App;
use crate::tui::event::translate;
use crate::tui::markdown::make_skin;
use crate::tui::watch::DbWatcher;

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut out = io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            // A bar cursor sits at the insertion point (left edge of the cell)
            // rather than covering the cell to its right, so edit-mode typing and
            // backspace land where the cursor visually is. The cursor is only
            // shown in edit mode, so a bar everywhere is harmless.
            SetCursorStyle::SteadyBar,
        )
        .context("failed to enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            SetCursorStyle::DefaultUserShape,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

pub fn run(store: &Store, profile_name: &str, base_url: &str) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            io::stdout(),
            SetCursorStyle::DefaultUserShape,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        default_hook(info);
    }));

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to init terminal")?;

    let mut watcher = DbWatcher::new(store)?;
    let mut app = App::new(profile_name.to_string(), base_url, store)?;
    let skin = make_skin();

    app.ensure_middle_loaded(store);
    app.ensure_right_loaded(store);
    if watcher.changed()? {
        app.refresh_from_store(store, base_url);
    }
    let tick = Duration::from_millis(500);
    // Footer status messages auto-clear so they don't linger as stale state.
    // The `tick`-bounded poll guarantees we re-check the deadline at least
    // twice a second even when the user is idle.
    let status_ttl = Duration::from_secs(4);
    let mut last_status = app.status.clone();
    let mut status_at = app.status.as_ref().map(|_| Instant::now());

    // Draw once, then redraw after input or after another SQLite connection
    // commits new data. `poll` keeps the TUI responsive without busy-looping.
    terminal.draw(|frame| ui::draw(frame, &mut app, &skin))?;
    loop {
        let mut should_draw = false;
        if poll(tick)? {
            let ev = read()?;
            // Ignore key-release events so one physical press doesn't fire twice.
            if let Event::Key(k) = &ev
                && k.kind != KeyEventKind::Press
            {
                continue;
            }
            let action = translate(
                app.focus,
                app.pane_rects,
                app.is_editing(),
                app.prompt(),
                ev,
            );
            app.apply(action, store);
            should_draw = true;
        }

        if watcher.changed()? {
            app.refresh_from_store(store, base_url);
            should_draw = true;
        }

        // Restart the auto-clear timer whenever the message changes; expire it
        // once it has been visible for `status_ttl`.
        if app.status != last_status {
            last_status = app.status.clone();
            status_at = app.status.as_ref().map(|_| Instant::now());
        }
        if let Some(shown_at) = status_at
            && shown_at.elapsed() >= status_ttl
        {
            app.status = None;
            last_status = None;
            status_at = None;
            should_draw = true;
        }

        if app.should_quit {
            break;
        }
        if should_draw {
            terminal.draw(|frame| ui::draw(frame, &mut app, &skin))?;
        }
    }
    Ok(())
}

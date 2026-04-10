//! Vim-style key handling. Mode- and focus-aware.
//!
//! Kept deliberately data-free: one `dispatch` fn that switches on the app's
//! current mode. Grow this into a table-driven keymap later if needed — for
//! now, explicit matches read well and are easy to audit.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, Mode};
use crate::ui::modals::fuzzy::FuzzyTarget;

pub fn dispatch(app: &mut App, key: KeyEvent) {
    // Image viewer modal intercepts all input — Esc/q/v close it.
    if app.image_viewer.is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('v') => {
                app.close_image_viewer();
            }
            _ => {}
        }
        return;
    }

    match app.mode {
        Mode::Normal => handle_normal(app, key),
        Mode::Insert => handle_insert(app, key),
        Mode::Search => handle_search(app, key),
        Mode::Help => handle_help(app, key),
    }
}

fn handle_normal(app: &mut App, key: KeyEvent) {
    // Two-key sequences starting with `g`: `gg` → top, `gd` → jump to
    // reply target (only meaningful on the messages pane). Any other
    // second key falls through to the normal single-key handler below.
    if let Some('g') = app.pending_key {
        app.pending_key = None;
        match key.code {
            KeyCode::Char('g') => {
                app.go_top();
                return;
            }
            KeyCode::Char('d') if app.focus == crate::app::Focus::Messages => {
                app.jump_to_reply_target();
                return;
            }
            _ => {}
        }
    }

    // Two-key sequence `dd` — delete the selected message (Vim-style).
    // Only meaningful on the messages pane.
    if let Some('d') = app.pending_key {
        app.pending_key = None;
        if let KeyCode::Char('d') = key.code {
            if app.focus == crate::app::Focus::Messages {
                app.delete_selected();
                return;
            }
        }
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Char('?') => app.toggle_help(),

        // Focus switching.
        KeyCode::Tab => app.cycle_focus(),
        KeyCode::Char('w') if ctrl => app.cycle_focus(),

        // Movement.
        KeyCode::Char('j') | KeyCode::Down => app.move_down(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(1),
        KeyCode::Char('h') | KeyCode::Left => {
            if app.focus == crate::app::Focus::Messages {
                app.focus = crate::app::Focus::ChatList;
            }
        }
        KeyCode::Char('l') | KeyCode::Right => {
            if app.focus == crate::app::Focus::ChatList {
                app.focus = crate::app::Focus::Messages;
            }
        }
        KeyCode::Char('d') if ctrl => app.move_down(10),
        KeyCode::Char('d') if app.focus == crate::app::Focus::Messages => {
            app.pending_key = Some('d');
        }
        KeyCode::Char('u') if ctrl => app.move_up(10),
        KeyCode::Char('f') if ctrl => app.move_down(20),
        KeyCode::Char('b') if ctrl => app.move_up(20),
        KeyCode::Char('G') => app.go_bottom(),
        KeyCode::Char('g') => app.pending_key = Some('g'),

        // Insert mode / input.
        KeyCode::Char('i') | KeyCode::Enter => app.enter_insert(),

        // Reply to the currently-selected message (toggle).
        KeyCode::Char('r') if app.focus == crate::app::Focus::Messages => app.toggle_reply(),
        // Edit the currently-selected outgoing message (toggle).
        KeyCode::Char('e') if app.focus == crate::app::Focus::Messages => app.toggle_edit(),
        // View the selected message's attached photo.
        KeyCode::Char('v') if app.focus == crate::app::Focus::Messages => app.view_selected(),

        // Fuzzy search.
        KeyCode::Char('/') => match app.focus {
            crate::app::Focus::Messages => app.open_fuzzy(FuzzyTarget::Messages),
            _ => app.open_fuzzy(FuzzyTarget::ChatList),
        },
        KeyCode::Char('p') if ctrl => app.open_fuzzy(FuzzyTarget::GlobalSwitcher),

        _ => {}
    }
}

fn handle_insert(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.enter_normal(),
        KeyCode::Enter if !ctrl => {
            // Ctrl+Enter would be newline; plain Enter sends.
            app.send_input();
        }
        KeyCode::Char('w') if ctrl => app.cycle_focus(),
        _ => {
            // Delegate everything else to the textarea so multi-line editing,
            // arrows, backspace, etc. all work natively.
            app.input.input(tui_textarea::Input::from(key));
            // Any real keystroke counts as "still typing" from Telegram's
            // perspective. The App debounces so we only fire every few sec.
            app.maybe_send_typing();
        }
    }
}

fn handle_search(app: &mut App, key: KeyEvent) {
    let Some(fuzzy) = app.fuzzy.as_mut() else {
        app.mode = Mode::Normal;
        return;
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.close_fuzzy(),
        KeyCode::Enter => app.confirm_fuzzy(),
        KeyCode::Down => fuzzy.move_cursor(1),
        KeyCode::Up => fuzzy.move_cursor(-1),
        KeyCode::Char('n') if ctrl => fuzzy.move_cursor(1),
        KeyCode::Char('p') if ctrl => fuzzy.move_cursor(-1),
        KeyCode::Char('j') if ctrl => fuzzy.move_cursor(1),
        KeyCode::Char('k') if ctrl => fuzzy.move_cursor(-1),
        KeyCode::Backspace => fuzzy.pop_char(),
        KeyCode::Char(c) => fuzzy.push_char(c),
        _ => {}
    }
}

fn handle_help(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => app.toggle_help(),
        _ => {}
    }
}

/// Static list of bindings shown on the help screen.
pub fn bindings_help() -> &'static [(&'static str, &'static str)] {
    &[
        ("q", "Quit"),
        ("?", "Toggle help"),
        ("Tab / Ctrl-w", "Cycle focus between panes"),
        ("h j k l", "Move left/down/up/right"),
        ("gg / G", "Jump to top / bottom"),
        ("gd", "Jump to reply target (in messages pane)"),
        ("Ctrl-d / Ctrl-u", "Half page down / up"),
        ("Ctrl-f / Ctrl-b", "Full page down / up"),
        ("i / Enter", "Enter insert mode"),
        ("Esc", "Leave insert mode (back to messages)"),
        ("/", "Fuzzy search (context-aware)"),
        ("Ctrl-p", "Global chat switcher"),
        ("r", "Reply to selected message (toggle)"),
        ("e", "Edit selected outgoing message (toggle)"),
        ("v", "View attached photo (opens image modal)"),
        ("dd", "Delete selected message"),
        ("Enter (insert)", "Send message"),
    ]
}

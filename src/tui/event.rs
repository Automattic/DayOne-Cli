use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::tui::editor::Move as EditMove;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Middle,
    Right,
}

/// Which modal confirmation prompt, if any, is currently capturing input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    None,
    /// Edit-mode exit/quit guard: save / discard / keep editing.
    UnsavedChanges,
    /// Delete-entry confirmation: confirm / cancel.
    DeleteEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delta {
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleDir {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Move(Delta),
    Enter,
    Back,
    Cycle(CycleDir),
    ScrollRight(Delta),
    ClickSidebar(u16),
    ClickMiddle(u16),
    // --- View-mode text selection (read-only markdown pane) ---
    StartRightSelection {
        col: u16,
        line: u16,
    },
    UpdateRightSelection {
        col: u16,
        line: u16,
    },
    FinishRightSelection {
        col: u16,
        line: u16,
    },
    CopyToClipboard,
    Delete,
    ConfirmDelete,
    CancelDelete,
    Quit,
    NoOp,
    /// Create a new entry in the current journal and start editing it.
    NewEntry,
    // --- Edit mode ---
    /// Start editing the currently selected entry (Enter on the content pane).
    EnterEdit,
    EditInsertChar(char),
    EditInsertStr(String),
    EditNewline,
    EditBackspace,
    EditDelete,
    /// Move the edit cursor; bool = extend the selection.
    EditMove(EditMove, bool),
    EditCopySelection,
    EditSave,
    /// Request to leave edit mode (Esc). Prompts when there are unsaved changes.
    EditExit,
    // --- Modal confirm prompt (unsaved-changes guard) ---
    /// Save the buffer, then complete the pending exit/quit.
    ConfirmSave,
    /// Discard the buffer, then complete the pending exit/quit.
    ConfirmDiscard,
    /// Dismiss the prompt and keep editing.
    CancelPrompt,
    /// Place the cursor at a content-relative (col, row).
    EditClick(u16, u16),
    /// Extend the selection to a content-relative (col, row).
    EditDrag(u16, u16),
    EditScroll(Delta),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PaneRects {
    pub sidebar: Rect,
    pub middle: Rect,
    pub right: Rect,
    pub sidebar_list_top: u16,
    pub middle_list_top: u16,
    pub right_content_top: u16,
}

pub fn translate(
    focus: Focus,
    rects: PaneRects,
    editing: bool,
    prompt: Prompt,
    event: Event,
) -> Action {
    // A modal prompt swallows everything except its own choice keys. The two
    // prompts differ: unsaved-changes is three-way (save/discard/keep), delete
    // is a simple confirm/cancel.
    match prompt {
        Prompt::UnsavedChanges => {
            return match event {
                Event::Key(key) => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => Action::ConfirmSave,
                    KeyCode::Char('n') | KeyCode::Char('N') => Action::ConfirmDiscard,
                    KeyCode::Esc => Action::CancelPrompt,
                    _ => Action::NoOp,
                },
                _ => Action::NoOp,
            };
        }
        Prompt::DeleteEntry => {
            return match event {
                Event::Key(key) => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        Action::ConfirmDelete
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::CancelDelete,
                    _ => Action::NoOp,
                },
                _ => Action::NoOp,
            };
        }
        Prompt::None => {}
    }
    if editing {
        return match event {
            Event::Key(key) => translate_edit_key(key),
            Event::Mouse(mouse) => translate_edit_mouse(rects, mouse),
            Event::Paste(text) => Action::EditInsertStr(text),
            _ => Action::NoOp,
        };
    }
    match event {
        Event::Key(key) => translate_key(focus, key),
        Event::Mouse(mouse) => translate_mouse(rects, mouse),
        _ => Action::NoOp,
    }
}

fn translate_key(focus: Focus, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Action::Quit,
            KeyCode::Char('y') => return Action::CopyToClipboard,
            KeyCode::Char('n') => return Action::NewEntry,
            KeyCode::Char('d') => return Action::Delete,
            _ => {}
        }
    }
    // Enter activates the selected entry: edit a journal entry, or open a chat
    // day's messages. On the sidebar it just drills into the entry list.
    // Arrow-right / `l` drill in to the read-only preview without editing.
    if key.code == KeyCode::Enter && matches!(focus, Focus::Middle | Focus::Right) {
        return Action::EnterEdit;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => match focus {
            Focus::Right => Action::ScrollRight(Delta::Up),
            _ => Action::Move(Delta::Up),
        },
        KeyCode::Down | KeyCode::Char('j') => match focus {
            Focus::Right => Action::ScrollRight(Delta::Down),
            _ => Action::Move(Delta::Down),
        },
        KeyCode::Home | KeyCode::Char('g') => Action::Move(Delta::Home),
        KeyCode::End | KeyCode::Char('G') => Action::Move(Delta::End),
        KeyCode::PageUp => Action::ScrollRight(Delta::PageUp),
        KeyCode::PageDown => Action::ScrollRight(Delta::PageDown),
        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => Action::Enter,
        KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => Action::Back,
        KeyCode::Tab => Action::Cycle(CycleDir::Forward),
        KeyCode::BackTab => Action::Cycle(CycleDir::Backward),
        _ => Action::NoOp,
    }
}

fn translate_edit_key(key: KeyEvent) -> Action {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Action::Quit,
            KeyCode::Char('s') => Action::EditSave,
            KeyCode::Char('y') => Action::EditCopySelection,
            KeyCode::Char('a') => Action::EditMove(EditMove::Home, false),
            KeyCode::Char('e') => Action::EditMove(EditMove::End, false),
            KeyCode::Home => Action::EditMove(EditMove::DocStart, shift),
            KeyCode::End => Action::EditMove(EditMove::DocEnd, shift),
            _ => Action::NoOp,
        };
    }
    match key.code {
        KeyCode::Esc => Action::EditExit,
        KeyCode::Enter => Action::EditNewline,
        KeyCode::Backspace => Action::EditBackspace,
        KeyCode::Delete => Action::EditDelete,
        KeyCode::Tab => Action::EditInsertChar('\t'),
        KeyCode::Left => Action::EditMove(EditMove::Left, shift),
        KeyCode::Right => Action::EditMove(EditMove::Right, shift),
        KeyCode::Up => Action::EditMove(EditMove::Up, shift),
        KeyCode::Down => Action::EditMove(EditMove::Down, shift),
        KeyCode::Home => Action::EditMove(EditMove::Home, shift),
        KeyCode::End => Action::EditMove(EditMove::End, shift),
        KeyCode::PageUp => Action::EditScroll(Delta::PageUp),
        KeyCode::PageDown => Action::EditScroll(Delta::PageDown),
        KeyCode::Char(c) => Action::EditInsertChar(c),
        _ => Action::NoOp,
    }
}

fn translate_edit_mouse(rects: PaneRects, mouse: MouseEvent) -> Action {
    let content_x = rects.right.x.saturating_add(1);
    let rel_col = mouse.column.saturating_sub(content_x);
    let rel_row = mouse.row.saturating_sub(rects.right_content_top);
    let in_content = inside(rects.right, mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if in_content => {
            Action::EditClick(rel_col, rel_row)
        }
        MouseEventKind::Drag(MouseButton::Left) => Action::EditDrag(rel_col, rel_row),
        MouseEventKind::ScrollUp => Action::EditScroll(Delta::Up),
        MouseEventKind::ScrollDown => Action::EditScroll(Delta::Down),
        _ => Action::NoOp,
    }
}

fn translate_mouse(rects: PaneRects, mouse: MouseEvent) -> Action {
    let (x, y) = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if inside(rects.sidebar, x, y) {
                let offset = y.saturating_sub(rects.sidebar_list_top);
                Action::ClickSidebar(offset)
            } else if inside(rects.middle, x, y) {
                let offset = y.saturating_sub(rects.middle_list_top);
                Action::ClickMiddle(offset)
            } else if inside(rects.right, x, y) {
                right_selection_action(rects, x, y, SelectionMouseAction::Start)
            } else {
                Action::NoOp
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let (x, y) = clamp_to_inner(rects.right, x, y);
            right_selection_action(rects, x, y, SelectionMouseAction::Update)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let (x, y) = clamp_to_inner(rects.right, x, y);
            right_selection_action(rects, x, y, SelectionMouseAction::Finish)
        }
        MouseEventKind::ScrollUp => Action::ScrollRight(Delta::Up),
        MouseEventKind::ScrollDown => Action::ScrollRight(Delta::Down),
        _ => Action::NoOp,
    }
}

enum SelectionMouseAction {
    Start,
    Update,
    Finish,
}

fn right_selection_action(
    rects: PaneRects,
    x: u16,
    y: u16,
    action: SelectionMouseAction,
) -> Action {
    let inner_left = rects.right.x.saturating_add(1);
    let col = x.saturating_sub(inner_left);
    let line = y.saturating_sub(rects.right_content_top).saturating_add(
        rects
            .right_content_top
            .saturating_sub(rects.right.y.saturating_add(1)),
    );
    match action {
        SelectionMouseAction::Start => Action::StartRightSelection { col, line },
        SelectionMouseAction::Update => Action::UpdateRightSelection { col, line },
        SelectionMouseAction::Finish => Action::FinishRightSelection { col, line },
    }
}

fn clamp_to_inner(rect: Rect, x: u16, y: u16) -> (u16, u16) {
    let min_x = rect.x.saturating_add(1);
    let min_y = rect.y.saturating_add(1);
    let max_x = rect.x.saturating_add(rect.width.saturating_sub(2));
    let max_y = rect.y.saturating_add(rect.height.saturating_sub(2));
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

fn inside(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    fn rects() -> PaneRects {
        PaneRects {
            sidebar: Rect::new(0, 1, 10, 20),
            middle: Rect::new(10, 1, 20, 20),
            right: Rect::new(30, 1, 30, 20),
            sidebar_list_top: 2,
            middle_list_top: 2,
            right_content_top: 2,
        }
    }

    #[test]
    fn q_does_not_quit() {
        // Plain `q` is now treated as NoOp so users can type `q` without exiting
        // (e.g. mid-scroll accidental presses). Ctrl+C is the only quit key.
        for focus in [Focus::Sidebar, Focus::Middle, Focus::Right] {
            assert_eq!(
                translate(focus, rects(), false, Prompt::None, key(KeyCode::Char('q'))),
                Action::NoOp
            );
        }
    }

    #[test]
    fn ctrl_c_quits() {
        assert_eq!(
            translate(Focus::Sidebar, rects(), false, Prompt::None, ctrl('c')),
            Action::Quit
        );
    }

    #[test]
    fn ctrl_y_maps_to_copy_to_clipboard() {
        for focus in [Focus::Sidebar, Focus::Middle, Focus::Right] {
            assert_eq!(
                translate(focus, rects(), false, Prompt::None, ctrl('y')),
                Action::CopyToClipboard
            );
        }
    }

    #[test]
    fn ctrl_n_maps_to_new_entry() {
        for focus in [Focus::Sidebar, Focus::Middle, Focus::Right] {
            assert_eq!(
                translate(focus, rects(), false, Prompt::None, ctrl('n')),
                Action::NewEntry
            );
        }
    }

    #[test]
    fn ctrl_d_maps_to_delete() {
        for focus in [Focus::Sidebar, Focus::Middle, Focus::Right] {
            assert_eq!(
                translate(focus, rects(), false, Prompt::None, ctrl('d')),
                Action::Delete
            );
        }
    }

    #[test]
    fn enter_on_entry_or_content_pane_starts_edit() {
        // Enter activates the selected entry from either the list or the
        // content pane; →/l still drill in to the read-only preview.
        for focus in [Focus::Middle, Focus::Right] {
            assert_eq!(
                translate(focus, rects(), false, Prompt::None, key(KeyCode::Enter)),
                Action::EnterEdit
            );
        }
        assert_eq!(
            translate(
                Focus::Middle,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Right)
            ),
            Action::Enter
        );
        assert_eq!(
            translate(
                Focus::Middle,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Char('l'))
            ),
            Action::Enter
        );
    }

    #[test]
    fn delete_prompt_routes_confirm_or_cancel() {
        // y / Enter confirm; n / Esc cancel; everything else is a no-op so the
        // prompt stays up until the user makes an explicit choice.
        let p = Prompt::DeleteEntry;
        assert_eq!(
            translate(Focus::Middle, rects(), false, p, key(KeyCode::Char('y'))),
            Action::ConfirmDelete
        );
        assert_eq!(
            translate(Focus::Middle, rects(), false, p, key(KeyCode::Enter)),
            Action::ConfirmDelete
        );
        assert_eq!(
            translate(Focus::Middle, rects(), false, p, key(KeyCode::Char('n'))),
            Action::CancelDelete
        );
        assert_eq!(
            translate(Focus::Middle, rects(), false, p, key(KeyCode::Esc)),
            Action::CancelDelete
        );
        assert_eq!(
            translate(Focus::Middle, rects(), false, p, key(KeyCode::Char('j'))),
            Action::NoOp
        );
    }

    #[test]
    fn enter_and_right_map_to_enter() {
        assert_eq!(
            translate(
                Focus::Sidebar,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Enter)
            ),
            Action::Enter
        );
        assert_eq!(
            translate(
                Focus::Sidebar,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Right)
            ),
            Action::Enter
        );
        assert_eq!(
            translate(
                Focus::Sidebar,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Char('l'))
            ),
            Action::Enter
        );
    }

    #[test]
    fn esc_and_left_map_to_back() {
        assert_eq!(
            translate(
                Focus::Middle,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Esc)
            ),
            Action::Back
        );
        assert_eq!(
            translate(
                Focus::Middle,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Left)
            ),
            Action::Back
        );
        assert_eq!(
            translate(
                Focus::Middle,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Char('h'))
            ),
            Action::Back
        );
        assert_eq!(
            translate(
                Focus::Middle,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Backspace)
            ),
            Action::Back
        );
    }

    #[test]
    fn down_scrolls_on_right_pane_but_moves_elsewhere() {
        assert_eq!(
            translate(
                Focus::Right,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Down)
            ),
            Action::ScrollRight(Delta::Down)
        );
        assert_eq!(
            translate(
                Focus::Sidebar,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Down)
            ),
            Action::Move(Delta::Down)
        );
    }

    #[test]
    fn tab_and_backtab_cycle() {
        assert_eq!(
            translate(
                Focus::Sidebar,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::Tab)
            ),
            Action::Cycle(CycleDir::Forward)
        );
        assert_eq!(
            translate(
                Focus::Sidebar,
                rects(),
                false,
                Prompt::None,
                key(KeyCode::BackTab)
            ),
            Action::Cycle(CycleDir::Backward)
        );
    }

    #[test]
    fn mouse_click_maps_to_correct_pane_and_offset() {
        let click = |x, y| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })
        };
        assert_eq!(
            translate(Focus::Sidebar, rects(), false, Prompt::None, click(5, 5)),
            Action::ClickSidebar(3)
        );
        assert_eq!(
            translate(Focus::Sidebar, rects(), false, Prompt::None, click(15, 5)),
            Action::ClickMiddle(3)
        );
        // Right-pane click starts a view-mode text selection.
        assert_eq!(
            translate(Focus::Sidebar, rects(), false, Prompt::None, click(40, 5)),
            Action::StartRightSelection { col: 9, line: 3 }
        );
        assert_eq!(
            translate(Focus::Sidebar, rects(), false, Prompt::None, click(100, 5)),
            Action::NoOp
        );
    }

    #[test]
    fn editing_routes_keys_to_editor() {
        assert_eq!(
            translate(
                Focus::Right,
                rects(),
                true,
                Prompt::None,
                key(KeyCode::Char('x'))
            ),
            Action::EditInsertChar('x')
        );
        assert_eq!(
            translate(Focus::Right, rects(), true, Prompt::None, key(KeyCode::Esc)),
            Action::EditExit
        );
        assert_eq!(
            translate(Focus::Right, rects(), true, Prompt::None, ctrl('s')),
            Action::EditSave
        );
    }

    #[test]
    fn confirming_routes_to_three_way_choice() {
        // The unsaved-changes prompt is modal and wins even while `editing` is
        // still true (the editor isn't torn down until the choice is made).
        assert_eq!(
            translate(
                Focus::Right,
                rects(),
                true,
                Prompt::UnsavedChanges,
                key(KeyCode::Char('y'))
            ),
            Action::ConfirmSave
        );
        assert_eq!(
            translate(
                Focus::Right,
                rects(),
                true,
                Prompt::UnsavedChanges,
                key(KeyCode::Char('n'))
            ),
            Action::ConfirmDiscard
        );
        assert_eq!(
            translate(
                Focus::Right,
                rects(),
                true,
                Prompt::UnsavedChanges,
                key(KeyCode::Esc)
            ),
            Action::CancelPrompt
        );
        // Stray keys are ignored so the prompt stays up.
        assert_eq!(
            translate(
                Focus::Right,
                rects(),
                true,
                Prompt::UnsavedChanges,
                key(KeyCode::Char('a'))
            ),
            Action::NoOp
        );
    }
}

use std::collections::HashMap;
use std::num::NonZeroUsize;

use anyhow::Result;
use lru::LruCache;

use crate::entry::attachments::{normalize_or_generate_entry_id, now_epoch_ms};
use crate::entry::body::build_entry_content;
use crate::entry::persistence::enqueue_entry_and_media_outbox;
use crate::store::sqlite::Store;
use crate::tui::actions;
use crate::tui::data::{
    self, DailyChatDay, DailyChatMessage, EntryBody, EntryPreview, JournalItem, UserSummary,
};
use crate::tui::editor::EntryEditor;
use crate::tui::event::{Action, CycleDir, Delta, Focus, PaneRects, Prompt};
use crate::util::now_rfc3339_utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarSection {
    Journal(usize),
    DailyChat,
}

/// An entry the user has asked to delete, awaiting y/n confirmation.
#[derive(Debug, Clone)]
struct PendingDelete {
    journal_id: String,
    entry_id: String,
    preview: String,
}

// No fixed row height: entries render variably (3 rows when no meta line
// and/or no divider after the last row, 4 otherwise). The UI layer stashes
// the actual rendered heights in `App::middle_row_heights` on every draw,
// and mouse hit-testing walks them.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor_col: u16,
    pub anchor_line: u16,
    pub focus_col: u16,
    pub focus_line: u16,
    pub dragging: bool,
}

impl TextSelection {
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        let start = (self.anchor_line, self.anchor_col);
        let end = (self.focus_line, self.focus_col);
        if start <= end {
            (start, end)
        } else {
            (end, start)
        }
    }
}

fn line_width(line: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    line.width()
}

fn slice_line_by_width(line: &str, start_col: u16, end_col: u16) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut col = 0u16;
    for ch in line.chars() {
        let width = ch.width().unwrap_or(0) as u16;
        let next = col.saturating_add(width);
        if next > start_col && col < end_col {
            out.push(ch);
        }
        col = next;
        if col >= end_col {
            break;
        }
    }
    out
}

/// What a pending unsaved-changes prompt should do once the user chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingConfirm {
    /// Leave edit mode (Esc).
    Exit,
    /// Quit the whole app (Ctrl+C).
    Quit,
}

/// The user's answer to an unsaved-changes prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmChoice {
    Save,
    Discard,
    KeepEditing,
}

pub struct App {
    pub profile_name: String,
    /// Base URL of the active profile, needed to enqueue delete operations.
    base_url: String,
    pub user: Option<UserSummary>,
    pub focus: Focus,
    pub journals: Vec<JournalItem>,
    pub daily_chat_days: Vec<DailyChatDay>,
    pub sidebar: SidebarSection,
    pub entries_by_journal: HashMap<String, Vec<EntryPreview>>,
    pub entry_bodies: LruCache<String, EntryBody>,
    pub daily_chat_messages: HashMap<String, Vec<DailyChatMessage>>,
    pub middle_index_entries: HashMap<String, usize>,
    pub middle_index_chat: usize,
    pub right_scroll: u16,
    pub status: Option<String>,
    pub pane_rects: PaneRects,
    /// Rendered row count per middle-pane item, populated by `ui::draw_middle`
    /// on every frame. Used by `Action::ClickMiddle` to map a pane-relative
    /// y-offset back to a logical item index.
    pub middle_row_heights: Vec<u16>,
    pub right_selection: Option<TextSelection>,
    pub right_rendered_lines: Vec<String>,
    /// Set when Ctrl+D is pressed on an entry; cleared on confirm/cancel.
    pending_delete: Option<PendingDelete>,
    pub should_quit: bool,
    /// Present while the content pane is in edit mode. Holds the scratch buffer
    /// for the entry being edited.
    pub editor: Option<EntryEditor>,
    /// Set while an unsaved-changes prompt is up, blocking edit input until the
    /// user picks save / discard / keep-editing.
    pending_confirm: Option<PendingConfirm>,
}

impl App {
    pub fn new(profile_name: String, base_url: &str, store: &Store) -> Result<Self> {
        let journals = data::load_journals(store)?;
        let daily_chat_days = data::load_daily_chat_days(store)?;
        let user = data::load_user_summary(store, base_url);
        let sidebar = if journals.is_empty() {
            SidebarSection::DailyChat
        } else {
            SidebarSection::Journal(0)
        };
        Ok(Self {
            profile_name,
            base_url: base_url.to_string(),
            user,
            focus: Focus::Sidebar,
            journals,
            daily_chat_days,
            sidebar,
            entries_by_journal: HashMap::new(),
            entry_bodies: LruCache::new(NonZeroUsize::new(32).unwrap()),
            daily_chat_messages: HashMap::new(),
            middle_index_entries: HashMap::new(),
            middle_index_chat: 0,
            right_scroll: 0,
            status: None,
            pane_rects: PaneRects::default(),
            middle_row_heights: Vec::new(),
            right_selection: None,
            right_rendered_lines: Vec::new(),
            pending_delete: None,
            should_quit: false,
            editor: None,
            pending_confirm: None,
        })
    }

    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    pub fn is_confirming(&self) -> bool {
        self.pending_confirm.is_some()
    }

    /// Which modal prompt (if any) is capturing input. Drives both the input
    /// router and the footer prompt text.
    pub fn prompt(&self) -> Prompt {
        if self.is_confirming_delete() {
            Prompt::DeleteEntry
        } else if self.is_confirming() {
            Prompt::UnsavedChanges
        } else {
            Prompt::None
        }
    }

    /// The persistent prompt text shown in the footer while a confirm is up.
    /// Returned separately from `status` so the status auto-clear timer can't
    /// make the prompt vanish while the modal is still active.
    pub fn confirm_prompt(&self) -> Option<String> {
        if let Some(pd) = &self.pending_delete {
            let label = if pd.preview.trim().is_empty() {
                "this entry".to_string()
            } else {
                pd.preview.chars().take(40).collect::<String>()
            };
            return Some(format!("Delete \"{label}\"? y confirm · n/Esc cancel"));
        }
        match self.pending_confirm {
            Some(PendingConfirm::Exit) => Some(
                "Unsaved changes — (y) save & exit · (n) discard · Esc keep editing".to_string(),
            ),
            Some(PendingConfirm::Quit) => Some(
                "Unsaved changes — (y) save & quit · (n) discard & quit · Esc keep editing"
                    .to_string(),
            ),
            None => None,
        }
    }

    pub fn apply(&mut self, action: Action, store: &Store) {
        match action {
            Action::Quit => {
                // Guard unsaved edits: prompt before quitting rather than
                // silently saving or silently discarding.
                if self.editor.as_ref().is_some_and(EntryEditor::is_dirty) {
                    self.pending_confirm = Some(PendingConfirm::Quit);
                } else {
                    self.should_quit = true;
                }
            }
            Action::NoOp => {}
            Action::Move(delta) => self.move_selection(delta, store),
            Action::Enter => self.enter_selection(store),
            Action::Back => self.back(),
            Action::Cycle(dir) => self.cycle_focus(dir),
            Action::ScrollRight(delta) => self.scroll_right(delta),
            Action::ClickSidebar(offset) => {
                self.focus = Focus::Sidebar;
                self.set_sidebar_by_offset(offset as usize);
                self.right_scroll = 0;
                self.ensure_middle_loaded(store);
                self.ensure_right_loaded(store);
            }
            Action::ClickMiddle(offset) => {
                self.focus = Focus::Middle;
                self.ensure_middle_loaded(store);
                let logical = self.middle_index_from_offset(offset as usize);
                self.set_middle_index(logical, store);
            }
            Action::StartRightSelection { col, line } => {
                self.focus = Focus::Right;
                let line = line.saturating_add(self.right_scroll);
                self.status = Some("Selecting… release, then Ctrl+Y to copy".to_string());
                self.right_selection = Some(TextSelection {
                    anchor_col: col,
                    anchor_line: line,
                    focus_col: col,
                    focus_line: line,
                    dragging: true,
                });
            }
            Action::UpdateRightSelection { col, line } => {
                if let Some(selection) = &mut self.right_selection {
                    selection.focus_col = col;
                    selection.focus_line = line.saturating_add(self.right_scroll);
                    self.status = Some("Selecting… release, then Ctrl+Y to copy".to_string());
                }
            }
            Action::FinishRightSelection { col, line } => {
                let Some(selection) = &mut self.right_selection else {
                    return;
                };
                selection.focus_col = col;
                selection.focus_line = line.saturating_add(self.right_scroll);
                selection.dragging = false;
                let msg = match self.selected_right_text() {
                    Some(text) => format!(
                        "Selection ready ({} chars) · Ctrl+Y to copy",
                        text.chars().count()
                    ),
                    None => "Selection empty".to_string(),
                };
                self.status = Some(msg);
            }
            Action::CopyToClipboard => {
                if self.right_selection.is_some() {
                    self.copy_right_selection_to_clipboard();
                } else {
                    self.copy_current_to_clipboard(store);
                }
            }
            Action::NewEntry => self.new_entry(store),
            Action::EnterEdit => self.edit_or_open(store),
            Action::EditInsertChar(c) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.insert_char(c);
                }
            }
            Action::EditInsertStr(s) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.insert_str(&s);
                }
            }
            Action::EditNewline => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.insert_newline();
                }
            }
            Action::EditBackspace => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.backspace();
                }
            }
            Action::EditDelete => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.delete_forward();
                }
            }
            Action::EditMove(mv, extend) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.move_cursor(mv, extend);
                }
            }
            Action::EditCopySelection => self.copy_selection_to_clipboard(),
            Action::EditSave => self.save_editor(store),
            Action::EditExit => self.request_edit_exit(),
            Action::ConfirmSave => self.resolve_confirm(store, ConfirmChoice::Save),
            Action::ConfirmDiscard => self.resolve_confirm(store, ConfirmChoice::Discard),
            Action::CancelPrompt => self.resolve_confirm(store, ConfirmChoice::KeepEditing),
            Action::EditClick(col, row) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.click(col as usize, row as usize);
                }
            }
            Action::EditDrag(col, row) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.drag(col as usize, row as usize);
                }
            }
            Action::EditScroll(delta) => {
                if let Some(ed) = self.editor.as_mut() {
                    match delta {
                        Delta::Up => ed.scroll_lines(-1, 1),
                        Delta::Down => ed.scroll_lines(1, 1),
                        Delta::PageUp => ed.scroll_lines(-1, 10),
                        Delta::PageDown => ed.scroll_lines(1, 10),
                        _ => {}
                    }
                }
            }
            Action::Delete => self.begin_delete(),
            Action::ConfirmDelete => self.confirm_delete(store),
            Action::CancelDelete => self.cancel_delete(),
        }
    }

    /// Create a fresh, empty entry in the current journal and open it for
    /// editing. The entry isn't written to the store until the first save, so
    /// backing out without typing leaves nothing behind.
    fn new_entry(&mut self, store: &Store) {
        // Resolve a target journal: the selected one, or the first available
        // when sitting in the daily-chat view.
        let journal_id = match self.sidebar {
            SidebarSection::Journal(i) => self.journals.get(i).map(|j| j.id.clone()),
            SidebarSection::DailyChat => self.journals.first().map(|j| j.id.clone()),
        };
        let Some(journal_id) = journal_id else {
            self.status = Some("new entry: no journal available".to_string());
            return;
        };
        // Make sure the chosen journal is the active sidebar selection so the
        // new entry lands in the right list after saving.
        if let Some(idx) = self.journals.iter().position(|j| j.id == journal_id) {
            self.sidebar = SidebarSection::Journal(idx);
            self.ensure_middle_loaded(store);
        }
        let entry_id = match normalize_or_generate_entry_id(None) {
            Ok(id) => id,
            Err(e) => {
                self.status = Some(format!("new entry failed: {e}"));
                return;
            }
        };
        self.focus = Focus::Right;
        self.right_selection = None;
        self.editor = Some(EntryEditor::new(journal_id, entry_id, ""));
        self.status = Some("new entry — type, then Ctrl+S save · Esc exit".to_string());
    }

    /// `Enter` on a selected entry. Journal entries open in the editor (with a
    /// live cursor); daily-chat days aren't editable, so they just open their
    /// messages in the content pane.
    fn edit_or_open(&mut self, store: &Store) {
        match self.sidebar {
            SidebarSection::Journal(_) => self.enter_edit_mode(store),
            SidebarSection::DailyChat => {
                self.ensure_right_loaded(store);
                self.focus = Focus::Right;
            }
        }
    }

    fn enter_edit_mode(&mut self, store: &Store) {
        // Only journal entries are editable (not the daily-chat view).
        let Some((entry_id, body)) = self.current_entry_body(store) else {
            self.status = Some("edit: select an entry first".to_string());
            return;
        };
        let Some(journal_id) = self.current_journal_id().map(str::to_owned) else {
            return;
        };
        self.right_selection = None;
        // Editing can be triggered from the entry list, so make sure the
        // content pane is focused (and stays focused after exit).
        self.focus = Focus::Right;
        self.editor = Some(EntryEditor::new(journal_id, entry_id, &body));
        self.status = Some("editing — Ctrl+S save · Esc exit".to_string());
    }

    /// Esc in edit mode. Edits commit only on an explicit save, so a clean
    /// buffer just closes; a dirty one raises the unsaved-changes prompt.
    fn request_edit_exit(&mut self) {
        match self.editor.as_ref() {
            Some(ed) if ed.is_dirty() => self.pending_confirm = Some(PendingConfirm::Exit),
            Some(_) => self.editor = None,
            None => {}
        }
    }

    /// Resolve a pending unsaved-changes prompt.
    fn resolve_confirm(&mut self, store: &Store, choice: ConfirmChoice) {
        let Some(pending) = self.pending_confirm else {
            return;
        };
        match choice {
            ConfirmChoice::KeepEditing => {
                self.pending_confirm = None;
                self.status = Some("Keeping changes".to_string());
                return;
            }
            ConfirmChoice::Save => self.save_editor(store),
            ConfirmChoice::Discard => self.status = Some("Discarded changes".to_string()),
        }
        // Both save and discard tear down the editor and complete the action.
        self.pending_confirm = None;
        self.editor = None;
        if pending == PendingConfirm::Quit {
            self.should_quit = true;
        }
    }

    /// Persist the editor's contents to the local store and outbox, mirroring
    /// `entry write`. No-op when nothing changed.
    fn save_editor(&mut self, store: &Store) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        if !ed.is_dirty() {
            self.status = Some("no changes".to_string());
            return;
        }
        let journal_id = ed.journal_id.clone();
        let entry_id = ed.entry_id.clone();
        let body = ed.to_text();
        match Self::persist_entry_body(store, &journal_id, &entry_id, body.clone()) {
            Ok(()) => {
                ed.mark_saved();
                // Keep the in-memory caches consistent so the rendered view and
                // middle-pane preview reflect the edit immediately.
                self.entry_bodies.put(
                    entry_id.clone(),
                    EntryBody {
                        body_markdown: body,
                    },
                );
                self.reload_current_entries(store);
                // Point the middle-pane selection at the saved entry so a newly
                // created entry is highlighted and rendered on exit.
                if let Some(pos) = self
                    .entries_by_journal
                    .get(&journal_id)
                    .and_then(|list| list.iter().position(|e| e.id == entry_id))
                {
                    self.middle_index_entries.insert(journal_id, pos);
                }
                self.status = Some("saved (queued for sync)".to_string());
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
        }
    }

    fn persist_entry_body(
        store: &Store,
        journal_id: &str,
        entry_id: &str,
        body: String,
    ) -> Result<()> {
        let now_iso = now_rfc3339_utc();
        let now_ms = now_epoch_ms();
        let existing = store
            .get_entry_json_row_with_extra_fields(entry_id)?
            .and_then(|(row, extra)| {
                serde_json::from_str::<serde_json::Value>(&row)
                    .ok()
                    .map(|value| (value, extra))
            });
        let content = build_entry_content(
            existing.as_ref().map(|(value, _)| value),
            existing.as_ref().and_then(|(_, extra)| extra.as_deref()),
            entry_id,
            body,
            None,
            now_ms,
            &[],
        )?;
        store.upsert_entry_json_row_with_edit_date(
            entry_id,
            journal_id,
            Some(&now_iso),
            Some(&now_iso),
            None,
            &content.to_string(),
        )?;
        enqueue_entry_and_media_outbox(
            store,
            journal_id,
            entry_id,
            &content,
            &now_iso,
            now_ms,
            &[],
        )?;
        Ok(())
    }

    fn copy_selection_to_clipboard(&mut self) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        let Some(text) = ed.selected_text() else {
            self.status = Some("copy: nothing selected".to_string());
            return;
        };
        let len = text.chars().count();
        match actions::copy_to_clipboard(&text) {
            Ok(()) => self.status = Some(format!("Copied {len} characters to clipboard")),
            Err(e) => self.status = Some(format!("copy failed: {e}")),
        }
    }

    pub fn is_confirming_delete(&self) -> bool {
        self.pending_delete.is_some()
    }

    /// Resolve the entry currently selected in the active journal, if any.
    fn current_delete_target(&self) -> Option<PendingDelete> {
        let journal_id = match self.sidebar {
            SidebarSection::Journal(i) => self.journals.get(i)?.id.clone(),
            SidebarSection::DailyChat => return None,
        };
        let idx = self
            .middle_index_entries
            .get(&journal_id)
            .copied()
            .unwrap_or(0);
        let entry = self.entries_by_journal.get(&journal_id)?.get(idx)?;
        Some(PendingDelete {
            journal_id,
            entry_id: entry.id.clone(),
            preview: entry.preview.clone(),
        })
    }

    fn begin_delete(&mut self) {
        match self.current_delete_target() {
            // The prompt itself is rendered from `confirm_prompt()`, which
            // survives the status auto-clear timer.
            Some(target) => self.pending_delete = Some(target),
            None => self.status = Some("delete: select an entry first".to_string()),
        }
    }

    fn confirm_delete(&mut self, store: &Store) {
        let Some(target) = self.pending_delete.take() else {
            return;
        };
        let base_url = self.base_url.clone();
        let result = crate::commands::entry_delete::execute(
            store,
            &base_url,
            crate::commands::entry_delete::EntryDeleteArgs {
                journal_id: target.journal_id,
                entry_id: target.entry_id,
            },
        );
        match result {
            Ok(_) => {
                // Reload so the tombstoned entry drops out of the sidebar and
                // selection lands on a neighbouring entry.
                self.refresh_from_store(store, &base_url);
                self.status = Some("Entry deleted".to_string());
            }
            Err(e) => self.status = Some(format!("delete failed: {e}")),
        }
    }

    fn cancel_delete(&mut self) {
        self.pending_delete = None;
        self.status = Some("Delete canceled".to_string());
    }

    fn current_entry_body(&mut self, store: &Store) -> Option<(String, String)> {
        match self.sidebar {
            SidebarSection::Journal(ji) => {
                let j = self.journals.get(ji).cloned()?;
                let idx = self.middle_index();
                let entry_id = self
                    .entries_by_journal
                    .get(&j.id)?
                    .get(idx)
                    .map(|p| p.id.clone())?;
                self.ensure_right_loaded(store);
                let body = self.entry_bodies.peek(&entry_id)?.body_markdown.clone();
                Some((entry_id, body))
            }
            SidebarSection::DailyChat => None,
        }
    }

    fn copy_current_to_clipboard(&mut self, store: &Store) {
        let Some((_, body)) = self.current_entry_body(store) else {
            self.status = Some("copy: select an entry first".to_string());
            return;
        };
        let len = body.chars().count();
        match actions::copy_to_clipboard(&body) {
            Ok(()) => self.status = Some(format!("Copied {len} characters to clipboard")),
            Err(e) => self.status = Some(format!("copy failed: {e}")),
        }
    }

    fn copy_right_selection_to_clipboard(&mut self) {
        let Some(text) = self.selected_right_text() else {
            self.status = Some("copy: drag over entry text first".to_string());
            return;
        };
        match actions::copy_to_clipboard(&text) {
            Ok(()) => self.status = Some("Copied selection".to_string()),
            Err(e) => self.status = Some(format!("copy failed: {e}")),
        }
    }

    fn selected_right_text(&self) -> Option<String> {
        let selection = self.right_selection?;
        let ((start_line, start_col), (end_line, end_col)) = selection.normalized();
        let mut selected = Vec::new();
        for line_idx in start_line..=end_line {
            let line = self.right_rendered_lines.get(line_idx as usize)?;
            let start = if line_idx == start_line { start_col } else { 0 };
            let end = if line_idx == end_line {
                end_col
            } else {
                line_width(line) as u16
            };
            selected.push(slice_line_by_width(line, start, end));
        }
        let text = selected.join("\n");
        (!text.is_empty()).then_some(text)
    }

    fn sidebar_length(&self) -> usize {
        self.journals.len() + 1
    }

    fn sidebar_flat_index(&self) -> usize {
        match self.sidebar {
            SidebarSection::Journal(i) => i,
            SidebarSection::DailyChat => self.journals.len(),
        }
    }

    fn set_sidebar_by_flat(&mut self, idx: usize) {
        if idx < self.journals.len() {
            self.sidebar = SidebarSection::Journal(idx);
        } else {
            self.sidebar = SidebarSection::DailyChat;
        }
    }

    fn set_sidebar_by_offset(&mut self, offset: usize) {
        let n = self.journals.len();
        if offset < n {
            self.sidebar = SidebarSection::Journal(offset);
        } else {
            self.sidebar = SidebarSection::DailyChat;
        }
    }

    /// Walk the per-row heights rendered on the previous frame and return the
    /// logical entry/chat-day index that contains the clicked y-offset.
    fn middle_index_from_offset(&self, offset: usize) -> usize {
        if self.middle_row_heights.is_empty() {
            return 0;
        }
        let mut acc = 0usize;
        for (i, h) in self.middle_row_heights.iter().enumerate() {
            acc += *h as usize;
            if offset < acc {
                return i;
            }
        }
        self.middle_row_heights.len() - 1
    }

    fn middle_list_len(&self) -> usize {
        match self.sidebar {
            SidebarSection::Journal(i) => self
                .journals
                .get(i)
                .and_then(|j| self.entries_by_journal.get(&j.id))
                .map(|v| v.len())
                .unwrap_or(0),
            SidebarSection::DailyChat => self.daily_chat_days.len(),
        }
    }

    fn middle_index(&self) -> usize {
        match self.sidebar {
            SidebarSection::Journal(i) => self
                .journals
                .get(i)
                .and_then(|j| self.middle_index_entries.get(&j.id).copied())
                .unwrap_or(0),
            SidebarSection::DailyChat => self.middle_index_chat,
        }
    }

    fn set_middle_index(&mut self, new_idx: usize, store: &Store) {
        let len = self.middle_list_len();
        let clamped = if len == 0 { 0 } else { new_idx.min(len - 1) };
        match self.sidebar {
            SidebarSection::Journal(i) => {
                if let Some(j) = self.journals.get(i) {
                    self.middle_index_entries.insert(j.id.clone(), clamped);
                }
            }
            SidebarSection::DailyChat => self.middle_index_chat = clamped,
        }
        self.right_scroll = 0;
        self.ensure_right_loaded(store);
    }

    fn move_selection(&mut self, delta: Delta, store: &Store) {
        match self.focus {
            Focus::Sidebar => {
                let len = self.sidebar_length();
                let cur = self.sidebar_flat_index();
                let next = move_index(cur, len, delta);
                self.set_sidebar_by_flat(next);
                self.right_scroll = 0;
                self.ensure_middle_loaded(store);
                self.ensure_right_loaded(store);
            }
            Focus::Middle => {
                let len = self.middle_list_len();
                if len == 0 {
                    return;
                }
                let cur = self.middle_index();
                let next = move_index(cur, len, delta);
                match self.sidebar {
                    SidebarSection::Journal(i) => {
                        if let Some(j) = self.journals.get(i) {
                            self.middle_index_entries.insert(j.id.clone(), next);
                        }
                    }
                    SidebarSection::DailyChat => self.middle_index_chat = next,
                }
                self.right_scroll = 0;
                self.ensure_right_loaded(store);
            }
            Focus::Right => self.scroll_right(delta),
        }
    }

    fn enter_selection(&mut self, store: &Store) {
        match self.focus {
            Focus::Sidebar => {
                self.ensure_middle_loaded(store);
                if self.middle_list_len() > 0 {
                    self.focus = Focus::Middle;
                    self.ensure_right_loaded(store);
                }
            }
            Focus::Middle => {
                if self.middle_list_len() > 0 {
                    self.ensure_right_loaded(store);
                    self.focus = Focus::Right;
                }
            }
            Focus::Right => {}
        }
    }

    fn back(&mut self) {
        match self.focus {
            Focus::Right => self.focus = Focus::Middle,
            Focus::Middle => self.focus = Focus::Sidebar,
            Focus::Sidebar => {}
        }
    }

    fn cycle_focus(&mut self, dir: CycleDir) {
        self.focus = match (self.focus, dir) {
            (Focus::Sidebar, CycleDir::Forward) => Focus::Middle,
            (Focus::Middle, CycleDir::Forward) => Focus::Right,
            (Focus::Right, CycleDir::Forward) => Focus::Sidebar,
            (Focus::Sidebar, CycleDir::Backward) => Focus::Right,
            (Focus::Middle, CycleDir::Backward) => Focus::Sidebar,
            (Focus::Right, CycleDir::Backward) => Focus::Middle,
        };
    }

    /// Largest scroll offset that still keeps content on screen. Zero when the
    /// rendered content fits the content pane, which locks scrolling entirely.
    /// `right_rendered_lines` and `pane_rects` are both refreshed every draw, so
    /// this reflects the current entry and terminal size.
    fn max_right_scroll(&self) -> u16 {
        let content = self.right_rendered_lines.len() as u16;
        // Subtract the top and bottom borders to get the visible content height.
        let viewport = self.pane_rects.right.height.saturating_sub(2);
        content.saturating_sub(viewport)
    }

    fn scroll_right(&mut self, delta: Delta) {
        let max = self.max_right_scroll();
        self.right_scroll = match delta {
            Delta::Up => self.right_scroll.saturating_sub(1),
            Delta::Down => self.right_scroll.saturating_add(1).min(max),
            Delta::PageUp => self.right_scroll.saturating_sub(10),
            Delta::PageDown => self.right_scroll.saturating_add(10).min(max),
            Delta::Home => 0,
            Delta::End => max,
        };
    }

    pub fn refresh_from_store(&mut self, store: &Store, base_url: &str) {
        let selected_journal_id = self.current_journal_id().map(str::to_owned);
        let selected_entry_id = self.current_entry_id().map(str::to_owned);
        let selected_chat_day_id = self.current_chat_day_id().map(str::to_owned);
        let old_middle_index = self.middle_index();
        let old_right_scroll = self.right_scroll;

        match data::load_journals(store) {
            Ok(journals) => self.journals = journals,
            Err(e) => self.status = Some(format!("refresh journals failed: {e}")),
        }
        match data::load_daily_chat_days(store) {
            Ok(days) => self.daily_chat_days = days,
            Err(e) => self.status = Some(format!("refresh daily chat failed: {e}")),
        }
        self.user = data::load_user_summary(store, base_url);

        self.restore_sidebar(selected_journal_id.as_deref());
        self.entries_by_journal.clear();
        self.entry_bodies.clear();
        self.daily_chat_messages.clear();

        match self.sidebar {
            SidebarSection::Journal(_) => {
                self.reload_current_entries(store);
                self.restore_entry_selection(selected_entry_id.as_deref(), old_middle_index);
            }
            SidebarSection::DailyChat => {
                self.restore_chat_selection(selected_chat_day_id.as_deref(), old_middle_index);
            }
        }

        let new_entry_id = self.current_entry_id().map(str::to_owned);
        let new_chat_day_id = self.current_chat_day_id().map(str::to_owned);
        if selected_entry_id == new_entry_id && selected_chat_day_id == new_chat_day_id {
            self.right_scroll = old_right_scroll;
        } else {
            self.right_scroll = 0;
        }
        self.ensure_right_loaded(store);
    }

    fn current_journal_id(&self) -> Option<&str> {
        match self.sidebar {
            SidebarSection::Journal(i) => self.journals.get(i).map(|j| j.id.as_str()),
            SidebarSection::DailyChat => None,
        }
    }

    fn current_entry_id(&self) -> Option<&str> {
        let journal_id = self.current_journal_id()?;
        let idx = self
            .middle_index_entries
            .get(journal_id)
            .copied()
            .unwrap_or(0);
        self.entries_by_journal
            .get(journal_id)?
            .get(idx)
            .map(|p| p.id.as_str())
    }

    fn current_chat_day_id(&self) -> Option<&str> {
        match self.sidebar {
            SidebarSection::DailyChat => self
                .daily_chat_days
                .get(self.middle_index_chat)
                .map(|d| d.id.as_str()),
            SidebarSection::Journal(_) => None,
        }
    }

    fn restore_sidebar(&mut self, selected_journal_id: Option<&str>) {
        if let Some(journal_id) = selected_journal_id
            && let Some(index) = self.journals.iter().position(|j| j.id == journal_id)
        {
            self.sidebar = SidebarSection::Journal(index);
            return;
        }
        if matches!(self.sidebar, SidebarSection::DailyChat) || self.journals.is_empty() {
            self.sidebar = SidebarSection::DailyChat;
        } else {
            self.sidebar = SidebarSection::Journal(0);
        }
    }

    fn reload_current_entries(&mut self, store: &Store) {
        let Some(journal_id) = self.current_journal_id().map(str::to_owned) else {
            return;
        };
        match data::load_entries(store, &journal_id) {
            Ok(rows) => {
                self.entries_by_journal.insert(journal_id, rows);
            }
            Err(e) => self.status = Some(format!("refresh entries failed: {e}")),
        }
    }

    fn restore_entry_selection(&mut self, selected_entry_id: Option<&str>, old_index: usize) {
        let Some(journal_id) = self.current_journal_id().map(str::to_owned) else {
            return;
        };
        let len = self
            .entries_by_journal
            .get(&journal_id)
            .map(Vec::len)
            .unwrap_or(0);
        let restored = selected_entry_id
            .and_then(|entry_id| {
                self.entries_by_journal
                    .get(&journal_id)?
                    .iter()
                    .position(|entry| entry.id == entry_id)
            })
            .unwrap_or_else(|| old_index.min(len.saturating_sub(1)));
        self.middle_index_entries.insert(journal_id, restored);
    }

    fn restore_chat_selection(&mut self, selected_chat_day_id: Option<&str>, old_index: usize) {
        let len = self.daily_chat_days.len();
        self.middle_index_chat = selected_chat_day_id
            .and_then(|day_id| self.daily_chat_days.iter().position(|day| day.id == day_id))
            .unwrap_or_else(|| old_index.min(len.saturating_sub(1)));
    }

    pub fn ensure_middle_loaded(&mut self, store: &Store) {
        match self.sidebar {
            SidebarSection::Journal(i) => {
                let Some(j) = self.journals.get(i).cloned() else {
                    return;
                };
                if !self.entries_by_journal.contains_key(&j.id) {
                    match data::load_entries(store, &j.id) {
                        Ok(rows) => {
                            self.entries_by_journal.insert(j.id.clone(), rows);
                        }
                        Err(e) => self.status = Some(format!("load entries failed: {e}")),
                    }
                }
            }
            SidebarSection::DailyChat => {}
        }
    }

    pub fn ensure_right_loaded(&mut self, store: &Store) {
        match self.sidebar {
            SidebarSection::Journal(ji) => {
                let Some(j) = self.journals.get(ji).cloned() else {
                    return;
                };
                let idx = self.middle_index();
                let Some(entry_id) = self
                    .entries_by_journal
                    .get(&j.id)
                    .and_then(|list| list.get(idx))
                    .map(|p| p.id.clone())
                else {
                    return;
                };
                if self.entry_bodies.peek(&entry_id).is_none() {
                    match data::load_entry_body(store, &entry_id) {
                        Ok(body) => {
                            self.entry_bodies.put(entry_id, body);
                        }
                        Err(e) => self.status = Some(format!("load entry failed: {e}")),
                    }
                }
            }
            SidebarSection::DailyChat => {
                let Some(day) = self.daily_chat_days.get(self.middle_index_chat).cloned() else {
                    return;
                };
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.daily_chat_messages.entry(day.id.clone())
                {
                    match data::load_daily_chat_messages(store, &day.id) {
                        Ok(msgs) => {
                            entry.insert(msgs);
                        }
                        Err(e) => self.status = Some(format!("load chat failed: {e}")),
                    }
                }
            }
        }
    }
}

fn move_index(cur: usize, len: usize, delta: Delta) -> usize {
    if len == 0 {
        return 0;
    }
    match delta {
        Delta::Up => cur.saturating_sub(1),
        Delta::Down => (cur + 1).min(len - 1),
        Delta::Home => 0,
        Delta::End => len - 1,
        Delta::PageUp => cur.saturating_sub(10),
        Delta::PageDown => (cur + 10).min(len - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::sqlite::{STAGING_URL, Store};
    use crate::tui::editor::Move as EditMove;

    fn test_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dayone.db");
        let store = Store::open_at(&path).expect("store");
        (dir, store)
    }

    fn upsert_journal(store: &Store, id: &str, name: &str) {
        store
            .upsert_json_row(
                "journals",
                id,
                Some("2026-05-01T00:00:00.000Z"),
                None,
                &format!(r#"{{"id":"{id}","name":"{name}"}}"#),
            )
            .expect("journal");
    }

    fn upsert_entry(store: &Store, id: &str, journal_id: &str, date: i64, body: &str) {
        store
            .upsert_entry_json_row(
                id,
                journal_id,
                Some("2026-05-01T00:00:00.000Z"),
                None,
                &format!(
                    r#"{{"id":"{id}","journal_id":"{journal_id}","date":{date},"body":"{body}"}}"#
                ),
            )
            .expect("entry");
    }

    #[test]
    fn refresh_picks_up_new_entries_and_preserves_selected_entry() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-old", "journal-1", 1_000, "old body");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);
        assert_eq!(app.current_entry_id(), Some("entry-old"));

        upsert_entry(&store, "entry-new", "journal-1", 2_000, "new body");
        app.refresh_from_store(&store, STAGING_URL);

        let entries = app.entries_by_journal.get("journal-1").expect("entries");
        let ids = entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["entry-new", "entry-old"]);
        assert_eq!(app.current_entry_id(), Some("entry-old"));
        assert_eq!(app.middle_index(), 1);
    }

    #[test]
    fn refresh_invalidates_cached_entry_body() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "first body");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);
        assert_eq!(
            app.entry_bodies
                .peek("entry-1")
                .map(|body| body.body_markdown.as_str()),
            Some("first body")
        );

        upsert_entry(&store, "entry-1", "journal-1", 1_000, "updated body");
        app.refresh_from_store(&store, STAGING_URL);

        assert_eq!(
            app.entry_bodies
                .peek("entry-1")
                .map(|body| body.body_markdown.as_str()),
            Some("updated body")
        );
    }

    #[test]
    fn refresh_preserves_right_scroll_when_selected_entry_is_unchanged() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-old", "journal-1", 1_000, "old body");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);
        app.right_scroll = 42;

        upsert_entry(&store, "entry-new", "journal-1", 2_000, "new body");
        app.refresh_from_store(&store, STAGING_URL);

        assert_eq!(app.current_entry_id(), Some("entry-old"));
        assert_eq!(app.right_scroll, 42);
    }

    #[test]
    fn right_scroll_is_locked_when_content_fits_the_pane() {
        use ratatui::layout::Rect;
        let (_dir, store) = test_store();
        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        // 10-row pane => 8 content rows; 3 lines of content fit comfortably.
        app.pane_rects.right = Rect::new(0, 0, 40, 10);
        app.right_rendered_lines = vec!["a".into(), "b".into(), "c".into()];

        app.apply(Action::ScrollRight(Delta::Down), &store);
        assert_eq!(
            app.right_scroll, 0,
            "down must not scroll when content fits"
        );
        app.apply(Action::ScrollRight(Delta::End), &store);
        assert_eq!(app.right_scroll, 0, "end must not scroll when content fits");
    }

    #[test]
    fn right_scroll_clamps_to_overflow() {
        use ratatui::layout::Rect;
        let (_dir, store) = test_store();
        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        // 10-row pane => 8 content rows; 20 lines overflow by 12.
        app.pane_rects.right = Rect::new(0, 0, 40, 10);
        app.right_rendered_lines = (0..20).map(|i| i.to_string()).collect();

        app.apply(Action::ScrollRight(Delta::End), &store);
        assert_eq!(app.right_scroll, 12, "end stops at last screenful");
        app.apply(Action::ScrollRight(Delta::Down), &store);
        assert_eq!(app.right_scroll, 12, "cannot scroll past the bottom");
        app.apply(Action::ScrollRight(Delta::Up), &store);
        assert_eq!(app.right_scroll, 11);
    }

    #[test]
    fn refresh_resets_right_scroll_when_selected_entry_disappears() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_journal(&store, "journal-2", "Other");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "body");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);
        app.right_scroll = 42;

        store
            .remap_journal_id("journal-1", "journal-2")
            .expect("entry should move journals");
        app.refresh_from_store(&store, STAGING_URL);

        assert_eq!(app.current_entry_id(), None);
        assert_eq!(app.middle_index(), 0);
        assert_eq!(app.right_scroll, 0);
    }

    #[test]
    fn refresh_handles_empty_current_journal_without_stale_body() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_journal(&store, "journal-2", "Other");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "body");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);
        assert!(app.entry_bodies.peek("entry-1").is_some());

        store
            .remap_journal_id("journal-1", "journal-2")
            .expect("entry should move journals");
        app.refresh_from_store(&store, STAGING_URL);

        assert_eq!(app.middle_list_len(), 0);
        assert_eq!(app.middle_index(), 0);
        assert!(app.entry_bodies.peek("entry-1").is_none());
    }

    fn upsert_daily_chat(store: &Store, id: &str, date: &str, messages: &[&str]) {
        let messages_json = messages
            .iter()
            .map(|text| serde_json::json!({ "role": "user", "text": text }))
            .collect::<Vec<_>>();
        store
            .upsert_json_row(
                "daily_chat_feed",
                id,
                Some("2026-05-01T00:00:00.000Z"),
                None,
                &serde_json::json!({
                    "id": id,
                    "date": date,
                    "messages": messages_json,
                })
                .to_string(),
            )
            .expect("daily chat should save");
    }

    #[test]
    fn refresh_updates_daily_chat_days_and_messages() {
        let (_dir, store) = test_store();
        upsert_daily_chat(&store, "chat-1", "2026-05-01", &["hello"]);

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        assert!(matches!(app.sidebar, SidebarSection::DailyChat));
        app.ensure_right_loaded(&store);
        assert_eq!(app.current_chat_day_id(), Some("chat-1"));
        assert_eq!(app.daily_chat_messages["chat-1"].len(), 1);

        upsert_daily_chat(&store, "chat-1", "2026-05-01", &["hello", "again"]);
        upsert_daily_chat(&store, "chat-2", "2026-05-02", &["newer"]);
        app.refresh_from_store(&store, STAGING_URL);

        let ids = app
            .daily_chat_days
            .iter()
            .map(|day| day.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["chat-2", "chat-1"]);
        assert_eq!(app.current_chat_day_id(), Some("chat-1"));
        assert_eq!(app.middle_index_chat, 1);
        assert_eq!(app.daily_chat_messages["chat-1"].len(), 2);
    }

    #[test]
    fn enter_from_entry_list_opens_editor_focused_on_content() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "hello");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);
        app.focus = Focus::Middle;

        app.apply(Action::EnterEdit, &store);

        assert!(
            app.is_editing(),
            "Enter on a list entry should start editing"
        );
        assert!(matches!(app.focus, Focus::Right));
    }

    #[test]
    fn enter_on_chat_day_opens_preview_not_editor() {
        let (_dir, store) = test_store();
        upsert_daily_chat(&store, "chat-1", "2026-05-01", &["hi"]);

        // No journals => sidebar starts on DailyChat.
        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.focus = Focus::Middle;

        app.apply(Action::EnterEdit, &store);

        assert!(!app.is_editing(), "chat days are not editable");
        assert!(matches!(app.focus, Focus::Right));
    }

    #[test]
    fn ctrl_d_confirm_deletes_entry_and_refreshes_sidebar() {
        let (_dir, store) = test_store();
        // entry_delete::execute requires an auth session for the base URL.
        let profile = store
            .get_or_create_profile_for_base_url(STAGING_URL)
            .expect("profile");
        store
            .save_auth_session(
                profile.id,
                "tok",
                "2026-03-17T00:00:00.000Z",
                r#"{"id":"test-user"}"#,
            )
            .expect("session");
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "first");
        upsert_entry(&store, "entry-2", "journal-1", 2_000, "second");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.focus = Focus::Middle;
        app.ensure_middle_loaded(&store);
        // Select the newer entry (sorted desc, index 0 == entry-2).
        assert_eq!(app.current_entry_id(), Some("entry-2"));

        app.apply(Action::Delete, &store);
        assert!(app.is_confirming_delete());

        app.apply(Action::ConfirmDelete, &store);
        assert!(!app.is_confirming_delete());

        let ids = app
            .entries_by_journal
            .get("journal-1")
            .expect("entries")
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["entry-1"], "deleted entry should be hidden");
        assert_eq!(app.status.as_deref(), Some("Entry deleted"));
    }

    #[test]
    fn ctrl_d_cancel_keeps_entry() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "first");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.focus = Focus::Middle;
        app.ensure_middle_loaded(&store);

        app.apply(Action::Delete, &store);
        assert!(app.is_confirming_delete());
        app.apply(Action::CancelDelete, &store);

        assert!(!app.is_confirming_delete());
        assert_eq!(app.current_entry_id(), Some("entry-1"));
        assert_eq!(app.status.as_deref(), Some("Delete canceled"));
    }

    #[test]
    fn successful_refresh_does_not_set_status_message() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "body");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);
        app.status = Some("keep me".to_string());

        upsert_entry(&store, "entry-2", "journal-1", 2_000, "new body");
        app.refresh_from_store(&store, STAGING_URL);

        assert_eq!(app.status.as_deref(), Some("keep me"));
    }

    fn entry_body_in_store(store: &Store, entry_id: &str) -> String {
        let raw = store
            .get_json_row_by_id("entries", entry_id)
            .expect("query")
            .expect("row");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        value
            .get("body")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("text").and_then(serde_json::Value::as_str))
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn enter_edit_loads_body_into_editor() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "hello");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.focus = Focus::Right;
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);

        app.apply(Action::EnterEdit, &store);
        let ed = app.editor.as_ref().expect("editor present");
        assert_eq!(ed.entry_id, "entry-1");
        assert_eq!(ed.journal_id, "journal-1");
        assert_eq!(ed.to_text(), "hello");
    }

    #[test]
    fn typing_then_save_persists_and_enqueues_outbox() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "hi");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.focus = Focus::Right;
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);

        app.apply(Action::EnterEdit, &store);
        app.apply(Action::EditMove(EditMove::DocEnd, false), &store);
        app.apply(Action::EditInsertChar('!'), &store);
        app.apply(Action::EditSave, &store);

        assert_eq!(entry_body_in_store(&store, "entry-1"), "hi!");
        // The save mirrors `entry write`, enqueuing an entry update outbox item.
        let leased = store
            .lease_outbox_items(i64::MAX, 10)
            .expect("outbox query");
        assert!(
            leased
                .iter()
                .any(|item| item.resource == "entry" && item.object_id == "journal-1:entry-1")
        );
        // Cached body reflects the edit so the rendered view updates at once.
        assert_eq!(
            app.entry_bodies
                .peek("entry-1")
                .map(|b| b.body_markdown.as_str()),
            Some("hi!")
        );
    }

    #[test]
    fn new_entry_creates_and_selects_entry_in_journal() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-old", "journal-1", 1_000, "old");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);

        app.apply(Action::NewEntry, &store);
        let new_id = app
            .editor
            .as_ref()
            .expect("editor present")
            .entry_id
            .clone();
        assert_ne!(new_id, "entry-old");
        assert!(matches!(app.focus, Focus::Right));

        app.apply(Action::EditInsertStr("brand new".to_string()), &store);
        // Esc on a dirty buffer prompts; choosing save commits and exits.
        app.apply(Action::EditExit, &store);
        assert!(app.is_confirming());
        app.apply(Action::ConfirmSave, &store);

        assert!(app.editor.is_none());
        assert!(!app.is_confirming());
        // The new entry is persisted and selected in the middle pane.
        assert_eq!(entry_body_in_store(&store, &new_id), "brand new");
        assert_eq!(app.current_entry_id(), Some(new_id.as_str()));
    }

    #[test]
    fn new_entry_without_typing_writes_nothing() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");

        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.ensure_middle_loaded(&store);

        app.apply(Action::NewEntry, &store);
        let new_id = app
            .editor
            .as_ref()
            .expect("editor present")
            .entry_id
            .clone();
        app.apply(Action::EditExit, &store);

        // Empty entry was never dirty, so nothing hit the store.
        assert!(app.editor.is_none());
        assert!(
            store
                .get_json_row_by_id("entries", &new_id)
                .expect("query")
                .is_none()
        );
    }

    /// Helper: open an existing entry in the editor and append `extra`.
    fn dirty_editor(store: &Store, body: &str, extra: &str) -> App {
        upsert_journal(store, "journal-1", "Journal");
        upsert_entry(store, "entry-1", "journal-1", 1_000, body);
        let mut app = App::new("staging".to_string(), STAGING_URL, store).expect("app");
        app.focus = Focus::Right;
        app.ensure_middle_loaded(store);
        app.ensure_right_loaded(store);
        app.apply(Action::EnterEdit, store);
        app.apply(Action::EditMove(EditMove::DocEnd, false), store);
        app.apply(Action::EditInsertStr(extra.to_string()), store);
        app
    }

    #[test]
    fn esc_save_choice_persists_and_clears_editor() {
        let (_dir, store) = test_store();
        let mut app = dirty_editor(&store, "abc", "def");

        app.apply(Action::EditExit, &store);
        assert!(app.is_confirming());
        app.apply(Action::ConfirmSave, &store);

        assert!(app.editor.is_none());
        assert!(!app.is_confirming());
        assert_eq!(entry_body_in_store(&store, "entry-1"), "abcdef");
    }

    #[test]
    fn esc_discard_choice_drops_changes() {
        let (_dir, store) = test_store();
        let mut app = dirty_editor(&store, "abc", "def");

        app.apply(Action::EditExit, &store);
        assert!(app.is_confirming());
        app.apply(Action::ConfirmDiscard, &store);

        assert!(app.editor.is_none());
        assert!(!app.is_confirming());
        // The on-disk body is untouched by a discarded edit.
        assert_eq!(entry_body_in_store(&store, "entry-1"), "abc");
    }

    #[test]
    fn esc_keep_editing_dismisses_prompt_and_stays() {
        let (_dir, store) = test_store();
        let mut app = dirty_editor(&store, "abc", "def");

        app.apply(Action::EditExit, &store);
        assert!(app.is_confirming());
        app.apply(Action::CancelPrompt, &store);

        assert!(!app.is_confirming());
        assert!(app.editor.is_some(), "should still be editing");
        assert_eq!(entry_body_in_store(&store, "entry-1"), "abc");
    }

    #[test]
    fn esc_on_clean_editor_exits_without_prompt() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        upsert_entry(&store, "entry-1", "journal-1", 1_000, "abc");
        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");
        app.focus = Focus::Right;
        app.ensure_middle_loaded(&store);
        app.ensure_right_loaded(&store);
        app.apply(Action::EnterEdit, &store);

        app.apply(Action::EditExit, &store);

        assert!(!app.is_confirming());
        assert!(app.editor.is_none());
    }

    #[test]
    fn quit_while_dirty_prompts_then_discards_and_quits() {
        let (_dir, store) = test_store();
        let mut app = dirty_editor(&store, "abc", "def");

        app.apply(Action::Quit, &store);
        assert!(app.is_confirming());
        assert!(!app.should_quit, "must not quit before the user chooses");

        app.apply(Action::ConfirmDiscard, &store);
        assert!(app.should_quit);
        assert_eq!(entry_body_in_store(&store, "entry-1"), "abc");
    }

    #[test]
    fn quit_while_dirty_can_save_then_quit() {
        let (_dir, store) = test_store();
        let mut app = dirty_editor(&store, "abc", "def");

        app.apply(Action::Quit, &store);
        app.apply(Action::ConfirmSave, &store);

        assert!(app.should_quit);
        assert_eq!(entry_body_in_store(&store, "entry-1"), "abcdef");
    }

    #[test]
    fn quit_while_clean_quits_immediately() {
        let (_dir, store) = test_store();
        upsert_journal(&store, "journal-1", "Journal");
        let mut app = App::new("staging".to_string(), STAGING_URL, &store).expect("app");

        app.apply(Action::Quit, &store);

        assert!(app.should_quit);
        assert!(!app.is_confirming());
    }
}

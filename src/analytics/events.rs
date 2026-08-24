//! Product analytics event names.
//!
//! Values mirror Day One Web's `EVENT` enum (`src/analytics/events.ts`) so the
//! CLI reports into the same Automattic Tracks vocabulary where the concepts
//! line up. The platform prefix (`dayone_cli_`) is applied at send time, just
//! as the web client prefixes with `dayone_web_` / `dayone_desktop_`.

/// A trackable CLI event. The string value is the un-prefixed Tracks event
/// suffix (snake_case), matching the corresponding Day One Web event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    // Auth.
    UserSignIn,
    UserSignOut,
    // Entries.
    EntryCreate,
    EntryEditFinish,
    EntryDelete,
    // Journals.
    JournalCreate,
    JournalUpdate,
    // Entry comments.
    EntryCommentAdded,
    EntryCommentEdited,
    EntryCommentDeleted,
    EntryCommentReactionAdded,
    EntryCommentReactionDeleted,
    // Daily chat.
    DailyChatEntryUpdated,
    // Generic command-lifecycle event fired once per invocation.
    CommandRun,
}

impl Event {
    pub(crate) const ALL: [Self; 14] = [
        Self::UserSignIn,
        Self::UserSignOut,
        Self::EntryCreate,
        Self::EntryEditFinish,
        Self::EntryDelete,
        Self::JournalCreate,
        Self::JournalUpdate,
        Self::EntryCommentAdded,
        Self::EntryCommentEdited,
        Self::EntryCommentDeleted,
        Self::EntryCommentReactionAdded,
        Self::EntryCommentReactionDeleted,
        Self::DailyChatEntryUpdated,
        Self::CommandRun,
    ];

    /// The un-prefixed snake_case event name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserSignIn => "user_sign_in",
            Self::UserSignOut => "user_sign_out",
            Self::EntryCreate => "entry_create",
            Self::EntryEditFinish => "entry_edit_finish",
            Self::EntryDelete => "entry_delete",
            Self::JournalCreate => "journal_create",
            Self::JournalUpdate => "journal_update",
            Self::EntryCommentAdded => "entry_comment_added",
            Self::EntryCommentEdited => "entry_comment_edited",
            Self::EntryCommentDeleted => "entry_comment_deleted",
            Self::EntryCommentReactionAdded => "entry_comment_reaction_added",
            Self::EntryCommentReactionDeleted => "entry_comment_reaction_deleted",
            Self::DailyChatEntryUpdated => "daily_chat_entry_updated",
            Self::CommandRun => "command_run",
        }
    }

    pub(crate) fn full_name(self) -> String {
        format!("dayone_cli_{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_valid_tracks_names() {
        // Every mapped event must be a valid Tracks name once prefixed.
        for event in Event::ALL {
            let full = event.full_name();
            assert!(
                crate::analytics::tracks::is_valid_name(&full),
                "event name `{full}` should be a valid Tracks name"
            );
        }
    }
}

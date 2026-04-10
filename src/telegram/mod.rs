//! Telegram data layer — actor model.
//!
//! The UI never calls Telegram directly. Instead it holds a [`TgHandle`]
//! (command sender) and receives [`TgEvent`]s through the main event loop.
//! This keeps the draw path synchronous while the real client runs on its
//! own task and can block on network I/O freely.

pub mod grammers;
pub mod mock;

use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Chat {
    pub id: String,
    pub name: String,
    pub username: Option<String>,
    pub last_preview: String,
    pub unread: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MediaKind {
    Photo,
    Video,
    Document,
    Sticker,
    Other,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Message {
    pub id: u64,
    pub sender: String,
    pub text: String,
    pub timestamp: chrono::DateTime<chrono::Local>,
    pub outgoing: bool,
    /// Id of the message this one is a reply to, if any. The UI resolves
    /// the preview at render time by looking up the target in the current
    /// chat's message list.
    pub reply_to_id: Option<u64>,
    /// Whether Telegram has recorded any edit on this message.
    pub edited: bool,
    /// Classification of any attached media. `None` for plain text messages.
    pub media_kind: Option<MediaKind>,
    /// Local cache path of the downloaded media file, or `None` if we
    /// haven't fetched it yet.
    pub media_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub enum Command {
    LoadChats,
    LoadMessages(String),
    /// Fetch a batch of messages older than `before_id` for the given chat.
    /// Used by the UI's scrollback path.
    LoadOlder {
        chat_id: String,
        before_id: u64,
    },
    Send {
        chat_id: String,
        text: String,
        /// Message id to reply to, if any. Interpreted as `i32` on the wire
        /// (Telegram's native size) but kept as `u64` here to match our
        /// `Message::id`.
        reply_to: Option<u64>,
    },
    Edit {
        chat_id: String,
        msg_id: u64,
        text: String,
    },
    /// Download the media file attached to `(chat_id, msg_id)`, cache it,
    /// and emit [`TgEvent::MediaFetched`] with the resolved path.
    FetchMedia {
        chat_id: String,
        msg_id: u64,
    },
    /// Delete one or more messages in a chat. For private chats and groups
    /// this revokes on both sides; the server will reject if the user lacks
    /// permission.
    Delete {
        chat_id: String,
        ids: Vec<u64>,
    },
    /// Send a one-shot "typing" action to the given chat. Telegram remembers
    /// this for ~4 seconds; the UI should re-issue periodically while the
    /// user is actively composing.
    SendTyping {
        chat_id: String,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum TgEvent {
    Connected,
    Disconnected,
    Error(String),
    ChatsLoaded(Vec<Chat>),
    MessagesLoaded {
        chat_id: String,
        messages: Vec<Message>,
    },
    /// Result of a scrollback fetch. `messages` are chronologically ordered
    /// (oldest first) and should be prepended to the UI's current view.
    /// Empty vec means we reached the start of history.
    OlderMessagesLoaded {
        chat_id: String,
        messages: Vec<Message>,
    },
    MessageSent {
        chat_id: String,
    },
    /// An incoming message landed in `chat_id`. Emitted by the live update
    /// stream; the UI decides whether to append, bump unread, or reorder.
    NewMessage {
        chat_id: String,
        message: Message,
    },
    /// A message in `chat_id` was edited. The full updated message is included.
    MessageEdited {
        chat_id: String,
        message: Message,
    },
    /// One or more message ids were deleted. `chat_id` is `Some` for channel
    /// deletes (Telegram routes them through a channel) and `None` for
    /// private/basic-group deletes — in that case the UI should scan the
    /// currently-open chat for matching ids.
    MessageDeleted {
        chat_id: Option<String>,
        ids: Vec<u64>,
    },
    /// A media file finished downloading to a local cache path.
    MediaFetched {
        chat_id: String,
        msg_id: u64,
        path: std::path::PathBuf,
    },
    /// Someone is typing (or just stopped) in the given chat. `active`
    /// distinguishes start/continue from cancel.
    Typing {
        chat_id: String,
        active: bool,
    },
}

pub struct TgHandle {
    pub cmd: UnboundedSender<Command>,
}

impl TgHandle {
    pub fn send(&self, c: Command) {
        let _ = self.cmd.send(c);
    }
}

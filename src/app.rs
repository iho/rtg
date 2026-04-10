//! Global application state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::keymap::dispatch;
use crate::telegram::{Chat, Command, MediaKind, Message, TgEvent, TgHandle};
use crate::ui::modals::fuzzy::{FuzzyPicker, FuzzyTarget};

/// A pending reply: which message is being quoted, plus enough info to
/// render the banner above the input bar.
#[derive(Debug, Clone)]
pub struct ReplyTarget {
    pub msg_id: u64,
    pub sender: String,
    pub preview: String,
}

/// A pending edit of one of the user's own outgoing messages.
#[derive(Debug, Clone)]
pub struct EditTarget {
    pub msg_id: u64,
}

/// Live state for the image-viewer modal: the decoded image wrapped in a
/// ratatui-image stateful protocol, plus the path for the title bar.
pub struct ImageViewer {
    pub path: PathBuf,
    pub protocol: StatefulProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Search,
    Help,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => " NORMAL ",
            Mode::Insert => " INSERT ",
            Mode::Search => " SEARCH ",
            Mode::Help => " HELP ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    ChatList,
    Messages,
    Input,
}

impl Focus {
    pub fn cycle(self) -> Self {
        match self {
            Focus::ChatList => Focus::Messages,
            Focus::Messages => Focus::Input,
            Focus::Input => Focus::ChatList,
        }
    }
}

pub struct App {
    pub should_quit: bool,
    pub mode: Mode,
    pub focus: Focus,

    pub tg: TgHandle,
    pub chats: Vec<Chat>,
    pub chat_cursor: usize,

    pub messages: Vec<Message>,
    pub msg_cursor: usize,

    pub input: tui_textarea::TextArea<'static>,
    pub pending_key: Option<char>,

    pub fuzzy: Option<FuzzyPicker>,
    pub status: String,

    pub replying_to: Option<ReplyTarget>,
    pub editing: Option<EditTarget>,

    pub picker: Picker,
    pub image_viewer: Option<ImageViewer>,
    /// When a `v` press kicks off a fetch, we remember the (chat, msg) so
    /// the MediaFetched event can auto-open the viewer when it arrives.
    pub pending_view: Option<(String, u64)>,
    /// Video player argv resolved from config + env at startup.
    pub player_argv: Vec<String>,

    /// A scrollback fetch is in flight; suppresses duplicate requests.
    pub loading_older: bool,

    /// Per-chat input drafts. The value is the textarea's lines at the
    /// moment the user last left that chat. Entries are removed on send.
    pub drafts: HashMap<String, Vec<String>>,

    /// Chats where someone is currently typing, and the instant at which
    /// the indicator should auto-expire.
    pub typing_until: HashMap<String, Instant>,
    /// Last time we sent an outbound typing ping; used to debounce so we
    /// don't spam Telegram on every keystroke.
    pub last_typing_sent: Option<Instant>,
}

impl App {
    pub fn new(tg: TgHandle, picker: Picker, player_argv: Vec<String>) -> Self {
        let app = Self {
            should_quit: false,
            mode: Mode::Normal,
            focus: Focus::ChatList,
            tg,
            chats: vec![],
            chat_cursor: 0,
            messages: vec![],
            msg_cursor: 0,
            input: tui_textarea::TextArea::default(),
            pending_key: None,
            fuzzy: None,
            status: "Connecting…".into(),
            replying_to: None,
            editing: None,
            picker,
            image_viewer: None,
            pending_view: None,
            player_argv,
            loading_older: false,
            drafts: HashMap::new(),
            typing_until: HashMap::new(),
            last_typing_sent: None,
        };
        app.tg.send(Command::LoadChats);
        app
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        dispatch(self, key);
    }

    pub fn on_tick(&mut self) {
        // Expire stale typing indicators.
        let now = Instant::now();
        self.typing_until.retain(|_, until| *until > now);
    }

    /// Debounced outbound typing ping — dispatches at most once every 3s.
    /// Called from the insert-mode key handler after the textarea consumes
    /// a keystroke. No-op if we already flagged "typing" recently.
    pub fn maybe_send_typing(&mut self) {
        const MIN_INTERVAL: Duration = Duration::from_secs(3);
        let now = Instant::now();
        if let Some(last) = self.last_typing_sent {
            if now.duration_since(last) < MIN_INTERVAL {
                return;
            }
        }
        let Some(chat_id) = self.current_chat_id() else {
            return;
        };
        self.last_typing_sent = Some(now);
        self.tg.send(Command::SendTyping { chat_id });
    }

    // ---- Telegram events coming from the actor ----

    pub fn apply_event(&mut self, ev: TgEvent) {
        match ev {
            TgEvent::Connected => self.status = "Connected. Press ? for help".into(),
            TgEvent::Disconnected => self.status = "Disconnected".into(),
            TgEvent::Error(s) => self.status = format!("Error: {s}"),
            TgEvent::ChatsLoaded(chats) => {
                let prev = self.current_chat_id();
                self.chats = chats;
                if let Some(id) = prev {
                    if let Some(i) = self.chats.iter().position(|c| c.id == id) {
                        self.chat_cursor = i;
                    } else {
                        self.chat_cursor = 0;
                    }
                } else if !self.chats.is_empty() {
                    self.chat_cursor = 0;
                }
                if self.messages.is_empty() {
                    self.request_messages_for_current();
                }
            }
            TgEvent::MessagesLoaded { chat_id, messages } => {
                if self.current_chat_id().as_deref() == Some(chat_id.as_str()) {
                    self.messages = messages;
                    self.msg_cursor = self.messages.len().saturating_sub(1);
                }
            }
            TgEvent::OlderMessagesLoaded { chat_id, messages } => {
                self.on_older_messages_loaded(chat_id, messages);
            }
            TgEvent::MessageSent { .. } => {}
            TgEvent::NewMessage { chat_id, message } => self.on_new_message(chat_id, message),
            TgEvent::MessageEdited { chat_id, message } => {
                self.on_message_edited(chat_id, message)
            }
            TgEvent::MessageDeleted { chat_id, ids } => self.on_message_deleted(chat_id, ids),
            TgEvent::MediaFetched {
                chat_id,
                msg_id,
                path,
            } => self.on_media_fetched(chat_id, msg_id, path),
            TgEvent::Typing { chat_id, active } => {
                if active {
                    // Telegram sends typing pings every ~4s; 6s gives us
                    // headroom before expiring on the UI side.
                    self.typing_until
                        .insert(chat_id, Instant::now() + Duration::from_secs(6));
                } else {
                    self.typing_until.remove(&chat_id);
                }
            }
        }
    }

    fn on_media_fetched(&mut self, chat_id: String, msg_id: u64, path: PathBuf) {
        // Stamp the path onto the in-memory message so we don't re-download
        // next time the user views it.
        let mut kind: Option<MediaKind> = None;
        if self.current_chat_id().as_deref() == Some(chat_id.as_str()) {
            if let Some(msg) = self.messages.iter_mut().find(|m| m.id == msg_id) {
                msg.media_path = Some(path.clone());
                kind = msg.media_kind;
            }
        }
        // If the user is waiting on this exact fetch, open the right thing.
        if self.pending_view.as_ref() == Some(&(chat_id, msg_id)) {
            self.pending_view = None;
            match kind {
                Some(MediaKind::Photo) | Some(MediaKind::Sticker) => {
                    self.open_image_viewer(path);
                }
                Some(MediaKind::Video) => self.launch_video(path),
                _ => {}
            }
        }
    }

    fn open_image_viewer(&mut self, path: PathBuf) {
        match image::ImageReader::open(&path)
            .and_then(|r| Ok(r.with_guessed_format()?))
            .map_err(|e| e.to_string())
            .and_then(|r| r.decode().map_err(|e| e.to_string()))
        {
            Ok(dyn_img) => {
                let protocol = self.picker.new_resize_protocol(dyn_img);
                self.image_viewer = Some(ImageViewer { path, protocol });
                self.status = String::new();
            }
            Err(e) => {
                self.status = format!("decode failed: {e}");
            }
        }
    }

    pub fn close_image_viewer(&mut self) {
        self.image_viewer = None;
    }

    /// Handle the `v` key: view the selected message's media. Photos and
    /// stickers open in the inline image modal; videos hand off to an
    /// external player (configurable via `VT_PLAYER`, default `mpv`).
    pub fn view_selected(&mut self) {
        let Some(msg) = self.messages.get(self.msg_cursor).cloned() else {
            return;
        };
        let Some(kind) = msg.media_kind else {
            self.status = "No media on this message".into();
            return;
        };
        let is_viewable = matches!(
            kind,
            MediaKind::Photo | MediaKind::Sticker | MediaKind::Video
        );
        if !is_viewable {
            self.status = "Media type not viewable".into();
            return;
        }

        // Already downloaded — dispatch directly.
        if let Some(path) = msg.media_path.clone() {
            match kind {
                MediaKind::Photo | MediaKind::Sticker => self.open_image_viewer(path),
                MediaKind::Video => self.launch_video(path),
                _ => {}
            }
            return;
        }

        // Fetch first, auto-dispatch on arrival.
        let Some(chat_id) = self.current_chat_id() else {
            return;
        };
        self.pending_view = Some((chat_id.clone(), msg.id));
        self.tg.send(Command::FetchMedia {
            chat_id,
            msg_id: msg.id,
        });
        self.status = match kind {
            MediaKind::Video => "Downloading video…".into(),
            _ => "Downloading…".into(),
        };
    }

    /// Hand a downloaded video file to an external player. Argv is resolved
    /// from config + env at startup (see `Config::player_argv`). stdin/out/err
    /// are detached so the player can't corrupt the TUI's raw-mode terminal.
    fn launch_video(&mut self, path: PathBuf) {
        use std::process::{Command as PCommand, Stdio};
        let Some((program, args)) = self.player_argv.split_first() else {
            self.status = "player not configured".into();
            return;
        };
        let result = PCommand::new(program)
            .args(args)
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match result {
            Ok(_) => {
                self.status = format!("launched {program}");
            }
            Err(e) => {
                self.status =
                    format!("{program}: {e} (set VT_PLAYER or [media] player)");
            }
        }
    }

    /// Live update from the Telegram stream: append to the open chat,
    /// update the list preview, bump unread, and float the chat to the top.
    fn on_new_message(&mut self, chat_id: String, message: Message) {
        let is_current = self.current_chat_id().as_deref() == Some(chat_id.as_str());

        if is_current {
            self.messages.push(message.clone());
            self.msg_cursor = self.messages.len().saturating_sub(1);
        }

        let Some(idx) = self.chats.iter().position(|c| c.id == chat_id) else {
            // New chat we've never seen — ask the actor for a fresh dialog list.
            self.tg.send(crate::telegram::Command::LoadChats);
            return;
        };

        let preview = message.text.lines().next().unwrap_or("").to_string();
        self.chats[idx].last_preview = preview;
        if !is_current && !message.outgoing {
            self.chats[idx].unread += 1;
        }

        // Float to index 0 (Telegram-style ordering).
        if idx != 0 {
            let chat = self.chats.remove(idx);
            self.chats.insert(0, chat);
            if self.chat_cursor == idx {
                self.chat_cursor = 0;
            } else if self.chat_cursor < idx {
                self.chat_cursor += 1;
            }
        }
    }

    fn on_message_edited(&mut self, chat_id: String, message: Message) {
        let is_current = self.current_chat_id().as_deref() == Some(chat_id.as_str());
        if is_current {
            if let Some(slot) = self.messages.iter_mut().find(|m| m.id == message.id) {
                *slot = message.clone();
            }
        }
        // If the edited message is the most recent one for that chat, refresh
        // its preview. We only know for sure when it's the currently-open chat.
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            if is_current
                && self
                    .messages
                    .last()
                    .map(|m| m.id == message.id)
                    .unwrap_or(false)
            {
                chat.last_preview =
                    message.text.lines().next().unwrap_or("").to_string();
            }
        }
    }

    fn on_message_deleted(&mut self, chat_id: Option<String>, ids: Vec<u64>) {
        // Channel delete: only touch messages if it's the currently-open chat.
        // Non-channel delete (chat_id = None): scan the current chat by id.
        let applies_to_current = match &chat_id {
            Some(id) => self.current_chat_id().as_deref() == Some(id.as_str()),
            None => true,
        };
        if applies_to_current {
            self.messages.retain(|m| !ids.contains(&m.id));
            if self.msg_cursor >= self.messages.len() {
                self.msg_cursor = self.messages.len().saturating_sub(1);
            }
        }
        // Preview: if the deleted message happened to be the last visible one
        // in the open chat, refresh the preview from the new tail.
        if applies_to_current {
            if let Some(chat_id) = self.current_chat_id() {
                let new_preview = self
                    .messages
                    .last()
                    .map(|m| m.text.lines().next().unwrap_or("").to_string())
                    .unwrap_or_default();
                if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat_id) {
                    chat.last_preview = new_preview;
                }
            }
        }
    }

    fn current_chat_id(&self) -> Option<String> {
        self.chats.get(self.chat_cursor).map(|c| c.id.clone())
    }

    fn request_messages_for_current(&self) {
        if let Some(id) = self.current_chat_id() {
            self.tg.send(Command::LoadMessages(id));
        }
    }

    /// Kick off a scrollback fetch for the current chat, unless one is
    /// already in flight or the batch is empty.
    fn request_older_messages(&mut self) {
        if self.loading_older {
            return;
        }
        let Some(oldest) = self.messages.first() else {
            return;
        };
        let Some(chat_id) = self.current_chat_id() else {
            return;
        };
        self.loading_older = true;
        self.status = "Loading older messages…".into();
        self.tg.send(Command::LoadOlder {
            chat_id,
            before_id: oldest.id,
        });
    }

    fn on_older_messages_loaded(&mut self, chat_id: String, older: Vec<Message>) {
        self.loading_older = false;
        // If the user switched chats while we were fetching, ignore.
        if self.current_chat_id().as_deref() != Some(chat_id.as_str()) {
            return;
        }
        if older.is_empty() {
            self.status = "No more history".into();
            return;
        }
        let n = older.len();
        let mut combined = older;
        combined.extend(self.messages.drain(..));
        self.messages = combined;
        // Keep the user visually anchored to the previous top — their next
        // `k` press will scroll naturally into the freshly-loaded batch.
        self.msg_cursor += n;
        self.status = format!("+{n} older");
    }

    fn set_chat_cursor(&mut self, new: usize) {
        if new == self.chat_cursor {
            return;
        }

        // Save the current chat's input as a draft (or clear its entry if
        // the input is empty). Reply/edit targets are bound to the old
        // chat's message ids, so they must go too.
        if let Some(old_id) = self.current_chat_id() {
            let lines = self.input.lines().to_vec();
            let is_empty = lines.iter().all(|l| l.is_empty());
            if is_empty {
                self.drafts.remove(&old_id);
            } else {
                self.drafts.insert(old_id, lines);
            }
        }
        self.replying_to = None;
        self.editing = None;

        self.chat_cursor = new;
        if let Some(c) = self.chats.get_mut(new) {
            c.unread = 0;
        }
        self.messages.clear();
        self.msg_cursor = 0;
        self.loading_older = false;

        // Restore the new chat's draft into the textarea (or empty it).
        let new_lines = self
            .current_chat_id()
            .and_then(|id| self.drafts.get(&id).cloned())
            .unwrap_or_default();
        self.input = if new_lines.is_empty() {
            tui_textarea::TextArea::default()
        } else {
            let mut ta = tui_textarea::TextArea::new(new_lines);
            ta.move_cursor(tui_textarea::CursorMove::Bottom);
            ta.move_cursor(tui_textarea::CursorMove::End);
            ta
        };

        self.request_messages_for_current();
    }

    // ---- Actions invoked by the keymap ----

    pub fn quit(&mut self) {
        self.tg.send(Command::Shutdown);
        self.should_quit = true;
    }

    pub fn cycle_focus(&mut self) {
        self.focus = self.focus.cycle();
        if self.focus == Focus::Input {
            self.mode = Mode::Insert;
        } else {
            self.mode = Mode::Normal;
        }
    }

    pub fn enter_insert(&mut self) {
        self.focus = Focus::Input;
        self.mode = Mode::Insert;
    }

    pub fn enter_normal(&mut self) {
        self.mode = Mode::Normal;
        if self.focus == Focus::Input {
            self.focus = Focus::Messages;
        }
    }

    pub fn toggle_help(&mut self) {
        self.mode = if self.mode == Mode::Help {
            Mode::Normal
        } else {
            Mode::Help
        };
    }

    pub fn open_fuzzy(&mut self, target: FuzzyTarget) {
        let items: Vec<String> = match target {
            FuzzyTarget::ChatList | FuzzyTarget::GlobalSwitcher => self
                .chats
                .iter()
                .map(|c| format!("{}  —  {}", c.name, c.last_preview))
                .collect(),
            FuzzyTarget::Messages => self
                .messages
                .iter()
                .map(|m| format!("{}: {}", m.sender, m.text))
                .collect(),
        };
        self.fuzzy = Some(FuzzyPicker::new(target, items));
        self.mode = Mode::Search;
    }

    pub fn close_fuzzy(&mut self) {
        self.fuzzy = None;
        self.mode = Mode::Normal;
    }

    pub fn confirm_fuzzy(&mut self) {
        let Some(picker) = self.fuzzy.as_ref() else {
            return;
        };
        if let Some(idx) = picker.selected_source_index() {
            match picker.target {
                FuzzyTarget::ChatList | FuzzyTarget::GlobalSwitcher => {
                    self.set_chat_cursor(idx);
                    self.focus = Focus::Messages;
                }
                FuzzyTarget::Messages => {
                    self.msg_cursor = idx;
                }
            }
        }
        self.close_fuzzy();
    }

    // ---- Movement helpers used by the keymap ----

    pub fn move_down(&mut self, n: usize) {
        match self.focus {
            Focus::ChatList => {
                let new =
                    (self.chat_cursor + n).min(self.chats.len().saturating_sub(1));
                self.set_chat_cursor(new);
            }
            Focus::Messages => {
                self.msg_cursor =
                    (self.msg_cursor + n).min(self.messages.len().saturating_sub(1));
            }
            Focus::Input => {}
        }
    }

    pub fn move_up(&mut self, n: usize) {
        match self.focus {
            Focus::ChatList => {
                let new = self.chat_cursor.saturating_sub(n);
                self.set_chat_cursor(new);
            }
            Focus::Messages => {
                // At the top of the loaded batch: absorb the keystroke by
                // requesting more history. The next `k` press will scroll
                // into the newly-prepended messages.
                if self.msg_cursor == 0 {
                    self.request_older_messages();
                } else {
                    self.msg_cursor = self.msg_cursor.saturating_sub(n);
                }
            }
            Focus::Input => {}
        }
    }

    pub fn go_top(&mut self) {
        match self.focus {
            Focus::ChatList => self.set_chat_cursor(0),
            Focus::Messages => {
                if self.msg_cursor == 0 {
                    // Already at the top — treat `gg` as "load more older".
                    self.request_older_messages();
                } else {
                    self.msg_cursor = 0;
                }
            }
            Focus::Input => {}
        }
    }

    pub fn go_bottom(&mut self) {
        match self.focus {
            Focus::ChatList => {
                let last = self.chats.len().saturating_sub(1);
                self.set_chat_cursor(last);
            }
            Focus::Messages => self.msg_cursor = self.messages.len().saturating_sub(1),
            Focus::Input => {}
        }
    }

    pub fn send_input(&mut self) {
        let text = self.input.lines().join("\n");
        if text.trim().is_empty() {
            return;
        }
        let Some(id) = self.current_chat_id() else {
            return;
        };

        if let Some(edit) = self.editing.take() {
            self.tg.send(Command::Edit {
                chat_id: id.clone(),
                msg_id: edit.msg_id,
                text,
            });
        } else {
            let reply_to = self.replying_to.as_ref().map(|r| r.msg_id);
            self.tg.send(Command::Send {
                chat_id: id.clone(),
                text,
                reply_to,
            });
            self.replying_to = None;
        }
        self.input = tui_textarea::TextArea::default();
        // The draft (if any) has been committed to Telegram, so drop it.
        self.drafts.remove(&id);
    }

    /// Toggle a reply against the currently-selected message. Pressing `r`
    /// again while already replying cancels the reply. Starting a reply
    /// cancels any in-progress edit (mutually exclusive compose states).
    pub fn toggle_reply(&mut self) {
        if self.replying_to.is_some() {
            self.replying_to = None;
            return;
        }
        let Some(msg) = self.messages.get(self.msg_cursor) else {
            return;
        };
        let preview: String = msg
            .text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        self.replying_to = Some(ReplyTarget {
            msg_id: msg.id,
            sender: msg.sender.clone(),
            preview,
        });
        // If we were in the middle of editing, drop it — can't do both.
        if self.editing.take().is_some() {
            self.input = tui_textarea::TextArea::default();
        }
        self.focus = Focus::Input;
        self.mode = Mode::Insert;
    }

    /// Delete the currently-selected message. Optimistically removes it
    /// from the local view; if the server rejects (permission), a later
    /// `LoadMessages` or stream update will repair the state.
    pub fn delete_selected(&mut self) {
        let Some(msg) = self.messages.get(self.msg_cursor).cloned() else {
            return;
        };
        let Some(chat_id) = self.current_chat_id() else {
            return;
        };
        self.tg.send(Command::Delete {
            chat_id,
            ids: vec![msg.id],
        });
        self.status = "Deleted".into();
    }

    /// Jump the message cursor to the replied-to message of the current
    /// selection, if that target is within the currently-loaded history
    /// window. Sets a status line otherwise.
    pub fn jump_to_reply_target(&mut self) {
        let Some(current) = self.messages.get(self.msg_cursor) else {
            return;
        };
        let Some(rid) = current.reply_to_id else {
            self.status = "Not a reply".into();
            return;
        };
        match self.messages.iter().position(|m| m.id == rid) {
            Some(idx) => {
                self.msg_cursor = idx;
                self.status = "→ reply target".into();
            }
            None => {
                self.status = "Reply target is older than loaded history".into();
            }
        }
    }

    /// Toggle an edit of the currently-selected message. Only valid for
    /// outgoing messages. Pressing `e` again cancels and clears the input.
    pub fn toggle_edit(&mut self) {
        if self.editing.is_some() {
            self.editing = None;
            self.input = tui_textarea::TextArea::default();
            return;
        }
        let Some(msg) = self.messages.get(self.msg_cursor) else {
            return;
        };
        if !msg.outgoing {
            self.status = "Can only edit your own messages".into();
            return;
        }
        self.editing = Some(EditTarget { msg_id: msg.id });
        // Cancel any pending reply — mutually exclusive.
        self.replying_to = None;

        // Pre-fill the input with the current message body, cursor at end.
        let lines: Vec<String> = msg.text.split('\n').map(String::from).collect();
        let mut ta = tui_textarea::TextArea::new(lines);
        ta.move_cursor(tui_textarea::CursorMove::Bottom);
        ta.move_cursor(tui_textarea::CursorMove::End);
        self.input = ta;

        self.focus = Focus::Input;
        self.mode = Mode::Insert;
    }
}

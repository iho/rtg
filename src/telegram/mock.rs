//! In-memory mock actor — lets the UI run fully without network access.

use std::collections::HashMap;
use tokio::sync::mpsc::{self, UnboundedSender};

use super::{Chat, Command, Message, TgEvent, TgHandle};

struct MockState {
    chats: Vec<Chat>,
    msgs: HashMap<String, Vec<Message>>,
    next_id: u64,
}

impl MockState {
    fn seed() -> Self {
        let sample_chats = [
            ("1", "Alice", Some("alice"), "see you at 5"),
            ("2", "Bob", Some("bobby"), "lgtm 🚀"),
            ("3", "Rust Lang", None, "new 1.85 released"),
            ("4", "Helix Editors", None, "modal editing is life"),
            ("5", "Ghostty Users", None, "kitty graphics working"),
            ("6", "Family", None, "dinner tomorrow?"),
            ("7", "Mom", Some("mom"), "call me when free"),
            ("8", "Work Ops", None, "deploy postponed"),
            ("9", "Neovim", None, "lua plugin ecosystem"),
            ("10", "Saved Messages", None, "notes to self"),
        ];

        let mut chats = Vec::new();
        let mut msgs: HashMap<String, Vec<Message>> = HashMap::new();

        for (id, name, user, preview) in sample_chats {
            chats.push(Chat {
                id: id.to_string(),
                name: name.to_string(),
                username: user.map(String::from),
                last_preview: preview.to_string(),
                unread: 0,
            });
            let base = chrono::Local::now();
            let conv = (0..25)
                .map(|i| Message {
                    id: i,
                    sender: if i % 3 == 0 {
                        "me".into()
                    } else {
                        name.to_string()
                    },
                    text: format!("{} — message {}", preview, i),
                    timestamp: base - chrono::Duration::minutes(25 - i as i64),
                    outgoing: i % 3 == 0,
                    // Every 5th message quotes the previous one, so the mock
                    // actually shows the reply marker.
                    reply_to_id: if i > 0 && i % 5 == 0 { Some(i - 1) } else { None },
                    edited: i % 7 == 0,
                    media_kind: None,
                    media_path: None,
                })
                .collect();
            msgs.insert(id.to_string(), conv);
        }

        Self {
            chats,
            msgs,
            next_id: 1_000,
        }
    }

    fn insert(&mut self, chat_id: &str, text: &str) {
        self.next_id += 1;
        let m = Message {
            id: self.next_id,
            sender: "me".into(),
            text: text.to_string(),
            timestamp: chrono::Local::now(),
            outgoing: true,
            reply_to_id: None,
            edited: false,
            media_kind: None,
            media_path: None,
        };
        self.msgs.entry(chat_id.to_string()).or_default().push(m);
        if let Some(c) = self.chats.iter_mut().find(|c| c.id == chat_id) {
            c.last_preview = text.lines().next().unwrap_or("").to_string();
        }
    }
}

pub fn spawn(events: UnboundedSender<TgEvent>) -> TgHandle {
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();

    tokio::spawn(async move {
        let mut state = MockState::seed();
        let _ = events.send(TgEvent::Connected);

        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Command::LoadChats => {
                    let _ = events.send(TgEvent::ChatsLoaded(state.chats.clone()));
                }
                Command::LoadOlder { chat_id, before_id: _ } => {
                    // Mock has a fixed history depth — always "no more".
                    let _ = events.send(TgEvent::OlderMessagesLoaded {
                        chat_id,
                        messages: vec![],
                    });
                }
                Command::LoadMessages(id) => {
                    let messages = state.msgs.get(&id).cloned().unwrap_or_default();
                    let _ = events.send(TgEvent::MessagesLoaded {
                        chat_id: id,
                        messages,
                    });
                }
                Command::Send { chat_id, text, reply_to: _ } => {
                    state.insert(&chat_id, &text);
                    let messages = state.msgs.get(&chat_id).cloned().unwrap_or_default();
                    let _ = events.send(TgEvent::MessagesLoaded {
                        chat_id: chat_id.clone(),
                        messages,
                    });
                    let _ = events.send(TgEvent::ChatsLoaded(state.chats.clone()));
                    let _ = events.send(TgEvent::MessageSent { chat_id });
                }
                Command::FetchMedia { .. } => {
                    let _ = events.send(TgEvent::Error(
                        "mock backend has no media to fetch".into(),
                    ));
                }
                Command::SendTyping { .. } => {
                    // Mock has no real presence channel; drop silently.
                }
                Command::Delete { chat_id, ids } => {
                    if let Some(list) = state.msgs.get_mut(&chat_id) {
                        list.retain(|m| !ids.contains(&m.id));
                    }
                    let messages = state.msgs.get(&chat_id).cloned().unwrap_or_default();
                    let _ = events.send(TgEvent::MessagesLoaded {
                        chat_id: chat_id.clone(),
                        messages,
                    });
                    let _ = events.send(TgEvent::ChatsLoaded(state.chats.clone()));
                }
                Command::Edit { chat_id, msg_id, text } => {
                    if let Some(list) = state.msgs.get_mut(&chat_id) {
                        if let Some(m) = list.iter_mut().find(|m| m.id == msg_id) {
                            m.text = text.clone();
                        }
                    }
                    let messages = state.msgs.get(&chat_id).cloned().unwrap_or_default();
                    let _ = events.send(TgEvent::MessagesLoaded {
                        chat_id,
                        messages,
                    });
                }
                Command::Shutdown => break,
            }
        }
    });

    TgHandle { cmd: cmd_tx }
}

//! Real Telegram backend via `grammers-client`.
//!
//! Runs as an actor: spawned from `main`, it owns the [`Client`], listens for
//! [`Command`]s on an mpsc, and pushes [`TgEvent`]s back. The draw loop never
//! touches the network.
//!
//! Login runs *before* the TUI enters raw mode so phone/code/2FA prompts
//! work over plain stdin/stdout.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use grammers_client::{
    session::{PackedChat, PackedType, Session},
    types::{Downloadable, Media, Message as GMessage},
    Client as GClient, Config as GConfig, InitParams, InputMessage, SignInError, Update,
};
use grammers_tl_types as tl;

use super::MediaKind;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedSender};

use super::{Chat, Command, Message, TgEvent, TgHandle};
use crate::config::Config;

/// Connect (logging in if necessary) and spawn the actor. MUST be called
/// before `enable_raw_mode`, because the login flow prompts on stdin.
pub async fn spawn(config: &Config, events: UnboundedSender<TgEvent>) -> Result<TgHandle> {
    let paths = Config::paths();
    std::fs::create_dir_all(&paths.config_dir).ok();
    std::fs::create_dir_all(&paths.data_dir).ok();

    let api_id = config.api_id.context("api_id missing from config")?;
    let api_hash = config
        .api_hash
        .clone()
        .context("api_hash missing from config")?;

    let session = Session::load_file_or_create(&paths.session_file)
        .with_context(|| format!("loading session file {:?}", paths.session_file))?;

    println!("Connecting to Telegram…");
    let client = GClient::connect(GConfig {
        session,
        api_id,
        api_hash: api_hash.clone(),
        params: InitParams {
            catch_up: true,
            ..InitParams::default()
        },
    })
    .await
    .context("connecting to Telegram")?;

    if !client.is_authorized().await? {
        login_interactive(&client).await?;
        client
            .session()
            .save_to_file(&paths.session_file)
            .context("saving session file")?;
        println!("Signed in. Session saved to {:?}", paths.session_file);
    }

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let session_path = paths.session_file.clone();
    tokio::spawn(actor_loop(client, session_path, cmd_rx, events));
    Ok(TgHandle { cmd: cmd_tx })
}

async fn login_interactive(client: &GClient) -> Result<()> {
    let phone = prompt("Phone (international, e.g. +15551234567): ").await?;
    let token = client
        .request_login_code(phone.trim())
        .await
        .context("requesting login code")?;

    let code = prompt("Login code: ").await?;
    match client.sign_in(&token, code.trim()).await {
        Ok(_) => Ok(()),
        Err(SignInError::PasswordRequired(pwd_token)) => {
            let hint = pwd_token.hint().map(str::to_owned).unwrap_or_default();
            let prompt_text = if hint.is_empty() {
                "2FA password: ".to_string()
            } else {
                format!("2FA password (hint: {hint}): ")
            };
            let password = tokio::task::spawn_blocking(move || rpassword::prompt_password(prompt_text))
                .await
                .context("spawning password prompt")?
                .context("reading password")?;
            client
                .check_password(pwd_token, password)
                .await
                .map(|_| ())
                .map_err(|e| anyhow::anyhow!("2FA failed: {e}"))
        }
        Err(SignInError::InvalidCode) => bail!("invalid login code"),
        Err(SignInError::SignUpRequired { .. }) => {
            bail!("this phone number has no Telegram account (sign-up not supported)")
        }
        Err(e) => bail!("sign-in failed: {e}"),
    }
}

async fn prompt(label: &str) -> Result<String> {
    use std::io::Write;
    print!("{label}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await
        .context("reading stdin")?;
    Ok(line)
}

async fn actor_loop(
    client: GClient,
    session_path: std::path::PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    events: UnboundedSender<TgEvent>,
) {
    let mut packed: HashMap<String, PackedChat> = HashMap::new();
    // Cache of grammers Media handles, keyed by (chat_id, msg_id), so we
    // can download on demand without re-fetching the message itself.
    let mut media_cache: HashMap<(String, u64), Media> = HashMap::new();
    let _ = events.send(TgEvent::Connected);

    // Spawn a dedicated task for the update stream. `next_update()` isn't
    // documented as cancel-safe, so we never put it inside a `select!`.
    // Instead we forward Updates through an unbounded mpsc the actor owns.
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<Update>();
    {
        let client = client.clone();
        let events = events.clone();
        tokio::spawn(async move {
            loop {
                match client.next_update().await {
                    Ok(update) => {
                        if up_tx.send(update).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = events
                            .send(TgEvent::Error(format!("update stream: {e}")));
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        });
    }

    loop {
        tokio::select! {
            biased;
            maybe_cmd = cmd_rx.recv() => {
                let Some(cmd) = maybe_cmd else { break };
                if handle_command(&client, &events, &mut packed, &mut media_cache, cmd).await {
                    break;
                }
            }
            Some(update) = up_rx.recv() => {
                handle_update(&events, &mut packed, &mut media_cache, update);
            }
        }
    }

    let _ = client.session().save_to_file(&session_path);
    let _ = events.send(TgEvent::Disconnected);
}

/// Returns `true` if the actor should stop (Shutdown received).
async fn handle_command(
    client: &GClient,
    events: &UnboundedSender<TgEvent>,
    packed: &mut HashMap<String, PackedChat>,
    media_cache: &mut HashMap<(String, u64), Media>,
    cmd: Command,
) -> bool {
    match cmd {
        Command::LoadChats => match load_chats(client).await {
            Ok((chats, pairs)) => {
                packed.clear();
                for (id, pc) in pairs {
                    packed.insert(id, pc);
                }
                let _ = events.send(TgEvent::ChatsLoaded(chats));
            }
            Err(e) => {
                let _ = events.send(TgEvent::Error(format!("load chats: {e}")));
            }
        },
        Command::LoadOlder { chat_id, before_id } => {
            let Some(pc) = packed.get(&chat_id).copied() else {
                let _ = events.send(TgEvent::Error(format!("unknown chat id {chat_id}")));
                return false;
            };
            match load_older(client, &chat_id, pc, before_id, media_cache).await {
                Ok(messages) => {
                    let _ = events.send(TgEvent::OlderMessagesLoaded { chat_id, messages });
                }
                Err(e) => {
                    let _ = events.send(TgEvent::Error(format!("load older: {e}")));
                }
            }
        }
        Command::LoadMessages(chat_id) => {
            let Some(pc) = packed.get(&chat_id).copied() else {
                let _ = events.send(TgEvent::Error(format!("unknown chat id {chat_id}")));
                return false;
            };
            match load_messages(client, &chat_id, pc, media_cache).await {
                Ok(messages) => {
                    let _ = events.send(TgEvent::MessagesLoaded { chat_id, messages });
                }
                Err(e) => {
                    let _ = events.send(TgEvent::Error(format!("load messages: {e}")));
                }
            }
        }
        Command::Send {
            chat_id,
            text,
            reply_to,
        } => {
            let Some(pc) = packed.get(&chat_id).copied() else {
                let _ = events.send(TgEvent::Error(format!("unknown chat id {chat_id}")));
                return false;
            };
            let input = InputMessage::text(text.as_str())
                .reply_to(reply_to.map(|id| id as i32));
            match client.send_message(pc, input).await {
                Ok(_) => {
                    // No explicit reload — the update stream will deliver our
                    // own outgoing message, and the App appends it on NewMessage.
                    let _ = events.send(TgEvent::MessageSent { chat_id });
                }
                Err(e) => {
                    let _ = events.send(TgEvent::Error(format!("send failed: {e}")));
                }
            }
        }
        Command::SendTyping { chat_id } => {
            let Some(pc) = packed.get(&chat_id).copied() else {
                return false;
            };
            // Fire-and-forget: a failed typing ping shouldn't surface as an
            // error to the user, and shouldn't block other commands.
            if let Err(e) = client
                .action(pc)
                .oneshot(tl::enums::SendMessageAction::SendMessageTypingAction)
                .await
            {
                tracing::debug!("typing ping failed: {e}");
            }
        }
        Command::Delete { chat_id, ids } => {
            let Some(pc) = packed.get(&chat_id).copied() else {
                let _ = events.send(TgEvent::Error(format!("unknown chat id {chat_id}")));
                return false;
            };
            let i32_ids: Vec<i32> = ids.iter().map(|&i| i as i32).collect();
            if let Err(e) = client.delete_messages(pc, &i32_ids).await {
                let _ = events.send(TgEvent::Error(format!("delete failed: {e}")));
            }
            // The live update stream will deliver Update::MessageDeleted and
            // our existing on_message_deleted handler applies it.
        }
        Command::FetchMedia { chat_id, msg_id } => {
            let Some(media) = media_cache.get(&(chat_id.clone(), msg_id)).cloned() else {
                let _ = events.send(TgEvent::Error(format!(
                    "no media cached for message {msg_id} in chat {chat_id}"
                )));
                return false;
            };
            // Spawn a sub-task so the actor keeps servicing other commands
            // (typing pings, sends, scrollback) while big files download.
            // Client is internally Arc-based, so clone is cheap.
            let client = client.clone();
            let events = events.clone();
            tokio::spawn(async move {
                match download_media(&client, &chat_id, msg_id, &media).await {
                    Ok(path) => {
                        let _ = events.send(TgEvent::MediaFetched {
                            chat_id,
                            msg_id,
                            path,
                        });
                    }
                    Err(e) => {
                        let _ = events.send(TgEvent::Error(format!("download failed: {e}")));
                    }
                }
            });
        }
        Command::Edit {
            chat_id,
            msg_id,
            text,
        } => {
            let Some(pc) = packed.get(&chat_id).copied() else {
                let _ = events.send(TgEvent::Error(format!("unknown chat id {chat_id}")));
                return false;
            };
            let input = InputMessage::text(text.as_str());
            if let Err(e) = client.edit_message(pc, msg_id as i32, input).await {
                let _ = events.send(TgEvent::Error(format!("edit failed: {e}")));
            }
            // The live update stream will deliver Update::MessageEdited, which
            // the App turns into an in-place refresh — no reload needed.
        }
        Command::Shutdown => return true,
    }
    false
}

fn handle_update(
    events: &UnboundedSender<TgEvent>,
    packed: &mut HashMap<String, PackedChat>,
    media_cache: &mut HashMap<(String, u64), Media>,
    update: Update,
) {
    match update {
        Update::NewMessage(msg) => {
            let pc = msg.chat().pack();
            let chat_id = pc.to_hex();
            packed.entry(chat_id.clone()).or_insert(pc);
            if let Some(media) = msg.media() {
                media_cache.insert((chat_id.clone(), msg.id() as u64), media);
            }
            let message = convert_message(&msg);
            let _ = events.send(TgEvent::NewMessage { chat_id, message });
        }
        Update::MessageEdited(msg) => {
            let pc = msg.chat().pack();
            let chat_id = pc.to_hex();
            packed.entry(chat_id.clone()).or_insert(pc);
            if let Some(media) = msg.media() {
                media_cache.insert((chat_id.clone(), msg.id() as u64), media);
            }
            let message = convert_message(&msg);
            let _ = events.send(TgEvent::MessageEdited { chat_id, message });
        }
        Update::MessageDeleted(deletion) => {
            let ids: Vec<u64> = deletion.messages().iter().map(|&i| i as u64).collect();
            let chat_id = deletion
                .channel_id()
                .and_then(|cid| resolve_channel(packed, cid));
            let _ = events.send(TgEvent::MessageDeleted { chat_id, ids });
        }
        Update::Raw(raw) => handle_raw_update(events, packed, raw),
        // Callback queries, inline queries — ignored.
        _ => {}
    }
}

/// Raw updates grammers doesn't surface as friendly variants. We only care
/// about typing pings here; everything else is dropped.
fn handle_raw_update(
    events: &UnboundedSender<TgEvent>,
    packed: &HashMap<String, PackedChat>,
    raw: tl::enums::Update,
) {
    use tl::enums::Update as U;
    let (peer_id, peer_ty, action) = match raw {
        U::UserTyping(u) => (u.user_id, TypingPeer::User, u.action),
        U::ChatUserTyping(u) => (u.chat_id, TypingPeer::BasicGroup, u.action),
        U::ChannelUserTyping(u) => (u.channel_id, TypingPeer::Channel, u.action),
        _ => return,
    };
    let Some(chat_id) = resolve_typing_chat(packed, peer_id, peer_ty) else {
        return;
    };
    let active = !matches!(action, tl::enums::SendMessageAction::SendMessageCancelAction);
    let _ = events.send(TgEvent::Typing { chat_id, active });
}

#[derive(Clone, Copy)]
enum TypingPeer {
    User,
    BasicGroup,
    Channel,
}

fn resolve_typing_chat(
    packed: &HashMap<String, PackedChat>,
    id: i64,
    ty: TypingPeer,
) -> Option<String> {
    for (hex, pc) in packed.iter() {
        let matches = match ty {
            TypingPeer::User => matches!(pc.ty, PackedType::User | PackedType::Bot) && pc.id == id,
            TypingPeer::BasicGroup => matches!(pc.ty, PackedType::Chat) && pc.id == id,
            TypingPeer::Channel => matches!(
                pc.ty,
                PackedType::Broadcast | PackedType::Megagroup | PackedType::Gigagroup
            ) && pc.id == id,
        };
        if matches {
            return Some(hex.clone());
        }
    }
    None
}

/// Find the packed-hex chat id for a raw Telegram channel id, by scanning
/// the actor's packed map for a channel/megagroup/broadcast entry whose
/// underlying id matches. Returns `None` if the channel isn't in our dialog
/// list yet — the caller emits `chat_id: None` and the App handles the
/// fallback by searching the current chat.
fn resolve_channel(packed: &HashMap<String, PackedChat>, channel_id: i64) -> Option<String> {
    use grammers_client::session::PackedType;
    for (hex, pc) in packed.iter() {
        let is_channel = matches!(
            pc.ty,
            PackedType::Broadcast | PackedType::Megagroup | PackedType::Gigagroup
        );
        if is_channel && pc.id == channel_id {
            return Some(hex.clone());
        }
    }
    None
}

fn convert_message(msg: &GMessage) -> Message {
    let sender_name = msg
        .sender()
        .map(|s| s.name().to_string())
        .unwrap_or_else(|| "?".into());
    let ts = msg.date().with_timezone(&chrono::Local);
    let media_kind = msg.media().as_ref().map(media_kind_of);
    Message {
        id: msg.id() as u64,
        sender: if msg.outgoing() {
            "me".into()
        } else {
            sender_name
        },
        text: msg.text().to_string(),
        timestamp: ts,
        outgoing: msg.outgoing(),
        reply_to_id: msg.reply_to_message_id().map(|i| i as u64),
        edited: msg.edit_date().is_some(),
        media_kind,
        media_path: None,
    }
}

fn media_kind_of(m: &Media) -> MediaKind {
    match m {
        Media::Photo(_) => MediaKind::Photo,
        Media::Sticker(_) => MediaKind::Sticker,
        // Telegram carries videos as Documents. Classify by mime prefix so
        // `video/mp4`, `video/quicktime`, etc. all register as Video.
        Media::Document(d) => match d.mime_type() {
            Some(m) if m.starts_with("video/") => MediaKind::Video,
            _ => MediaKind::Document,
        },
        _ => MediaKind::Other,
    }
}

/// Returns `(our_chats, id → packed_chat)`. The string id is the packed hex
/// representation so we can round-trip through `Chat::id` into grammers calls.
async fn load_chats(client: &GClient) -> Result<(Vec<Chat>, Vec<(String, PackedChat)>)> {
    let mut iter = client.iter_dialogs().limit(200);
    let mut chats = Vec::new();
    let mut packed = Vec::new();

    while let Some(dialog) = iter.next().await? {
        let g_chat = dialog.chat();
        let pc = g_chat.pack();
        let id = pc.to_hex();

        let preview = dialog
            .last_message
            .as_ref()
            .map(|m| m.text().to_string())
            .unwrap_or_default();

        let username = match g_chat {
            grammers_client::types::Chat::User(u) => u.username().map(String::from),
            grammers_client::types::Chat::Group(g) => g.username().map(String::from),
            grammers_client::types::Chat::Channel(c) => c.username().map(String::from),
        };

        chats.push(Chat {
            id: id.clone(),
            name: g_chat.name().to_string(),
            username,
            last_preview: preview,
            unread: 0,
        });
        packed.push((id, pc));
    }
    Ok((chats, packed))
}

async fn load_messages(
    client: &GClient,
    chat_id: &str,
    peer: PackedChat,
    media_cache: &mut HashMap<(String, u64), Media>,
) -> Result<Vec<Message>> {
    let mut iter = client.iter_messages(peer).limit(50);
    let mut out: Vec<Message> = Vec::new();
    while let Some(msg) = iter.next().await? {
        if let Some(media) = msg.media() {
            media_cache.insert((chat_id.to_string(), msg.id() as u64), media);
        }
        out.push(convert_message(&msg));
    }
    // grammers yields newest-first; flip so the UI scrolls chronologically.
    out.reverse();
    Ok(out)
}

async fn load_older(
    client: &GClient,
    chat_id: &str,
    peer: PackedChat,
    before_id: u64,
    media_cache: &mut HashMap<(String, u64), Media>,
) -> Result<Vec<Message>> {
    let mut iter = client
        .iter_messages(peer)
        .offset_id(before_id as i32)
        .limit(50);
    let mut out: Vec<Message> = Vec::new();
    while let Some(msg) = iter.next().await? {
        if let Some(media) = msg.media() {
            media_cache.insert((chat_id.to_string(), msg.id() as u64), media);
        }
        out.push(convert_message(&msg));
    }
    // grammers yields newest-first; reverse so caller can prepend a chunk
    // that is itself chronologically ordered.
    out.reverse();
    Ok(out)
}

/// Download the given media to a stable path under the cache dir.
async fn download_media(
    client: &GClient,
    chat_id: &str,
    msg_id: u64,
    media: &Media,
) -> Result<std::path::PathBuf> {
    let cache_dir = directories::ProjectDirs::from("dev", "vimtelegam", "vim-telegam")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("vim-telegam"))
        .join("media");
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("creating media cache dir {cache_dir:?}"))?;

    let path = cache_dir.join(format!("{chat_id}_{msg_id}"));
    if !path.exists() {
        let downloadable = Downloadable::Media(media.clone());
        client
            .download_media(&downloadable, &path)
            .await
            .with_context(|| format!("downloading media for msg {msg_id}"))?;
    }
    Ok(path)
}

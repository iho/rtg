//! Top-level UI composition: layout + per-pane draw calls.

pub mod modals;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, Focus, Mode};

pub fn draw(f: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(1)])
        .split(root[0]);

    let has_compose_banner = app.replying_to.is_some() || app.editing.is_some();
    let right_constraints: Vec<Constraint> = if has_compose_banner {
        vec![
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(5),
        ]
    } else {
        vec![Constraint::Min(1), Constraint::Length(5)]
    };
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints(right_constraints)
        .split(body[1]);

    draw_chat_list(f, app, body[0]);
    draw_messages(f, app, right[0]);
    if has_compose_banner {
        draw_compose_banner(f, app, right[1]);
        draw_input(f, app, right[2]);
    } else {
        draw_input(f, app, right[1]);
    }
    draw_status(f, app, root[1]);

    // Modal overlays draw last.
    if app.mode == Mode::Help {
        modals::help::draw(f);
    }
    if app.fuzzy.is_some() {
        modals::fuzzy::draw(f, app);
    }
    if app.image_viewer.is_some() {
        modals::image_viewer::draw(f, app);
    }
}

fn pane_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_chat_list(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::ChatList;
    let now = std::time::Instant::now();
    let items: Vec<ListItem> = app
        .chats
        .iter()
        .map(|c| {
            let unread = if c.unread > 0 {
                format!(" ({})", c.unread)
            } else {
                "".into()
            };
            let title = Line::from(vec![
                Span::styled(
                    c.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(unread, Style::default().fg(Color::Yellow)),
            ]);

            // If someone's typing in this chat, replace the preview line.
            let is_typing = app
                .typing_until
                .get(&c.id)
                .map(|t| *t > now)
                .unwrap_or(false);
            let preview = if is_typing {
                Line::from(Span::styled(
                    "typing…",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::ITALIC),
                ))
            } else {
                Line::from(Span::styled(
                    c.last_preview.clone(),
                    Style::default().fg(Color::DarkGray),
                ))
            };
            ListItem::new(vec![title, preview])
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.chat_cursor));

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_style(focused))
                .title(" Chats "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");

    f.render_stateful_widget(list, area, &mut state);
}

fn draw_messages(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Messages;
    let title = match app.chats.get(app.chat_cursor) {
        Some(c) => {
            let is_typing = app
                .typing_until
                .get(&c.id)
                .map(|t| *t > std::time::Instant::now())
                .unwrap_or(false);
            if is_typing {
                format!(" {}  · typing… ", c.name)
            } else {
                format!(" {} ", c.name)
            }
        }
        None => " — ".into(),
    };

    // Inner width = area.width minus borders. Clamp to at least 1 so we
    // never feed zero into the wrapper.
    let wrap_width = (area.width as usize).saturating_sub(2).max(1);

    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|m| {
            let sender_style = if m.outgoing {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
            };
            let mut header_spans = vec![
                Span::styled(format!("{} ", m.sender), sender_style),
                Span::styled(
                    m.timestamp.format("%H:%M").to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if m.edited {
                header_spans.push(Span::styled(
                    " (edited)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            let header = Line::from(header_spans);

            let mut lines: Vec<Line> = Vec::with_capacity(3);
            lines.push(header);

            // If this message is a reply, show a one-line stub quoting the
            // target. We look it up in the currently-loaded batch; if it's
            // not there (older than our window) we fall back to a plain
            // "↳ reply" marker.
            if let Some(rid) = m.reply_to_id {
                let stub = match app.messages.iter().find(|t| t.id == rid) {
                    Some(target) => {
                        let preview: String = target
                            .text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(wrap_width.saturating_sub(target.sender.len() + 4))
                            .collect();
                        Line::from(vec![
                            Span::styled(
                                "↳ ",
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::styled(
                                format!("{}: ", target.sender),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(preview, Style::default().fg(Color::DarkGray)),
                        ])
                    }
                    None => Line::from(vec![
                        Span::styled("↳ ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            "reply",
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                        ),
                    ]),
                };
                lines.push(stub);
            }

            for wrapped in wrap_text(&m.text, wrap_width) {
                lines.push(Line::from(wrapped));
            }

            // Media badge: show a pill for attached media. Caption (if any)
            // is already in the wrapped body above.
            if let Some(kind) = m.media_kind {
                use crate::telegram::MediaKind;
                let label = match kind {
                    MediaKind::Photo => "[📷 photo  — press v]",
                    MediaKind::Video => "[🎬 video  — press v]",
                    MediaKind::Document => "[📄 document]",
                    MediaKind::Sticker => "[🪄 sticker  — press v]",
                    MediaKind::Other => "[media]",
                };
                lines.push(Line::from(Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
            }

            lines.push(Line::from(""));
            ListItem::new(lines)
        })
        .collect();

    let mut state = ListState::default();
    if !app.messages.is_empty() {
        state.select(Some(app.msg_cursor));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_style(focused))
                .title(title),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));

    f.render_stateful_widget(list, area, &mut state);
}

fn draw_compose_banner(f: &mut Frame, app: &App, area: Rect) {
    // Edit takes precedence when both would somehow be set; in practice they
    // are mutually exclusive, but this keeps the render path total.
    let line = if app.editing.is_some() {
        Line::from(vec![
            Span::styled(
                "┃ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "editing message",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   (e to cancel)", Style::default().fg(Color::DarkGray)),
        ])
    } else if let Some(r) = app.replying_to.as_ref() {
        Line::from(vec![
            Span::styled(
                "┃ ",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled("replying to ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                r.sender.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(": ", Style::default().fg(Color::DarkGray)),
            Span::raw(r.preview.clone()),
            Span::styled("   (r to cancel)", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        return;
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_input(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Input;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_style(focused))
        .title(" Message ");
    app.input.set_block(block);
    app.input
        .set_cursor_line_style(Style::default()); // no underline
    f.render_widget(&app.input, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mode_style = match app.mode {
        Mode::Normal => Style::default().bg(Color::Blue).fg(Color::White),
        Mode::Insert => Style::default().bg(Color::Green).fg(Color::Black),
        Mode::Search => Style::default().bg(Color::Magenta).fg(Color::White),
        Mode::Help => Style::default().bg(Color::Yellow).fg(Color::Black),
    }
    .add_modifier(Modifier::BOLD);

    let focus_label = match app.focus {
        Focus::ChatList => "chats",
        Focus::Messages => "messages",
        Focus::Input => "input",
    };

    let line = Line::from(vec![
        Span::styled(app.mode.label().to_string(), mode_style),
        Span::raw(" "),
        Span::styled(
            format!("[{}]", focus_label),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  "),
        Span::styled(app.status.clone(), Style::default().fg(Color::DarkGray)),
    ]);

    f.render_widget(Paragraph::new(line).wrap(Wrap { trim: true }), area);
}

/// Unicode-aware word wrap. Splits on whitespace (collapsing runs), measures
/// each char by its terminal width, and hard-breaks words that exceed the
/// width on their own. Preserves explicit `\n` paragraph breaks.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if width == 0 {
        return vec![text.to_string()];
    }

    let char_width = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    let str_width = |s: &str| s.chars().map(char_width).sum::<usize>();

    let mut out: Vec<String> = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }

        let mut line = String::new();
        let mut line_w = 0usize;

        for word in paragraph.split_whitespace() {
            let word_w = str_width(word);
            let sep_w = if line.is_empty() { 0 } else { 1 };

            // If the current word won't fit (with a leading space), break.
            if line_w + sep_w + word_w > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
                line_w = 0;
            }

            if word_w > width {
                // Word longer than the whole width: hard-break char-by-char.
                for c in word.chars() {
                    let cw = char_width(c);
                    if line_w + cw > width && !line.is_empty() {
                        out.push(std::mem::take(&mut line));
                        line_w = 0;
                    }
                    line.push(c);
                    line_w += cw;
                }
            } else {
                if !line.is_empty() {
                    line.push(' ');
                    line_w += 1;
                }
                line.push_str(word);
                line_w += word_w;
            }
        }

        out.push(line);
    }

    if out.is_empty() {
        out.push(String::new());
    }
    out
}

//! Reusable fzf-like fuzzy picker modal, backed by `nucleo` for scoring.
//!
//! Usage:
//! - construct with `FuzzyPicker::new(target, sources)`,
//! - feed characters via `push_char` / `pop_char`,
//! - move selection via `move_cursor`,
//! - read the chosen source-index via `selected_source_index`.

use nucleo::{Config, Matcher, Utf32Str};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::app::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuzzyTarget {
    ChatList,
    Messages,
    GlobalSwitcher,
}

impl FuzzyTarget {
    pub fn title(&self) -> &'static str {
        match self {
            FuzzyTarget::ChatList => " Search chats ",
            FuzzyTarget::Messages => " Search messages ",
            FuzzyTarget::GlobalSwitcher => " Switch to chat ",
        }
    }
    pub fn prompt(&self) -> &'static str {
        match self {
            FuzzyTarget::ChatList => "chat> ",
            FuzzyTarget::Messages => "msg> ",
            FuzzyTarget::GlobalSwitcher => "> ",
        }
    }
}

pub struct FuzzyPicker {
    pub target: FuzzyTarget,
    sources: Vec<String>,
    query: String,
    /// (source_index, score), ranked descending.
    ranked: Vec<(usize, u32)>,
    cursor: usize,
    matcher: Matcher,
}

impl std::fmt::Debug for FuzzyPicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuzzyPicker")
            .field("target", &self.target)
            .field("query", &self.query)
            .field("n_sources", &self.sources.len())
            .field("n_ranked", &self.ranked.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl FuzzyPicker {
    pub fn new(target: FuzzyTarget, sources: Vec<String>) -> Self {
        let mut me = Self {
            target,
            sources,
            query: String::new(),
            ranked: vec![],
            cursor: 0,
            matcher: Matcher::new(Config::DEFAULT),
        };
        me.rescore();
        me
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.rescore();
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.rescore();
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.ranked.is_empty() {
            self.cursor = 0;
            return;
        }
        let len = self.ranked.len() as i32;
        let new = (self.cursor as i32 + delta).rem_euclid(len);
        self.cursor = new as usize;
    }

    /// Index into the *original* sources vec for the highlighted row.
    pub fn selected_source_index(&self) -> Option<usize> {
        self.ranked.get(self.cursor).map(|(i, _)| *i)
    }

    fn rescore(&mut self) {
        self.ranked.clear();

        if self.query.is_empty() {
            self.ranked
                .extend(self.sources.iter().enumerate().map(|(i, _)| (i, 0)));
            self.cursor = 0;
            return;
        }

        let mut needle_buf: Vec<char> = Vec::new();
        let needle = Utf32Str::new(&self.query, &mut needle_buf);

        let mut haystack_buf: Vec<char> = Vec::new();
        for (i, src) in self.sources.iter().enumerate() {
            haystack_buf.clear();
            let haystack = Utf32Str::new(src, &mut haystack_buf);
            if let Some(score) = self.matcher.fuzzy_match(haystack, needle) {
                self.ranked.push((i, score as u32));
            }
        }
        self.ranked.sort_by(|a, b| b.1.cmp(&a.1));
        self.cursor = 0;
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let Some(picker) = app.fuzzy.as_ref() else {
        return;
    };

    let area = super::centered(70, 70, f.area());
    f.render_widget(Clear, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let input_line = Line::from(vec![
        Span::styled(
            picker.target.prompt(),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
        Span::raw(&picker.query),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    let input = Paragraph::new(input_line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .title(picker.target.title()),
    );
    f.render_widget(input, layout[0]);

    let items: Vec<ListItem> = picker
        .ranked
        .iter()
        .map(|(idx, score)| {
            let text = picker.sources.get(*idx).map(String::as_str).unwrap_or("");
            let mut spans = vec![Span::raw(text.to_string())];
            if *score > 0 {
                spans.push(Span::styled(
                    format!("  ·{}", score),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut state = ListState::default();
    if !picker.ranked.is_empty() {
        state.select(Some(picker.cursor));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(format!(" {} matches ", picker.ranked.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, layout[1], &mut state);
}

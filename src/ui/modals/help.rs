//! Help modal — lists all Vim-style bindings.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem},
    Frame,
};

use crate::keymap;

pub fn draw(f: &mut Frame) {
    let area = super::centered(60, 70, f.area());
    f.render_widget(Clear, area);

    let items: Vec<ListItem> = keymap::bindings_help()
        .iter()
        .map(|(k, desc)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {:<18}", k),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(*desc),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Keybindings  (q/?/Esc to close) "),
    );
    f.render_widget(list, area);
}

//! Image viewer modal — renders the current [`ImageViewer`] state using
//! `ratatui-image`. Picker selection happens once at startup; rendering is
//! adaptive via the `StatefulImage` widget.

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Clear},
    Frame,
};
use ratatui_image::StatefulImage;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &mut App) {
    let Some(viewer) = app.image_viewer.as_mut() else {
        return;
    };

    let area = super::centered(80, 80, f.area());
    f.render_widget(Clear, area);

    let title = format!(
        " {}   (v/Esc/q to close) ",
        viewer
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("image")
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Carve off a one-row footer for caption/hint if we ever want one; for
    // now just render the image in the full inner area.
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1)])
        .split(inner);

    let image = StatefulImage::default();
    f.render_stateful_widget(image, layout[0], &mut viewer.protocol);
}

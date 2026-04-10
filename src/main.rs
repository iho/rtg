//! vim-telegam: a Vim-style terminal Telegram client.

mod app;
mod config;
mod event;
mod keymap;
mod telegram;
mod ui;

use anyhow::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing_subscriber::EnvFilter;

use crate::app::App;
use crate::config::Config;
use crate::event::{Event, EventLoop};
use crate::telegram::{TgEvent, TgHandle};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    // Connect + log in BEFORE entering raw mode so the login flow can prompt
    // interactively on stdin. After this returns we have a live actor, and
    // the TUI can take over the terminal.
    let config = Config::load();
    let player_argv = config.player_argv();
    let (tg_tx, tg_rx) = tokio::sync::mpsc::unbounded_channel::<TgEvent>();

    let tg = if config.has_credentials() {
        match telegram::grammers::spawn(&config, tg_tx.clone()).await {
            Ok(handle) => handle,
            Err(e) => {
                eprintln!("Failed to connect to Telegram: {e}");
                eprintln!("Falling back to mock backend.");
                telegram::mock::spawn(tg_tx)
            }
        }
    } else {
        eprintln!(
            "No Telegram credentials found. Set TG_API_ID and TG_API_HASH\n\
             (or write them to {:?}) to see real chats. Using mock backend.",
            Config::paths().config_file
        );
        telegram::mock::spawn(tg_tx)
    };

    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, tg, tg_rx, player_argv).await;
    restore_terminal(&mut terminal)?;
    result
}

fn init_tracing() -> Result<()> {
    let log_dir = directories::ProjectDirs::from("dev", "vimtelegam", "vim-telegam")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("vim-telegam"));
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::daily(log_dir, "vim-telegam.log");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(file_appender)
        .with_ansi(false)
        .init();
    Ok(())
}

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn run(
    terminal: &mut Tui,
    tg: TgHandle,
    tg_rx: UnboundedReceiver<TgEvent>,
    player_argv: Vec<String>,
) -> Result<()> {
    // Build the image protocol picker before the event stream starts —
    // query_stdio writes to stdout and reads from stdin, so it must run
    // before crossterm's EventStream takes over. Fall back to a safe
    // default font size if the query fails (e.g. non-Kitty terminals).
    let picker = ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::from_fontsize((8, 16)));

    let mut app = App::new(tg, picker, player_argv);
    let mut events = EventLoop::new(60.0, tg_rx);

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &mut app))?;

        match events.next().await? {
            Event::Key(key) => app.on_key(key),
            Event::Tick => app.on_tick(),
            Event::Resize(_, _) => {}
            Event::Tg(tg_ev) => app.apply_event(tg_ev),
        }
    }
    Ok(())
}

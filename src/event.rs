//! Async event loop: terminal input + ticks + telegram events, multiplexed.

use anyhow::Result;
use crossterm::event::{Event as CtEvent, EventStream, KeyEvent};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{interval, Interval};

use crate::telegram::TgEvent;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Event {
    Key(KeyEvent),
    Tick,
    Resize(u16, u16),
    Tg(TgEvent),
}

pub struct EventLoop {
    stream: EventStream,
    tick: Interval,
    tg_rx: UnboundedReceiver<TgEvent>,
}

impl EventLoop {
    pub fn new(tick_rate_hz: f64, tg_rx: UnboundedReceiver<TgEvent>) -> Self {
        let period = Duration::from_secs_f64(1.0 / tick_rate_hz);
        Self {
            stream: EventStream::new(),
            tick: interval(period),
            tg_rx,
        }
    }

    pub async fn next(&mut self) -> Result<Event> {
        loop {
            tokio::select! {
                _ = self.tick.tick() => return Ok(Event::Tick),
                Some(ev) = self.stream.next() => {
                    match ev? {
                        CtEvent::Key(k) if k.kind == crossterm::event::KeyEventKind::Press => {
                            return Ok(Event::Key(k));
                        }
                        CtEvent::Resize(w, h) => return Ok(Event::Resize(w, h)),
                        _ => continue,
                    }
                }
                Some(tg) = self.tg_rx.recv() => return Ok(Event::Tg(tg)),
            }
        }
    }
}

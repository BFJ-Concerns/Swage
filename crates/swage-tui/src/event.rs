//! Event loop multiplexing terminal input, gateway WebSocket events, and
//! render tick intervals.
//!
//! Uses `tokio::select!` to handle three concurrent concerns without any
//! blocking the others. Terminal events come from crossterm's async event
//! stream; gateway events from the WebSocket client; render ticks from a
//! tokio interval timer.

use futures::StreamExt;
use std::time::Duration;
use swage_client::websocket::ServerEvent;
use tokio::sync::mpsc;
use tokio::time::{self, Interval};

/// Multiplexes terminal input events, gateway WebSocket events, and render
/// ticks into a single `AppEvent` stream using `tokio::select!`.
///
/// Terminal events come from crossterm's async `EventStream`; gateway events
/// arrive through an `mpsc::Receiver` that the WebSocket read loop writes to
/// from a separate task; render ticks come from a `tokio` interval timer.
pub struct EventHandler {
    /// Async terminal event stream from crossterm.
    ///
    /// `None` in headless environments (tests, no-TTY contexts) where the
    /// stream cannot be initialised. When `None`, terminal events are never
    /// produced and only tick and gateway arms are active.
    stream: Option<crossterm::event::EventStream>,
    /// Fixed-interval timer that drives screen redraws.
    tick_interval: Interval,
}

impl EventHandler {
    /// Create an event handler that fires render ticks at the given rate.
    ///
    /// Initialises a crossterm `EventStream` for terminal input. Must be
    /// called in a context where a terminal (TTY) is available; use
    /// [`EventHandler::new_headless`] in tests or non-TTY environments.
    pub fn new(tick_rate: Duration) -> Self {
        Self {
            stream: Some(crossterm::event::EventStream::new()),
            tick_interval: time::interval(tick_rate),
        }
    }

    /// Create an event handler without a terminal stream.
    ///
    /// The terminal arm is permanently dormant — only tick and gateway events
    /// fire. Intended for tests and environments without a TTY.
    pub fn new_headless(tick_rate: Duration) -> Self {
        Self {
            stream: None,
            tick_interval: time::interval(tick_rate),
        }
    }

    /// Wait for the next event from any source and return it.
    ///
    /// Three sources are multiplexed with `tokio::select!`:
    /// 1. Terminal key press or resize from crossterm (if stream is active).
    /// 2. `ServerEvent` from the gateway via an mpsc channel (when `Some`).
    /// 3. Render tick from the interval timer.
    ///
    /// When the gateway channel is closed, `gateway_rx` is set to `None` and
    /// `AppEvent::Disconnected` is returned once. Subsequent calls skip that
    /// arm since `pending()` never resolves when no receiver is present.
    pub async fn next(&mut self, gateway_rx: &mut Option<mpsc::Receiver<ServerEvent>>) -> AppEvent {
        loop {
            tokio::select! {
                // Terminal keypress, resize, focus, or paste events.
                // When stream is None (headless mode), this arm is permanently
                // dormant via `pending()`.
                maybe_event = async {
                    match self.stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending::<
                            Option<std::io::Result<crossterm::event::Event>>
                        >().await,
                    }
                } => {
                    match maybe_event {
                        Some(Ok(event)) => return AppEvent::Terminal(event),
                        // Terminal error or end-of-stream — retry rather than
                        // propagating; the next poll may succeed.
                        Some(Err(_)) | None => continue,
                    }
                }

                // Gateway ServerEvent via mpsc channel.
                // When gateway_rx is None, `pending()` keeps this arm dormant.
                gateway = async {
                    match gateway_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending::<Option<ServerEvent>>().await,
                    }
                } => {
                    match gateway {
                        Some(event) => return AppEvent::Gateway(Box::new(event)),
                        // All senders dropped — gateway has disconnected.
                        None => {
                            *gateway_rx = None;
                            return AppEvent::Disconnected;
                        }
                    }
                }

                // Render tick fires at the configured interval.
                _ = self.tick_interval.tick() => return AppEvent::Tick,
            }
        }
    }
}

/// Events the application event loop can produce.
pub enum AppEvent {
    /// A terminal input event (key press, resize, etc.).
    Terminal(crossterm::event::Event),
    /// A server event received from the gateway WebSocket.
    Gateway(Box<swage_client::websocket::ServerEvent>),
    /// A render tick — time to redraw the screen.
    Tick,
    /// The gateway WebSocket connection was lost.
    Disconnected,
}

#[cfg(test)]
mod tests {
    // These timeouts bound test waits around single-event receives; expiry only
    // drops the receive future and does not abandon shared state mutation.
    #![allow(clippy::disallowed_methods)]

    use super::*;
    use std::time::Duration;
    use swage_client::websocket::ServerEvent;
    use tokio::sync::mpsc;

    // Gateway event sent through the mpsc channel arrives as AppEvent::Gateway.
    #[tokio::test]
    async fn gateway_event_routes_to_gateway_variant() {
        let mut handler = EventHandler::new_headless(Duration::from_millis(1000));
        let (tx, rx) = mpsc::channel(8);
        let mut rx = Some(rx);
        tx.send(ServerEvent::Shutdown {
            reason: "test".to_string(),
        })
        .await
        .unwrap();
        let event = tokio::time::timeout(Duration::from_millis(200), handler.next(&mut rx))
            .await
            .expect("gateway event should arrive quickly");
        assert!(matches!(event, AppEvent::Gateway(_)));
    }

    // Dropping all senders produces AppEvent::Disconnected and clears gateway_rx.
    #[tokio::test]
    async fn channel_close_produces_disconnected_and_clears_receiver() {
        let mut handler = EventHandler::new_headless(Duration::from_millis(1000));
        let (tx, rx) = mpsc::channel::<ServerEvent>(8);
        let mut rx = Some(rx);
        drop(tx);
        let event = tokio::time::timeout(Duration::from_millis(200), handler.next(&mut rx))
            .await
            .expect("disconnected event should arrive quickly");
        assert!(matches!(event, AppEvent::Disconnected));
        assert!(rx.is_none(), "gateway_rx should be cleared on disconnect");
    }

    // With a short tick interval and no other events, AppEvent::Tick is returned.
    #[tokio::test]
    async fn tick_fires_when_no_gateway_channel() {
        let mut handler = EventHandler::new_headless(Duration::from_millis(5));
        let result = tokio::time::timeout(Duration::from_millis(500), handler.next(&mut None))
            .await
            .expect("tick should fire within timeout");
        // Headless handler has no terminal stream, so only Tick can fire.
        assert!(matches!(result, AppEvent::Tick), "expected Tick event");
    }

    // AppEvent variants are all distinct (no accidental equality).
    #[test]
    fn app_event_variants_are_distinct() {
        // Each variant wraps a different type — this is a compile-time check
        // that the enum is well-formed. The discriminant offsets confirm they are
        // separate variants.
        use std::mem::discriminant;
        let terminal = AppEvent::Terminal(crossterm::event::Event::FocusGained);
        let tick = AppEvent::Tick;
        let disconnected = AppEvent::Disconnected;
        assert_ne!(discriminant(&terminal), discriminant(&tick));
        assert_ne!(discriminant(&tick), discriminant(&disconnected));
        assert_ne!(discriminant(&terminal), discriminant(&disconnected));
    }
}

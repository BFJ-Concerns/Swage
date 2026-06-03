use std::{future::Future, time::Duration};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use swage_core::GatewayError;

/// Spawn a maintenance loop that stops cooperatively between ticks.
///
/// Cancellation is deliberately checked before a tick starts and while waiting
/// for the next interval, but not while the tick future is running. Several
/// gateway maintenance ticks move persisted rows through multi-step state
/// transitions; interrupting the future at an arbitrary `.await` can leave
/// durable `running` records with no later selector able to resume them.
pub fn spawn_periodic_loop<F, Fut>(
    interval: Duration,
    shutdown: CancellationToken,
    error_message: &'static str,
    mut tick: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), GatewayError>> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            if shutdown.is_cancelled() {
                break;
            }

            if let Err(error) = tick().await {
                warn!(%error, "{error_message}");
            }

            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(interval) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_waits_for_in_flight_tick_to_finish() {
        let shutdown = CancellationToken::new();
        let did_finish_tick = Arc::new(AtomicBool::new(false));
        let did_finish_tick_for_loop = Arc::clone(&did_finish_tick);
        let (tick_started_tx, tick_started_rx) = oneshot::channel();
        let (finish_tick_tx, finish_tick_rx) = oneshot::channel();
        let mut tick_started_tx = Some(tick_started_tx);
        let mut finish_tick_rx = Some(finish_tick_rx);

        let handle = spawn_periodic_loop(
            Duration::from_secs(60),
            shutdown.clone(),
            "test tick failed",
            move || {
                let tick_started_tx = tick_started_tx.take();
                let finish_tick_rx = finish_tick_rx.take();
                let did_finish_tick = Arc::clone(&did_finish_tick_for_loop);
                async move {
                    if let Some(tick_started_tx) = tick_started_tx {
                        let _ = tick_started_tx.send(());
                    }
                    if let Some(finish_tick_rx) = finish_tick_rx {
                        finish_tick_rx.await.expect("test should release tick");
                    }
                    did_finish_tick.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        );

        tick_started_rx.await.expect("tick should start");
        shutdown.cancel();
        tokio::task::yield_now().await;

        assert!(
            !handle.is_finished(),
            "loop must not abort an in-flight tick on cancellation"
        );
        assert!(!did_finish_tick.load(Ordering::SeqCst));

        finish_tick_tx
            .send(())
            .expect("loop should still await tick");
        handle.await.expect("background loop should join cleanly");
        assert!(did_finish_tick.load(Ordering::SeqCst));
    }
}

//! `swage gateway status` — check whether the gateway is running and healthy.
//!
//! Combines runtime metadata inspection with a live health check to report
//! the full gateway state: PID, address, uptime, and health status.

use chrono::Utc;
use swage_client::{error::ClientError, http::GatewayClient, runtime::RuntimeMetadata};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the `swage gateway status` command.
///
/// Exit semantics:
/// - `Ok(())` (exit 0): gateway is running and health check passed
/// - `Err(_)` (exit non-zero): not running, stale, or unresponsive
pub async fn run() -> Result<(), ClientError> {
    let meta = match RuntimeMetadata::load() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Gateway is not running");
            return Err(ClientError::GatewayNotRunning {
                reason: "no metadata file found".to_string(),
            });
        }
        Err(e) => return Err(ClientError::MetadataIo { source: e }),
    };

    // Metadata file exists — check if the PID is still alive.
    if meta.is_stale() {
        eprintln!("Gateway is not running (stale PID file removed)");
        // Clean up the stale file so subsequent commands don't see it.
        let _ = RuntimeMetadata::remove();
        return Err(ClientError::GatewayNotRunning {
            reason: format!("PID {} is no longer running", meta.pid),
        });
    }

    // PID is alive — run a live health check.
    let client = GatewayClient::new(&meta.address);
    match client.health().await {
        Ok(()) => {
            let uptime = format_uptime(Utc::now() - meta.started_at);
            println!("Gateway is running (PID {})", meta.pid);
            println!("  Address: {}", meta.address);
            println!("  Uptime:  {uptime}");
            Ok(())
        }
        Err(_) => {
            eprintln!(
                "Gateway process is running (PID {}) but not responding to health checks",
                meta.pid
            );
            Err(ClientError::GatewayNotRunning {
                reason: format!("PID {} is alive but health check failed", meta.pid),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Uptime formatting
// ---------------------------------------------------------------------------

/// Format a duration as a human-readable uptime string.
///
/// - < 1 minute → `"Xs"` (seconds only)
/// - 1–59 minutes → `"Xm Ys"` (minutes and remaining seconds)
/// - ≥ 1 hour → `"Xh Ym"` (hours and remaining minutes; seconds omitted)
///
/// Negative durations (clock drift) are treated as zero to avoid confusing output.
pub(crate) fn format_uptime(duration: chrono::Duration) -> String {
    // Clamp to zero — negative durations should not produce confusing output.
    let total_secs = duration.num_seconds().max(0);

    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

// ---------------------------------------------------------------------------
// Tests — written first; they fail until format_uptime and run() are defined
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    // Sub-minute durations render as seconds only.
    #[test]
    fn format_uptime_seconds_only() {
        assert_eq!(format_uptime(Duration::seconds(0)), "0s");
        assert_eq!(format_uptime(Duration::seconds(1)), "1s");
        assert_eq!(format_uptime(Duration::seconds(45)), "45s");
        assert_eq!(format_uptime(Duration::seconds(59)), "59s");
    }

    // 1–59 minutes renders as "Xm Ys".
    #[test]
    fn format_uptime_minutes_and_seconds() {
        assert_eq!(format_uptime(Duration::seconds(60)), "1m 0s");
        assert_eq!(format_uptime(Duration::seconds(90)), "1m 30s");
        assert_eq!(format_uptime(Duration::seconds(5 * 60 + 3)), "5m 3s");
        assert_eq!(format_uptime(Duration::seconds(59 * 60 + 59)), "59m 59s");
    }

    // >= 1 hour renders as "Xh Ym" (minutes, no seconds).
    #[test]
    fn format_uptime_hours_and_minutes() {
        assert_eq!(format_uptime(Duration::seconds(3600)), "1h 0m");
        assert_eq!(format_uptime(Duration::seconds(3600 + 5 * 60)), "1h 5m");
        assert_eq!(
            format_uptime(Duration::seconds(2 * 3600 + 34 * 60 + 17)),
            "2h 34m"
        );
        assert_eq!(
            format_uptime(Duration::seconds(12 * 3600 + 3 * 60)),
            "12h 3m"
        );
    }

    // Negative or very small durations should not panic.
    #[test]
    fn format_uptime_handles_near_zero() {
        // Negative could theoretically appear if clocks drift — should not panic.
        let result = std::panic::catch_unwind(|| format_uptime(Duration::seconds(-5)));
        assert!(
            result.is_ok(),
            "format_uptime should not panic on negative input"
        );
    }
}

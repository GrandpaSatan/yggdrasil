/// Background Flow Scheduler — Yggdrasil "Always-On" Dream Mode (Sprint 055).
///
/// Runs in a separate tokio task, monitoring for:
/// 1. **Idle triggers** — when no user requests arrive for `min_idle_secs`, runs idle-triggered flows
/// 2. **Cron triggers** — runs flows on a cron schedule
///
/// The scheduler yields immediately when a user request comes in (idle flows are lower priority).
/// Dream flows run sequentially to avoid overloading GPU resources.
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use ygg_domain::config::{FlowConfig, FlowTrigger};

use crate::flow::FlowEngine;
use crate::state::AppState;

/// Tracks the last time a user request was processed.
/// Updated by the chat handler, read by the scheduler.
#[derive(Clone)]
pub struct ActivityTracker {
    tx: watch::Sender<Instant>,
    rx: watch::Receiver<Instant>,
}

impl ActivityTracker {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(Instant::now());
        Self { tx, rx }
    }

    /// Called by chat_handler on every incoming request.
    pub fn mark_active(&self) {
        let _ = self.tx.send(Instant::now());
    }

    /// Returns how long since the last user activity.
    pub fn idle_duration(&self) -> Duration {
        self.rx.borrow().elapsed()
    }
}

/// Spawn the background flow scheduler.
///
/// This task runs for the lifetime of the server. It checks every 30 seconds
/// for idle or cron conditions and executes matching flows.
pub fn spawn_scheduler(
    state: Arc<AppState>,
    activity: ActivityTracker,
    flows: Vec<FlowConfig>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let check_interval = Duration::from_secs(30);

        // Separate flows by trigger type
        let idle_flows: Vec<&FlowConfig> = flows
            .iter()
            .filter(|f| matches!(f.trigger, FlowTrigger::Idle { .. }))
            .collect();

        let cron_flows: Vec<&FlowConfig> = flows
            .iter()
            .filter(|f| matches!(f.trigger, FlowTrigger::Cron { .. }))
            .collect();

        if idle_flows.is_empty() && cron_flows.is_empty() {
            tracing::info!("no background flows configured — scheduler idle");
            return;
        }

        tracing::info!(
            idle = idle_flows.len(),
            cron = cron_flows.len(),
            "background flow scheduler started"
        );

        let mut last_cron_check = Instant::now();

        loop {
            tokio::time::sleep(check_interval).await;

            // ── Idle flows ───────────────────────────────────
            let idle_secs = activity.idle_duration().as_secs();

            for flow in &idle_flows {
                if let FlowTrigger::Idle { min_idle_secs } = &flow.trigger {
                    if idle_secs >= *min_idle_secs {
                        // Double-check: still idle? (user may have sent a request during our check)
                        if activity.idle_duration().as_secs() >= *min_idle_secs {
                            tracing::info!(
                                flow = %flow.name,
                                idle_secs = idle_secs,
                                "triggering idle flow"
                            );

                            match state.flow_engine.execute(flow, "consolidate").await {
                                Ok(result) => {
                                    tracing::info!(
                                        flow = %flow.name,
                                        ms = result.elapsed_ms,
                                        steps = result.step_timings.len(),
                                        "idle flow complete"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        flow = %flow.name,
                                        error = %e,
                                        "idle flow failed"
                                    );
                                }
                            }
                        }
                    }
                }
            }

            // ── Cron flows ───────────────────────────────────
            let since_last = last_cron_check.elapsed();
            if since_last >= Duration::from_secs(60) {
                last_cron_check = Instant::now();

                for flow in &cron_flows {
                    if let FlowTrigger::Cron { schedule } = &flow.trigger {
                        // Simple cron check: parse "interval_minutes" from schedule
                        // Full cron parsing deferred to next sprint
                        if let Some(mins) = parse_simple_interval(schedule) {
                            let now_mins = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                / 60;

                            if now_mins % mins == 0 {
                                tracing::info!(
                                    flow = %flow.name,
                                    schedule = %schedule,
                                    "triggering cron flow"
                                );

                                match state.flow_engine.execute(flow, "scheduled").await {
                                    Ok(result) => {
                                        tracing::info!(
                                            flow = %flow.name,
                                            ms = result.elapsed_ms,
                                            "cron flow complete"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            flow = %flow.name,
                                            error = %e,
                                            "cron flow failed"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Parse simple interval from cron-like schedule string.
/// Supports: "every_Nm" (e.g. "every_240m" = every 4 hours).
/// Full cron expression parsing deferred to next sprint.
fn parse_simple_interval(schedule: &str) -> Option<u64> {
    if schedule.starts_with("every_") && schedule.ends_with('m') {
        schedule[6..schedule.len() - 1].parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_interval() {
        assert_eq!(parse_simple_interval("every_240m"), Some(240));
        assert_eq!(parse_simple_interval("every_60m"), Some(60));
        assert_eq!(parse_simple_interval("every_5m"), Some(5));
        assert_eq!(parse_simple_interval("0 */4 * * *"), None); // full cron not yet supported
        assert_eq!(parse_simple_interval("invalid"), None);
    }

    #[test]
    fn test_activity_tracker() {
        let tracker = ActivityTracker::new();
        // Just created — idle for ~0 seconds
        assert!(tracker.idle_duration().as_secs() < 2);

        tracker.mark_active();
        assert!(tracker.idle_duration().as_secs() < 1);
    }
}

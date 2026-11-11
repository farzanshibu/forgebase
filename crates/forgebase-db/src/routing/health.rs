//! Replica health: lag estimates and the rotation state machine
//! (ADR-0029 §2.7). Pure code; the monitor in [`super::replicas`] feeds it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::{Lsn, ReplicaOptions};
use crate::topology::NodeState;

/// Primary WAL positions kept to estimate time lag (at 500 ms, about two
/// minutes).
const HISTORY: usize = 256;

/// The primary's WAL position over time, to turn a replica's replay
/// position into a time lag without privileged views or clock agreement:
/// the replica is behind by the age of the oldest sample whose position it
/// has not replayed.
#[derive(Debug, Default)]
pub(crate) struct LsnHistory {
    samples: VecDeque<(Instant, Lsn)>,
}

impl LsnHistory {
    pub(crate) fn push(&mut self, at: Instant, lsn: Lsn) {
        if self.samples.back().is_some_and(|(_, l)| *l > lsn) {
            // The primary went back (a restore, a new primary): start over.
            self.samples.clear();
        }
        self.samples.push_back((at, lsn));
        while self.samples.len() > HISTORY {
            self.samples.pop_front();
        }
    }

    /// The latest primary position.
    #[cfg(test)]
    pub(crate) fn latest(&self) -> Option<Lsn> {
        self.samples.back().map(|(_, l)| *l)
    }

    /// How long a replica at `replay` is behind, as of `now`.
    pub(crate) fn lag(&self, replay: Lsn, now: Instant) -> Duration {
        let Some(&(_, latest)) = self.samples.back() else {
            return Duration::ZERO;
        };
        if replay >= latest {
            return Duration::ZERO;
        }
        // The first sample the replica has not replayed; older than the
        // whole history when it has replayed none of them.
        let first_missing = self
            .samples
            .iter()
            .find(|(_, l)| *l > replay)
            .map_or(now, |(at, _)| *at);
        now.saturating_duration_since(first_missing)
    }
}

/// One health sample of a replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sample {
    /// In recovery, `lag` behind.
    Standby { replay: Lsn, lag: Duration, lag_bytes: u64 },
    /// Answers but is not in recovery (promoted, or never a standby).
    NotStandby,
    /// Did not answer.
    Unreachable(String),
}

/// Rotation state of one replica. Only [`NodeState::Streaming`] serves reads.
#[derive(Debug, Clone)]
pub(crate) struct Health {
    pub(crate) state: NodeState,
    /// Consecutive samples under half the limits while out of rotation.
    good: u32,
    pub(crate) lag: Duration,
    pub(crate) lag_bytes: u64,
    pub(crate) last_error: Option<String>,
    pub(crate) sampled: bool,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            state: NodeState::CatchingUp,
            good: 0,
            lag: Duration::ZERO,
            lag_bytes: 0,
            last_error: None,
            sampled: false,
        }
    }
}

impl Health {
    /// Whether the replica may serve reads.
    pub(crate) fn in_rotation(&self) -> bool {
        self.state == NodeState::Streaming
    }

    /// Takes one sample; returns the new state when it changed.
    ///
    /// * Over `max_lag` or `max_lag_bytes`: out of rotation (`lagging`).
    /// * Out of rotation (`lagging`, `unreachable`, `needs_rebuild`): back
    ///   after `return_after` consecutive samples under half of both limits.
    /// * The first sample of a new node puts it in rotation at once when it
    ///   is within the limits (`catching_up` → `streaming`).
    pub(crate) fn observe(&mut self, sample: &Sample, limits: &ReplicaOptions) -> Option<NodeState> {
        let before = self.state;
        let first = !self.sampled;
        self.sampled = true;
        match sample {
            Sample::Unreachable(e) => {
                self.last_error = Some(e.clone());
                self.good = 0;
                self.state = NodeState::Unreachable;
            }
            Sample::NotStandby => {
                self.last_error = Some("the node is not in recovery (promoted, or not a standby)".into());
                self.good = 0;
                self.state = NodeState::NeedsRebuild;
            }
            Sample::Standby { lag, lag_bytes, .. } => {
                self.lag = *lag;
                self.lag_bytes = *lag_bytes;
                let over = *lag > limits.max_lag || *lag_bytes > limits.max_lag_bytes;
                let under_half = *lag <= limits.max_lag / 2 && *lag_bytes <= limits.max_lag_bytes / 2;
                match self.state {
                    NodeState::Streaming => {
                        if over {
                            self.good = 0;
                            self.state = NodeState::Lagging;
                        }
                    }
                    _ if first || self.state == NodeState::CatchingUp => {
                        if over {
                            self.state = if first {
                                NodeState::Lagging
                            } else {
                                NodeState::CatchingUp
                            };
                        } else {
                            self.last_error = None;
                            self.state = NodeState::Streaming;
                        }
                    }
                    _ => {
                        if under_half {
                            self.good += 1;
                            if self.good >= limits.return_after.max(1) {
                                self.good = 0;
                                self.last_error = None;
                                self.state = NodeState::Streaming;
                            }
                        } else {
                            self.good = 0;
                        }
                    }
                }
            }
        }
        (self.state != before).then_some(self.state)
    }

    /// The read path saw the replica fail to answer: out of rotation until
    /// the monitor brings it back.
    pub(crate) fn mark_unreachable(&mut self, error: String) -> Option<NodeState> {
        let before = self.state;
        self.sampled = true;
        self.good = 0;
        self.last_error = Some(error);
        self.state = NodeState::Unreachable;
        (self.state != before).then_some(self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ReplicaOptions {
        ReplicaOptions {
            max_lag: Duration::from_millis(2_000),
            max_lag_bytes: 1_000,
            ..ReplicaOptions::default()
        }
    }

    fn standby(lag_ms: u64, lag_bytes: u64) -> Sample {
        Sample::Standby {
            replay: Lsn(0),
            lag: Duration::from_millis(lag_ms),
            lag_bytes,
        }
    }

    #[test]
    fn lagging_replicas_return_after_three_samples_under_half_the_limit() {
        let l = limits();
        let mut h = Health::default();
        assert!(!h.in_rotation());
        assert_eq!(h.observe(&standby(0, 0), &l), Some(NodeState::Streaming));
        assert_eq!(h.observe(&standby(1_999, 1_000), &l), None, "at the limit stays");
        assert_eq!(h.observe(&standby(0, 1_001), &l), Some(NodeState::Lagging));
        assert!(!h.in_rotation());
        // Under the limit but above half: no progress.
        assert_eq!(h.observe(&standby(1_500, 0), &l), None);
        assert_eq!(h.observe(&standby(0, 400), &l), None);
        assert_eq!(
            h.observe(&standby(0, 501), &l),
            None,
            "a sample above half resets the count"
        );
        assert_eq!(h.observe(&standby(0, 400), &l), None);
        assert_eq!(h.observe(&standby(1_000, 500), &l), None);
        assert_eq!(h.observe(&standby(0, 0), &l), Some(NodeState::Streaming));
        assert!(h.in_rotation());
    }

    #[test]
    fn unreachable_and_promoted_nodes_leave_rotation() {
        let l = limits();
        let mut h = Health::default();
        h.observe(&standby(0, 0), &l);
        assert_eq!(
            h.observe(&Sample::Unreachable("refused".into()), &l),
            Some(NodeState::Unreachable)
        );
        assert_eq!(h.last_error.as_deref(), Some("refused"));
        for _ in 0..2 {
            assert_eq!(h.observe(&standby(0, 0), &l), None);
        }
        assert_eq!(h.observe(&standby(0, 0), &l), Some(NodeState::Streaming));
        assert_eq!(h.last_error, None);
        assert_eq!(h.observe(&Sample::NotStandby, &l), Some(NodeState::NeedsRebuild));
        assert_eq!(h.mark_unreachable("io".into()), Some(NodeState::Unreachable));
        assert_eq!(h.mark_unreachable("io".into()), None);
    }

    #[test]
    fn a_new_node_over_the_limit_starts_lagging() {
        let mut h = Health::default();
        assert_eq!(h.observe(&standby(5_000, 0), &limits()), Some(NodeState::Lagging));
    }

    #[test]
    fn time_lag_is_the_age_of_the_first_unreplayed_sample() {
        let t0 = Instant::now();
        let mut hist = LsnHistory::default();
        assert_eq!(hist.lag(Lsn(0), t0), Duration::ZERO);
        hist.push(t0, Lsn(100));
        hist.push(t0 + Duration::from_millis(500), Lsn(100));
        hist.push(t0 + Duration::from_millis(1_000), Lsn(200));
        hist.push(t0 + Duration::from_millis(1_500), Lsn(300));
        let now = t0 + Duration::from_millis(2_000);
        assert_eq!(hist.latest(), Some(Lsn(300)));
        assert_eq!(hist.lag(Lsn(300), now), Duration::ZERO);
        assert_eq!(hist.lag(Lsn(400), now), Duration::ZERO);
        assert_eq!(hist.lag(Lsn(250), now), Duration::from_millis(500));
        assert_eq!(hist.lag(Lsn(100), now), Duration::from_millis(1_000));
        assert_eq!(hist.lag(Lsn(50), now), Duration::from_millis(2_000));
        // A primary that goes back resets the history.
        hist.push(now, Lsn(10));
        assert_eq!(
            hist.lag(Lsn(5), now + Duration::from_millis(10)),
            Duration::from_millis(10)
        );
    }
}

//! Pure scheduling policy; AppKit callbacks and ownership stay in Runtime.
use std::time::{Duration, Instant};

pub(super) const PERMISSION_POLL_MS: u64 = 1_000;

#[derive(Default)]
pub(super) struct PermissionRetry {
    delay_ms: u64,
}
impl PermissionRetry {
    pub(super) fn restart(&mut self) {
        self.delay_ms = 0;
    }
    pub(super) fn next(&mut self, needed: bool) -> Option<u64> {
        if !needed {
            self.restart();
            return None;
        }
        self.delay_ms = if self.delay_ms == 0 {
            PERMISSION_POLL_MS
        } else {
            (self.delay_ms * 2).min(30_000)
        };
        Some(self.delay_ms)
    }
}

pub(super) fn asr_wake_deadline(
    now: Instant,
    operation: Option<Instant>,
    cleanup_pending: bool,
    scheduled: Option<Instant>,
    cleanup_check_ms: &mut u64,
) -> Option<Instant> {
    if !cleanup_pending {
        *cleanup_check_ms = 10;
    }
    operation.or_else(|| {
        if !cleanup_pending {
            return None;
        }
        scheduled.or_else(|| {
            let deadline = now + Duration::from_millis(*cleanup_check_ms);
            *cleanup_check_ms = (*cleanup_check_ms * 2).min(1_000);
            Some(deadline)
        })
    })
}

const EVENT_LANES: usize = 11;
pub(super) const EVENTS_PER_LANE: usize = 32;
pub(super) const EVENTS_PER_PASS: usize = 128;
pub(super) const EVENT_BUDGET: Duration = Duration::from_millis(2);

pub(super) fn drain_event_lanes(
    next_lane: &mut usize,
    mut drain: impl FnMut(usize) -> bool,
    mut elapsed: impl FnMut() -> Duration,
) -> bool {
    let mut counts = [0; EVENT_LANES];
    let mut available = [true; EVENT_LANES];
    let mut total = 0;
    let mut continuation = false;
    while available.iter().any(|ready| *ready) {
        let lane = *next_lane;
        *next_lane = (lane + 1) % EVENT_LANES;
        if !available[lane] {
            continue;
        }
        if drain(lane) {
            counts[lane] += 1;
            total += 1;
            if counts[lane] == EVENTS_PER_LANE {
                available[lane] = false;
                continuation = true;
            }
            if total == EVENTS_PER_PASS || elapsed() >= EVENT_BUDGET {
                continuation = true;
                break;
            }
        } else {
            available[lane] = false;
        }
    }
    continuation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_and_pass_caps_are_fair_and_consumed_suppression_continues() {
        let mut cursor = 0;
        let mut counts = [0; EVENT_LANES];
        assert!(drain_event_lanes(
            &mut cursor,
            |lane| {
                counts[lane] += 1;
                true
            },
            || Duration::ZERO
        ));
        assert_eq!(counts.iter().sum::<usize>(), EVENTS_PER_PASS);
        assert!(counts.iter().all(|count| (11..=12).contains(count)));
        let mut remaining = 65;
        let mut passes = 0;
        loop {
            let mut consumed = 0;
            let more = drain_event_lanes(
                &mut cursor,
                |lane| {
                    if lane == 8 && remaining != 0 {
                        remaining -= 1;
                        consumed += 1;
                        true
                    } else {
                        false
                    }
                },
                || Duration::ZERO,
            );
            assert!(consumed <= 32);
            passes += 1;
            if !more {
                break;
            }
        }
        assert_eq!(passes, 3);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn cooperative_time_budget_rotates_even_if_one_handler_takes_two_ms() {
        let mut cursor = 0;
        let mut serviced = Vec::new();
        for _ in 0..EVENT_LANES {
            assert!(drain_event_lanes(
                &mut cursor,
                |lane| {
                    serviced.push(lane);
                    true
                },
                || EVENT_BUDGET
            ));
        }
        assert_eq!(serviced, (0..EVENT_LANES).collect::<Vec<_>>());
        assert!(!drain_event_lanes(&mut cursor, |_| false, || EVENT_BUDGET));
    }

    #[test]
    fn completion_watchdog_backoff_is_pending_only_and_keeps_exact_operation_deadline() {
        let now = Instant::now();
        let mut delay = 10;
        assert_eq!(asr_wake_deadline(now, None, false, None, &mut delay), None);
        let operation = now + Duration::from_secs(60);
        assert_eq!(
            asr_wake_deadline(now, Some(operation), false, None, &mut delay),
            Some(operation)
        );
        assert_eq!(
            asr_wake_deadline(now, None, false, Some(operation), &mut delay),
            None
        );
        let first = asr_wake_deadline(now, None, true, None, &mut delay).unwrap();
        assert_eq!(first, now + Duration::from_millis(10));
        assert_eq!(
            asr_wake_deadline(now, None, true, Some(first), &mut delay),
            Some(first)
        );
        assert_eq!(delay, 20);
        let mut instant = first;
        for _ in 0..20 {
            let next = asr_wake_deadline(instant, None, true, None, &mut delay).unwrap();
            assert!(next > instant);
            assert!(next <= instant + Duration::from_secs(1));
            instant = next;
        }
        assert_eq!(
            asr_wake_deadline(instant, None, false, Some(instant), &mut delay),
            None
        );
        assert_eq!(delay, 10);
    }

    #[test]
    fn permission_backoff_has_no_healthy_idle_timer_and_caps_retries() {
        let mut retry = PermissionRetry::default();
        assert_eq!(
            (0..8)
                .map(|_| retry.next(true).unwrap())
                .collect::<Vec<_>>(),
            [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]
        );
        for _ in 0..100 {
            assert_eq!(retry.next(true), Some(30_000));
        }
        assert_eq!(retry.next(false), None);
        for _ in 0..100 {
            assert_eq!(retry.next(false), None);
        }
        retry.restart();
        assert_eq!(retry.next(true), Some(1_000));
    }
}

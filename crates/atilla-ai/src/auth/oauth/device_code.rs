// straitjacket-allow-file[:duplication] — the inline `#[cfg(test)] mod tests`
// rebuilds the same poll scaffold (FakeClock + recording sleeper + poll closure)
// per case so each interval/timeout path is exercised in isolation. The clone
// detector reads the repeated test setup as duplication; it is deliberate,
// load-bearing per-case fixtures.
//! RFC 8628 device-code poller, ported from pi-ai's
//! `packages/ai/src/auth/oauth/device-code.ts` at pinned commit `3da591ab`.
//!
//! [`poll_oauth_device_code_flow`] drives the poll loop: it computes a deadline
//! from `expires_in_seconds`, sleeps `interval` between polls, honors server
//! `slow_down` responses, and throws the exact timeout messages pi does
//! (`device-code.ts:46-98`).
//!
//! # Sync port deviations
//!
//! pi's `abortableSleep` is an `async` `setTimeout` + `AbortSignal` race
//! (`device-code.ts:26-44`). Here the loop is synchronous: the deadline is read
//! from the [`Clock`] seam, and each wait is delegated to an injected `sleep`
//! closure so the interval progression is testable. Production callers build the
//! closure from the [`Timers`] seam via [`abortable_sleep`]; tests pass a closure
//! that advances a [`crate::seams::clock::FakeClock`] and records the delays.
//! Abort is checked at the top of each loop iteration and by the sleeper (pi
//! also cancels a sleep mid-wait; the sync port checks abort before and after
//! the wait rather than interrupting it).

use std::sync::{Arc, Condvar, Mutex};

use crate::seams::clock::{Clock, Timers};
use crate::seams::provider::AbortSignal;

use crate::auth::error::AuthFlowError;

/// Minimum poll interval, in ms (`device-code.ts:5`).
pub const MINIMUM_INTERVAL_MS: i64 = 1000;
/// RFC 8628 §3.2 default poll interval when the server omits `interval`
/// (`device-code.ts:7`).
pub const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
/// RFC 8628 §3.5 `slow_down` interval increment, in ms (`device-code.ts:9`).
pub const SLOW_DOWN_INTERVAL_INCREMENT_MS: i64 = 5000;

/// Cancellation message (`device-code.ts:1`).
pub const CANCEL_MESSAGE: &str = "Login cancelled";
/// Plain timeout message (`device-code.ts:2`).
pub const TIMEOUT_MESSAGE: &str = "Device flow timed out";
/// Timeout message after one or more `slow_down` responses, with the WSL/VM
/// clock-drift wording copied verbatim (`device-code.ts:3-4`).
pub const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";

/// The outcome of a single poll (`device-code.ts:11-16`).
#[derive(Debug, Clone, PartialEq)]
pub enum PollResult<T> {
    /// Authorization still pending.
    Pending,
    /// The server asked the client to slow down; carries the optional new
    /// server interval.
    SlowDown {
        /// The server-provided interval, in seconds, if any.
        interval_seconds: Option<f64>,
    },
    /// The flow failed with this message.
    Failed {
        /// The failure message.
        message: String,
    },
    /// The flow completed with this value.
    Complete {
        /// The completed value (e.g. tokens).
        value: T,
    },
}

/// Poll configuration (`device-code.ts:18-24`).
#[derive(Debug, Clone, Default)]
pub struct DeviceCodePollOptions {
    /// The initial poll interval, in seconds.
    pub interval_seconds: Option<f64>,
    /// The device-code lifetime, in seconds (deadline). `None` = no deadline.
    pub expires_in_seconds: Option<f64>,
    /// Whether to wait one interval before the first poll.
    pub wait_before_first_poll: bool,
}

/// A `sleep` step: block for `delay_ms`, or return [`AuthFlowError`] on cancel.
pub type SleepFn<'a> = dyn FnMut(u64) -> Result<(), AuthFlowError> + 'a;

/// A single poll step, producing the next [`PollResult`].
pub type PollFn<'a, T> = dyn FnMut() -> Result<PollResult<T>, AuthFlowError> + 'a;

/// Block for `delay_ms` using the [`Timers`] seam, returning
/// [`CANCEL_MESSAGE`] if `signal` is aborted before or after the wait.
///
/// This is the production `sleep` step for [`poll_oauth_device_code_flow`].
/// With [`crate::seams::clock::SystemClock`] it sleeps real time; a
/// [`crate::seams::clock::FakeClock`]-based test supplies its own closure.
pub fn abortable_sleep(
    timers: &dyn Timers,
    delay_ms: u64,
    signal: Option<&AbortSignal>,
) -> Result<(), AuthFlowError> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        return Err(AuthFlowError::new(CANCEL_MESSAGE));
    }
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let fired = pair.clone();
    timers.set_timeout(
        delay_ms,
        Box::new(move || {
            let (lock, cvar) = &*fired;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }),
    );
    let (lock, cvar) = &*pair;
    let mut done = lock.lock().unwrap();
    while !*done {
        done = cvar.wait(done).unwrap();
    }
    drop(done);
    if signal.is_some_and(AbortSignal::is_aborted) {
        return Err(AuthFlowError::new(CANCEL_MESSAGE));
    }
    Ok(())
}

fn initial_interval_ms(interval_seconds: Option<f64>) -> i64 {
    MINIMUM_INTERVAL_MS
        .max((interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS) * 1000.0).floor() as i64)
}

/// Poll an RFC 8628 device-code flow to completion (`device-code.ts:46-98`).
///
/// `poll` performs one poll; `sleep` waits between polls. Returns the completed
/// value, or an [`AuthFlowError`] on failure/cancel/timeout.
pub fn poll_oauth_device_code_flow<T>(
    options: &DeviceCodePollOptions,
    clock: &dyn Clock,
    signal: Option<&AbortSignal>,
    sleep: &mut SleepFn,
    poll: &mut PollFn<T>,
) -> Result<T, AuthFlowError> {
    let deadline: f64 = match options.expires_in_seconds {
        Some(seconds) => clock.now_ms() as f64 + seconds * 1000.0,
        None => f64::INFINITY,
    };
    let mut interval_ms = initial_interval_ms(options.interval_seconds);
    let mut slow_down_responses = 0usize;

    if options.wait_before_first_poll {
        let remaining_ms = deadline - clock.now_ms() as f64;
        if remaining_ms > 0.0 {
            let wait = (interval_ms as f64).min(remaining_ms) as u64;
            sleep(wait)?;
        }
    }

    while (clock.now_ms() as f64) < deadline {
        if signal.is_some_and(AbortSignal::is_aborted) {
            return Err(AuthFlowError::new(CANCEL_MESSAGE));
        }

        match poll()? {
            PollResult::Complete { value } => return Ok(value),
            PollResult::Failed { message } => return Err(AuthFlowError::new(message)),
            PollResult::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                // Trust the server-provided interval when finite and positive
                // (GitHub reports the new minimum); otherwise RFC 8628 §3.5:
                // increase by 5 seconds.
                interval_ms = match interval_seconds {
                    Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                        MINIMUM_INTERVAL_MS.max((seconds * 1000.0).floor() as i64)
                    }
                    _ => MINIMUM_INTERVAL_MS.max(interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS),
                };
            }
            PollResult::Pending => {}
        }

        let remaining_ms = deadline - clock.now_ms() as f64;
        if remaining_ms <= 0.0 {
            break;
        }
        let wait = (interval_ms as f64).min(remaining_ms) as u64;
        sleep(wait)?;
    }

    Err(AuthFlowError::new(if slow_down_responses > 0 {
        SLOW_DOWN_TIMEOUT_MESSAGE
    } else {
        TIMEOUT_MESSAGE
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::clock::FakeClock;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Build a `sleep` closure that advances `clock` and records each delay into
    /// the returned shared log (read after the poller returns).
    #[allow(clippy::type_complexity)]
    fn recording_sleeper(
        clock: &FakeClock,
    ) -> (
        impl FnMut(u64) -> Result<(), AuthFlowError> + '_,
        Rc<RefCell<Vec<u64>>>,
    ) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_handle = log.clone();
        let sleeper = move |ms| {
            log.borrow_mut().push(ms);
            clock.advance(ms);
            Ok(())
        };
        (sleeper, log_handle)
    }

    #[test]
    fn default_interval_is_five_seconds() {
        let clock = FakeClock::new(0);
        let (mut sleep, recorded) = recording_sleeper(&clock);
        let mut polls = 0;
        let mut poll = || {
            polls += 1;
            Ok(if polls >= 2 {
                PollResult::Complete { value: "token" }
            } else {
                PollResult::Pending
            })
        };
        let options = DeviceCodePollOptions {
            interval_seconds: None,
            expires_in_seconds: Some(900.0),
            wait_before_first_poll: false,
        };
        let value =
            poll_oauth_device_code_flow(&options, &clock, None, &mut sleep, &mut poll).unwrap();
        assert_eq!(value, "token");
        assert_eq!(*recorded.borrow(), vec![5000]);
    }

    #[test]
    fn slow_down_without_server_interval_adds_five_seconds() {
        let clock = FakeClock::new(0);
        let (mut sleep, recorded) = recording_sleeper(&clock);
        let mut polls = 0;
        let mut poll = || {
            polls += 1;
            Ok(match polls {
                1 => PollResult::SlowDown {
                    interval_seconds: None,
                },
                _ => PollResult::Complete { value: 42 },
            })
        };
        let options = DeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
            wait_before_first_poll: false,
        };
        let value =
            poll_oauth_device_code_flow(&options, &clock, None, &mut sleep, &mut poll).unwrap();
        assert_eq!(value, 42);
        // First sleep uses the post-slow_down interval: 5000 + 5000.
        assert_eq!(*recorded.borrow(), vec![10_000]);
    }

    #[test]
    fn slow_down_uses_server_interval_when_positive() {
        let clock = FakeClock::new(0);
        let (mut sleep, recorded) = recording_sleeper(&clock);
        let mut polls = 0;
        let mut poll = || {
            polls += 1;
            Ok(match polls {
                1 => PollResult::SlowDown {
                    interval_seconds: Some(7.0),
                },
                _ => PollResult::Complete { value: 1 },
            })
        };
        let options = DeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
            wait_before_first_poll: false,
        };
        poll_oauth_device_code_flow(&options, &clock, None, &mut sleep, &mut poll).unwrap();
        assert_eq!(*recorded.borrow(), vec![7000]);
    }

    #[test]
    fn times_out_with_plain_message() {
        let clock = FakeClock::new(0);
        let (mut sleep, recorded) = recording_sleeper(&clock);
        let mut poll = || Ok(PollResult::<()>::Pending);
        let options = DeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(10.0),
            wait_before_first_poll: false,
        };
        let err =
            poll_oauth_device_code_flow(&options, &clock, None, &mut sleep, &mut poll).unwrap_err();
        assert_eq!(err.message, TIMEOUT_MESSAGE);
        assert_eq!(*recorded.borrow(), vec![5000, 5000]);
    }

    #[test]
    fn times_out_with_slow_down_message() {
        let clock = FakeClock::new(0);
        let (mut sleep, _recorded) = recording_sleeper(&clock);
        let mut poll = || {
            Ok(PollResult::<()>::SlowDown {
                interval_seconds: None,
            })
        };
        let options = DeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(10.0),
            wait_before_first_poll: false,
        };
        let err =
            poll_oauth_device_code_flow(&options, &clock, None, &mut sleep, &mut poll).unwrap_err();
        assert_eq!(err.message, SLOW_DOWN_TIMEOUT_MESSAGE);
    }

    #[test]
    fn failed_poll_propagates_message() {
        let clock = FakeClock::new(0);
        let (mut sleep, _recorded) = recording_sleeper(&clock);
        let mut poll = || {
            Ok(PollResult::<()>::Failed {
                message: "xAI device code expired".into(),
            })
        };
        let options = DeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
            wait_before_first_poll: false,
        };
        let err =
            poll_oauth_device_code_flow(&options, &clock, None, &mut sleep, &mut poll).unwrap_err();
        assert_eq!(err.message, "xAI device code expired");
    }

    #[test]
    fn aborted_signal_cancels_before_poll() {
        let clock = FakeClock::new(0);
        let (mut sleep, _recorded) = recording_sleeper(&clock);
        let mut poll = || Ok(PollResult::<()>::Pending);
        let signal = AbortSignal::aborted();
        let options = DeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
            wait_before_first_poll: false,
        };
        let err =
            poll_oauth_device_code_flow(&options, &clock, Some(&signal), &mut sleep, &mut poll)
                .unwrap_err();
        assert_eq!(err.message, CANCEL_MESSAGE);
    }

    #[test]
    fn wait_before_first_poll_sleeps_one_interval_first() {
        let clock = FakeClock::new(0);
        let (mut sleep, recorded) = recording_sleeper(&clock);
        let mut poll = || Ok(PollResult::Complete { value: 0 });
        let options = DeviceCodePollOptions {
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(900.0),
            wait_before_first_poll: true,
        };
        poll_oauth_device_code_flow(&options, &clock, None, &mut sleep, &mut poll).unwrap();
        // Slept once before polling, then completed on the first poll.
        assert_eq!(*recorded.borrow(), vec![5000]);
    }

    #[test]
    fn abortable_sleep_blocks_then_returns_via_timers() {
        // SystemClock fires the timer on a real (short) delay.
        let clock = crate::seams::clock::SystemClock::new();
        abortable_sleep(&clock, 1, None).unwrap();
    }

    #[test]
    fn abortable_sleep_cancels_when_pre_aborted() {
        let clock = crate::seams::clock::SystemClock::new();
        let signal = AbortSignal::aborted();
        let err = abortable_sleep(&clock, 10_000, Some(&signal)).unwrap_err();
        assert_eq!(err.message, CANCEL_MESSAGE);
    }
}

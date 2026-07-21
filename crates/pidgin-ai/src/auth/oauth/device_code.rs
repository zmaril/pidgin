// straitjacket-allow-file:duplication — the inline `#[cfg(test)] mod tests`
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

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// The result of one JS-side poll, fed back into [`DeviceCodePollMachine::advance`].
///
/// This is the boundary-serialized mirror of pi's `OAuthDeviceCodePollResult`
/// (`device-code.ts:11-16`): the napi shim runs the caller's `poll()` callback,
/// maps its `{ status }` object to this input, and re-enters the machine.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DevicePollInput {
    /// Authorization still pending (`device-code.ts:12`).
    Pending,
    /// The server asked the client to slow down, with an optional new interval
    /// (`device-code.ts:13`).
    SlowDown {
        /// The server-provided interval, in seconds, if any.
        #[serde(default)]
        interval_seconds: Option<f64>,
    },
    /// The flow failed with this message (`device-code.ts:14`).
    Failed {
        /// The failure message.
        message: String,
    },
    /// The flow completed with this value (`device-code.ts:16`).
    Complete {
        /// The completed value (e.g. tokens), passed through opaquely.
        value: Value,
    },
    /// The caller's abort signal fired while the shim was polling or waiting;
    /// the machine surfaces [`CANCEL_MESSAGE`] (`device-code.ts:65-66,94`).
    Aborted,
}

/// One action the machine yields for the napi shim to perform.
///
/// This is the device-code analogue of [`super::flow::Step`]: Rust stays pure
/// and yields the next action, the shim performs the effect (poll callback or
/// `setTimeout` wait) and re-enters with a [`DevicePollInput`].
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DevicePollStep {
    /// Run the caller's `poll()` now, then re-enter with the result.
    Poll,
    /// Sleep `delay_ms`, then run the caller's `poll()`, then re-enter.
    Wait {
        /// The delay before the next poll, in ms.
        delay_ms: u64,
    },
    /// Terminal: the flow completed with this value.
    Done {
        /// The completed value, passed back to the caller.
        value: Value,
    },
    /// Terminal: the flow failed / cancelled / timed out with this message.
    Error {
        /// The failure message.
        message: String,
    },
}

/// A JS-drivable state machine over the same poll loop as
/// [`poll_oauth_device_code_flow`] (`device-code.ts:46-98`).
///
/// The pure Rust driver keeps the loop in Rust; this machine turns it inside
/// out so the napi shim owns the effects (the `poll()` callback and the fake
/// timers) exactly as pi's tests expect. [`start`](Self::start) yields the first
/// step and [`advance`](Self::advance) consumes each poll result and yields the
/// next, mirroring [`super::flow::OAuthFlowMachine`].
///
/// Deadline handling follows behavior (b): the deadline is checked *before*
/// scheduling the next wait (`device-code.ts:89-92`), so the machine never
/// schedules a poll it knows would land at or past the deadline.
pub struct DeviceCodePollMachine {
    expires_in_seconds: Option<f64>,
    deadline: i64,
    interval_ms: i64,
    slow_down_responses: usize,
    wait_before_first_poll: bool,
    started: bool,
}

impl DeviceCodePollMachine {
    /// Build a machine from the same [`DeviceCodePollOptions`] the pure driver
    /// takes. The deadline is resolved lazily in [`start`](Self::start) from the
    /// injected start time (`device-code.ts:47-50`).
    pub fn new(options: DeviceCodePollOptions) -> Self {
        Self {
            expires_in_seconds: options.expires_in_seconds,
            deadline: i64::MAX,
            interval_ms: initial_interval_ms(options.interval_seconds),
            slow_down_responses: 0,
            wait_before_first_poll: options.wait_before_first_poll,
            started: false,
        }
    }

    /// Yield the first step (`device-code.ts:47-62`).
    ///
    /// Computes the deadline from `now_ms`, then either waits one interval before
    /// the first poll ([`DevicePollStep::Wait`]) or polls immediately
    /// ([`DevicePollStep::Poll`]). If waiting and the deadline has already passed,
    /// yields [`DevicePollStep::Error`] with [`TIMEOUT_MESSAGE`] rather than a poll.
    pub fn start(&mut self, now_ms: i64) -> DevicePollStep {
        self.started = true;
        self.deadline = match self.expires_in_seconds {
            Some(seconds) => now_ms.saturating_add((seconds * 1000.0).floor() as i64),
            None => i64::MAX,
        };

        if self.wait_before_first_poll {
            let remaining = self.deadline - now_ms;
            if remaining <= 0 {
                return DevicePollStep::Error {
                    message: TIMEOUT_MESSAGE.to_string(),
                };
            }
            return DevicePollStep::Wait {
                delay_ms: self.interval_ms.min(remaining) as u64,
            };
        }

        DevicePollStep::Poll
    }

    /// Consume one poll result and yield the next step (`device-code.ts:64-97`).
    ///
    /// `Complete`/`Failed`/`Aborted` are terminal; `Pending`/`SlowDown` update
    /// the interval and re-check the deadline before scheduling the next wait.
    pub fn advance(&mut self, input: DevicePollInput, now_ms: i64) -> DevicePollStep {
        debug_assert!(self.started, "advance called before start");
        match input {
            DevicePollInput::Complete { value } => DevicePollStep::Done { value },
            DevicePollInput::Failed { message } => DevicePollStep::Error { message },
            DevicePollInput::Aborted => DevicePollStep::Error {
                message: CANCEL_MESSAGE.to_string(),
            },
            DevicePollInput::Pending => self.wait_or_timeout(now_ms),
            DevicePollInput::SlowDown { interval_seconds } => {
                self.slow_down_responses += 1;
                // Same rule as the pure driver: trust a finite, positive
                // server interval; otherwise RFC 8628 §3.5 add 5 seconds
                // (`device-code.ts:81-86`).
                self.interval_ms = match interval_seconds {
                    Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                        MINIMUM_INTERVAL_MS.max((seconds * 1000.0).floor() as i64)
                    }
                    _ => {
                        MINIMUM_INTERVAL_MS.max(self.interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS)
                    }
                };
                self.wait_or_timeout(now_ms)
            }
        }
    }

    /// Behavior-(b) deadline pre-check (`device-code.ts:89-94`): if the deadline
    /// has passed, or scheduling the next wait would land at/after it, yield the
    /// terminal timeout error (WSL wording once any `slow_down` was seen);
    /// otherwise yield the next [`DevicePollStep::Wait`].
    fn wait_or_timeout(&self, now_ms: i64) -> DevicePollStep {
        let remaining = self.deadline - now_ms;
        if remaining <= 0 || now_ms.saturating_add(self.interval_ms) >= self.deadline {
            return DevicePollStep::Error {
                message: if self.slow_down_responses > 0 {
                    SLOW_DOWN_TIMEOUT_MESSAGE.to_string()
                } else {
                    TIMEOUT_MESSAGE.to_string()
                },
            };
        }
        DevicePollStep::Wait {
            delay_ms: self.interval_ms.min(remaining) as u64,
        }
    }
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

    // ---- DeviceCodePollMachine (JS-drivable wrapper) ----
    //
    // These mirror `test/oauth-device-code.test.ts` at pinned commit `3da591ab`,
    // driving the machine the way the napi shim does: `start(now)` then
    // `advance(pollResult, now)` per poll, with the shim owning the fake clock.

    use serde_json::json;

    /// Assert a step is `Wait { delay_ms }` with the expected delay.
    fn assert_wait(step: &DevicePollStep, expected_ms: u64) {
        match step {
            DevicePollStep::Wait { delay_ms } => assert_eq!(*delay_ms, expected_ms),
            other => panic!("expected Wait {{ {expected_ms} }}, got {other:?}"),
        }
    }

    /// Assert a step is a terminal `Error { message }`.
    fn assert_error(step: &DevicePollStep, expected: &str) {
        match step {
            DevicePollStep::Error { message } => assert_eq!(message, expected),
            other => panic!("expected Error({expected:?}), got {other:?}"),
        }
    }

    fn machine(
        interval_seconds: Option<f64>,
        expires_in_seconds: Option<f64>,
        wait_before_first_poll: bool,
    ) -> DeviceCodePollMachine {
        DeviceCodePollMachine::new(DeviceCodePollOptions {
            interval_seconds,
            expires_in_seconds,
            wait_before_first_poll,
        })
    }

    /// `test:9-39` — polls immediately, waits one interval on `pending`, then
    /// returns the completed value on the next poll.
    #[test]
    fn machine_polls_immediately_then_completes() {
        let mut m = machine(Some(2.0), Some(30.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        // First poll at t=0 is pending → wait one 2s interval.
        assert_wait(&m.advance(DevicePollInput::Pending, 0), 2000);
        // Second poll at t=2000 completes.
        match m.advance(
            DevicePollInput::Complete {
                value: json!("token"),
            },
            2000,
        ) {
            DevicePollStep::Done { value } => assert_eq!(value, json!("token")),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    /// `test:41-62` — `waitBeforeFirstPoll` sleeps one interval before the first
    /// poll, which then completes.
    #[test]
    fn machine_waits_before_first_poll() {
        let mut m = machine(Some(2.0), Some(30.0), true);
        assert_wait(&m.start(0), 2000);
        match m.advance(
            DevicePollInput::Complete {
                value: json!("token"),
            },
            2000,
        ) {
            DevicePollStep::Done { value } => assert_eq!(value, json!("token")),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    /// `test:64-91` — `slow_down` without a server interval bumps the interval by
    /// 5s (2000 + 5000 = 7000) before the next wait.
    #[test]
    fn machine_slow_down_without_interval_adds_five_seconds() {
        let mut m = machine(Some(2.0), Some(900.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        assert_wait(
            &m.advance(
                DevicePollInput::SlowDown {
                    interval_seconds: None,
                },
                0,
            ),
            7000,
        );
        match m.advance(
            DevicePollInput::Complete {
                value: json!("token"),
            },
            7000,
        ) {
            DevicePollStep::Done { value } => assert_eq!(value, json!("token")),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    /// `test:93-123` — `slow_down` with a server interval of 30s adopts it
    /// verbatim (30000ms) for the next wait.
    #[test]
    fn machine_slow_down_honors_server_interval() {
        let mut m = machine(Some(2.0), Some(900.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        assert_wait(
            &m.advance(
                DevicePollInput::SlowDown {
                    interval_seconds: Some(30.0),
                },
                0,
            ),
            30000,
        );
        match m.advance(
            DevicePollInput::Complete {
                value: json!("token"),
            },
            30000,
        ) {
            DevicePollStep::Done { value } => assert_eq!(value, json!("token")),
            other => panic!("expected Done, got {other:?}"),
        }
    }

    /// `test:125-138` — an abort during an in-flight wait surfaces as
    /// `Aborted` → `Error("Login cancelled")`.
    #[test]
    fn machine_abort_cancels_in_flight_wait() {
        let mut m = machine(Some(5.0), Some(30.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        // Pending schedules a wait; the abort fires mid-wait.
        assert_wait(&m.advance(DevicePollInput::Pending, 0), 5000);
        assert_error(&m.advance(DevicePollInput::Aborted, 0), CANCEL_MESSAGE);
    }

    /// `device-code.ts:70-74` — a `complete` value is returned; a `failed`
    /// status throws its message.
    #[test]
    fn machine_failed_propagates_message() {
        let mut m = machine(Some(5.0), Some(900.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        assert_error(
            &m.advance(
                DevicePollInput::Failed {
                    message: "xAI device code expired".to_string(),
                },
                0,
            ),
            "xAI device code expired",
        );
    }

    /// `device-code.ts:89-97` — behavior (b): when the next poll would land at
    /// the deadline, the machine times out with the plain message and schedules
    /// no trailing poll. interval 5s, expiry 10s: wait once, then time out.
    #[test]
    fn machine_times_out_with_plain_message() {
        let mut m = machine(Some(5.0), Some(10.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        assert_wait(&m.advance(DevicePollInput::Pending, 0), 5000);
        // At t=5000, now + interval (10000) == deadline → timeout, no poll.
        assert_error(&m.advance(DevicePollInput::Pending, 5000), TIMEOUT_MESSAGE);
    }

    /// `device-code.ts:96-97` — after a `slow_down`, the timeout uses the WSL /
    /// VM clock-drift wording. interval 5s + slow_down → 10s ≥ 10s expiry.
    #[test]
    fn machine_times_out_with_slow_down_message() {
        let mut m = machine(Some(5.0), Some(10.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        assert_error(
            &m.advance(
                DevicePollInput::SlowDown {
                    interval_seconds: None,
                },
                0,
            ),
            SLOW_DOWN_TIMEOUT_MESSAGE,
        );
    }

    /// `device-code.ts:51-54` — with no `intervalSeconds`, the initial interval
    /// defaults to 5s (`DEFAULT_POLL_INTERVAL_SECONDS`).
    #[test]
    fn machine_default_interval_is_five_seconds() {
        let mut m = machine(None, Some(900.0), false);
        assert!(matches!(m.start(0), DevicePollStep::Poll));
        assert_wait(&m.advance(DevicePollInput::Pending, 0), 5000);
    }

    /// Deserialization of `DevicePollInput` matches the shim's `{ status }`
    /// payloads (`device-code.ts:11-16`), including snake_case `slow_down` and a
    /// defaulted `interval_seconds`.
    #[test]
    fn device_poll_input_deserializes_status_payloads() {
        let pending: DevicePollInput =
            serde_json::from_value(json!({"status": "pending"})).unwrap();
        assert!(matches!(pending, DevicePollInput::Pending));

        let slow: DevicePollInput = serde_json::from_value(json!({"status": "slow_down"})).unwrap();
        assert!(matches!(
            slow,
            DevicePollInput::SlowDown {
                interval_seconds: None
            }
        ));

        let slow_with: DevicePollInput =
            serde_json::from_value(json!({"status": "slow_down", "interval_seconds": 7.5}))
                .unwrap();
        assert!(matches!(
            slow_with,
            DevicePollInput::SlowDown {
                interval_seconds: Some(v)
            } if (v - 7.5).abs() < f64::EPSILON
        ));

        let complete: DevicePollInput =
            serde_json::from_value(json!({"status": "complete", "value": {"token": "x"}})).unwrap();
        assert!(matches!(complete, DevicePollInput::Complete { .. }));

        let aborted: DevicePollInput =
            serde_json::from_value(json!({"status": "aborted"})).unwrap();
        assert!(matches!(aborted, DevicePollInput::Aborted));
    }

    /// Serialization of `DevicePollStep` matches the `{ kind }` shape the shim
    /// dispatches on.
    #[test]
    fn device_poll_step_serializes_kind_payloads() {
        assert_eq!(
            serde_json::to_value(DevicePollStep::Poll).unwrap(),
            json!({"kind": "poll"})
        );
        assert_eq!(
            serde_json::to_value(DevicePollStep::Wait { delay_ms: 2000 }).unwrap(),
            json!({"kind": "wait", "delay_ms": 2000})
        );
        assert_eq!(
            serde_json::to_value(DevicePollStep::Done {
                value: json!("token")
            })
            .unwrap(),
            json!({"kind": "done", "value": "token"})
        );
        assert_eq!(
            serde_json::to_value(DevicePollStep::Error {
                message: "boom".to_string()
            })
            .unwrap(),
            json!({"kind": "error", "message": "boom"})
        );
    }
}

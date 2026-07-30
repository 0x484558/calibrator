use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::System::EventLog::{
    DeregisterEventSource, EVENTLOG_ERROR_TYPE, RegisterEventSourceW, ReportEventW,
};
use windows::Win32::System::Threading::{
    CancelWaitableTimer, CreateEventW, CreateWaitableTimerExW, INFINITE, ResetEvent,
    SetWaitableTimerEx, TIMER_ALL_ACCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, TranslateMessage, WM_QUIT,
};
use windows::core::{HSTRING, PCWSTR};

use crate::calibration::{encode_sdr_white_level, hdr_balance};
use crate::display::{ApplyOutcome, apply_sdr_white_level, probe_sdr_white_level};
use crate::gpu_reset::GpuResetSubscription;
use crate::power_policy::{apply_process_policy, apply_to_current_event_thread};
use crate::probe::{ProbeRecorder, RunConfig};
use crate::session::ensure_interactive_window_station;
use crate::single_instance::{Acquire, InstanceGuard};
use crate::tray::{Tray, WindowSignals};
use crate::win32::{Error, Stage};
use crate::wmi_monitor::{SnapshotReader, WmiWatcher};

const DEBOUNCE_100NS: i64 = 1_500_000;
const DEBOUNCE_TOLERANCE_MS: u32 = 50;

/// Back-off before writing an error to the Event Log: 5 s in 100 ns units.
/// Coalescing tolerance is generous because diagnostic latency is irrelevant.
const LOG_FLUSH_100NS: i64 = 50_000_000;
const LOG_FLUSH_TOLERANCE_MS: u32 = 2_000;

/// Lightweight stateful diagnostic accumulator.
///
/// Retains the most recent failing reconcile description (if any) across multiple
/// rapid reconcile calls. A single Event Log entry is written only when the
/// log-flush timer fires, so a busy brightness-adjustment burst produces at most
/// one entry per `LOG_FLUSH_100NS` interval. A successful reconcile before the
/// timer fires clears the pending error and suppresses the entry entirely.
struct Diagnostics {
    /// Human-readable description of the most recent failure, or `None` if the
    /// last outcome was a success (or no error has been seen yet).
    pending: Option<String>,
    /// Whether the log-flush timer is currently armed.
    flush_armed: bool,
}

impl Diagnostics {
    const fn new() -> Self {
        Self { pending: None, flush_armed: false }
    }

    /// Record the outcome of one reconcile attempt.
    ///
    /// Returns `true` when a log-flush timer should be (re-)armed because a new
    /// failure arrived and the timer is not already running.
    fn record(&mut self, outcome: &ApplyOutcome) -> bool {
        if let Some(desc) = outcome_description(outcome) {
            self.pending = Some(desc);
            // Arm only if not already counting down; existing timer will
            // flush whatever error is current when it fires.
            let should_arm = !self.flush_armed;
            self.flush_armed = true;
            should_arm
        } else {
            // Recovery: cancel any pending error so the timer (if armed)
            // will find nothing to log when it fires.
            self.pending = None;
            false
        }
    }

    /// Called when the log-flush timer fires. Writes one Event Log entry if a
    /// failure is still pending, then resets state.
    fn flush(&mut self) {
        self.flush_armed = false;
        let Some(ref desc) = self.pending else {
            return;
        };
        write_event_log(desc);
        self.pending = None;
    }
}

/// Returns a short description of non-success outcomes, or `None` for outcomes
/// that are either normal operation or indicate no HDR target is present.
fn outcome_description(outcome: &ApplyOutcome) -> Option<String> {
    match outcome {
        // "No HDR target" is normal on non-HDR desktops; suppress.
        ApplyOutcome::AppliedAndVerified | ApplyOutcome::AlreadyCorrect | ApplyOutcome::NoHdrTarget => None,
        ApplyOutcome::AmbiguousInternalTargets => {
            Some("reconcile: ambiguous internal HDR targets".to_owned())
        }
        ApplyOutcome::TopologyUnavailable => {
            Some("reconcile: display topology query failed".to_owned())
        }
        ApplyOutcome::InspectionUnavailable => {
            Some("reconcile: HDR target inspection failed".to_owned())
        }
        ApplyOutcome::SetFailed(code) => {
            Some(format!("reconcile: DisplayConfigSetDeviceInfo failed (HRESULT {code:#010x})"))
        }
        ApplyOutcome::VerificationFailed { expected, observed } => {
            let obs = observed.map_or_else(|| "unavailable".to_owned(), |v| format!("{v}"));
            Some(format!(
                "reconcile: verification failed (expected SDR white level {expected}, read {obs})"
            ))
        }
    }
}

/// Writes a single error entry to the Windows Application event log.
///
/// Registration is intentionally transient (open/report/close) because this
/// path is rarely taken and holding a permanent source handle buys nothing.
/// If the registration itself fails the error is silently swallowed — diagnostics
/// must not cascade into further failures.
fn write_event_log(message: &str) {
    // SAFETY: literal wide string; source name does not need to match a registry
    // message-DLL entry for raw string inserts (EVENTLOG_ERROR_TYPE + %%message).
    let source = windows::core::w!("calibrator");
    let Ok(handle) = (unsafe { RegisterEventSourceW(PCWSTR::null(), source) }) else {
        return;
    };
    let wide: HSTRING = message.into();
    let strings = [PCWSTR(wide.as_ptr())];
    // Event ID 1, category 0, no binary data.
    // SAFETY: handle is valid; strings slice and its pointer outlive the call.
    let _ = unsafe {
        ReportEventW(
            handle,
            EVENTLOG_ERROR_TYPE,
            0,
            1,
            None,
            1,
            Some(&strings),
            None,
        )
    };
    // SAFETY: handle was returned by RegisterEventSourceW and is used exactly once.
    let _ = unsafe { DeregisterEventSource(handle) };
}

pub(crate) fn run() -> Result<(), Error> {
    let config = RunConfig::from_environment()?;
    ensure_interactive_window_station()?;

    let instance = match InstanceGuard::acquire(config.adjustment_enabled())? {
        Acquire::Acquired(instance) => instance,
        Acquire::AlreadyRunning => return Ok(()),
    };
    apply_process_policy()?;
    apply_to_current_event_thread()?;

    let brightness_signal = OwnedHandle::manual_reset_event()?;
    let reconcile_signal = OwnedHandle::manual_reset_event()?;
    let exit_signal = OwnedHandle::manual_reset_event()?;
    let wmi_failure_signal = OwnedHandle::manual_reset_event()?;
    let gpu_reset_signal = OwnedHandle::manual_reset_event()?;
    let probe_signal = OwnedHandle::manual_reset_event()?;
    let debounce_timer = OwnedHandle::waitable_timer()?;
    let log_flush_timer = OwnedHandle::waitable_timer()?;

    let mut probe = match config {
        RunConfig::Adjust => None,
        RunConfig::Probe(config) => Some(ProbeRecorder::create(config)?),
    };
    let mut tray = Tray::create(
        WindowSignals {
            reconcile: reconcile_signal.get(),
            probe: probe_signal.get(),
            exit: exit_signal.get(),
        },
        probe.is_some(),
    )?;
    if let Some(recorder) = probe.as_ref() {
        tray.set_probe_position(recorder.next_position());
    }

    // Probe mode is operator-driven and must remain usable even where WMI brightness or the
    // System event channel is absent. It creates neither consumer and can never adjust.
    let snapshot = if probe.is_none() {
        // Creating the first WMI connection establishes process COM security before the listener
        // creates an asynchronous callback sink on its own MTA thread.
        Some(SnapshotReader::create()?)
    } else {
        None
    };
    let _watcher = if probe.is_none() {
        Some(WmiWatcher::start(
            brightness_signal.get(),
            wmi_failure_signal.get(),
        )?)
    } else {
        None
    };
    let gpu_reset = if probe.is_none() {
        GpuResetSubscription::create(gpu_reset_signal.get()).ok()
    } else {
        None
    };

    let mut diag = Diagnostics::new();
    if let Some(snapshot) = snapshot.as_ref() {
        reconcile(snapshot, &mut diag, log_flush_timer.get())?;
    }
    let loop_result = run_event_loop(
        snapshot.as_ref(),
        WaitHandles {
            debounce_timer: debounce_timer.get(),
            log_flush_timer: log_flush_timer.get(),
            brightness_signal: brightness_signal.get(),
            reconcile_signal: reconcile_signal.get(),
            exit_signal: exit_signal.get(),
            wmi_failure_signal: wmi_failure_signal.get(),
            gpu_reset_signal: gpu_reset_signal.get(),
            probe_signal: probe_signal.get(),
            refresh_signal: instance.refresh_event(),
        },
        gpu_reset.as_ref(),
        &mut tray,
        probe.as_mut(),
        &mut diag,
    );
    if let Some(recorder) = probe.as_mut() {
        recorder.finish()?;
    }
    loop_result
}

fn run_event_loop(
    snapshot: Option<&SnapshotReader>,
    wait_handles: WaitHandles,
    gpu_reset: Option<&GpuResetSubscription>,
    tray: &mut Tray,
    mut probe: Option<&mut ProbeRecorder>,
    diag: &mut Diagnostics,
) -> Result<(), Error> {
    let handles = wait_handles.as_array();
    let mut debounce_armed = false;
    // log_flush_armed state is tracked inside `diag`; the timer handle lives in wait_handles.

    loop {
        // SAFETY: all handles remain live for the loop; no timeout means zero periodic wakeups.
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if wait == WAIT_FAILED {
            return Err(Error::last(Stage::EventWait));
        }

        let index = wait.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        match index {
            0 => {
                let was_armed = debounce_armed;
                debounce_armed = false;
                // CancelWaitableTimer can race a timer that has already become signaled. The
                // generation bit suppresses that stale completion after lifecycle/manual refresh.
                if was_armed && let Some(snapshot) = snapshot {
                    reconcile(snapshot, diag, wait_handles.log_flush_timer)?;
                }
            }
            1 => {
                // Log-flush timer fired: emit one Event Log entry for any accumulated error.
                diag.flush();
            }
            2 => {
                // SAFETY: owned manual-reset event.
                unsafe { ResetEvent(wait_handles.brightness_signal) }
                    .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                if snapshot.is_some() {
                    arm_debounce(wait_handles.debounce_timer)?;
                    debounce_armed = true;
                }
            }
            3 => {
                // Immediate lifecycle repair supersedes any pending brightness debounce because
                // it reads authoritative current brightness itself.
                unsafe { ResetEvent(wait_handles.reconcile_signal) }
                    .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                unsafe { ResetEvent(wait_handles.brightness_signal) }
                    .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                if debounce_armed {
                    // SAFETY: owned waitable timer, currently armed.
                    unsafe { CancelWaitableTimer(wait_handles.debounce_timer) }
                        .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                    debounce_armed = false;
                }
                if let Some(snapshot) = snapshot {
                    reconcile(snapshot, diag, wait_handles.log_flush_timer)?;
                }
            }
            4 => return Ok(()),
            5 => return Err(Error::condition(Stage::WmiSubscription)),
            6 => {
                // SAFETY: owned manual-reset event used by the pull subscription.
                unsafe { ResetEvent(wait_handles.gpu_reset_signal) }
                    .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                if let Some(gpu_reset) = gpu_reset
                    && gpu_reset.drain()?
                    && let Some(snapshot) = snapshot
                {
                    reconcile(snapshot, diag, wait_handles.log_flush_timer)?;
                }
            }
            7 => {
                // SAFETY: owned manual-reset event.
                unsafe { ResetEvent(wait_handles.probe_signal) }
                    .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                if let Some(recorder) = probe.as_deref_mut() {
                    let outcome = probe_sdr_white_level();
                    recorder.record(outcome)?;
                    tray.set_probe_position(recorder.next_position());
                }
            }
            8 => {
                // The auto-reset event was consumed by the wait. A normal secondary launch
                // signals it and exits; reconcile in the established initialized process.
                if debounce_armed {
                    // The explicit refresh reads current brightness now; a pending debounce would
                    // only duplicate the same derivation afterward.
                    unsafe { CancelWaitableTimer(wait_handles.debounce_timer) }
                        .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                    debounce_armed = false;
                }
                unsafe { ResetEvent(wait_handles.brightness_signal) }
                    .map_err(|ref error| Error::windows(Stage::EventWait, error))?;
                if let Some(snapshot) = snapshot {
                    reconcile(snapshot, diag, wait_handles.log_flush_timer)?;
                }
            }
            9 => {
                if !drain_messages()? {
                    return Ok(());
                }
            }
            _ => return Err(Error::condition(Stage::EventWait)),
        }
    }
}

#[derive(Clone, Copy)]
struct WaitHandles {
    debounce_timer: HANDLE,
    log_flush_timer: HANDLE,
    brightness_signal: HANDLE,
    reconcile_signal: HANDLE,
    exit_signal: HANDLE,
    wmi_failure_signal: HANDLE,
    gpu_reset_signal: HANDLE,
    probe_signal: HANDLE,
    refresh_signal: HANDLE,
}

impl WaitHandles {
    const fn as_array(self) -> [HANDLE; 9] {
        [
            self.debounce_timer,
            self.log_flush_timer,
            self.brightness_signal,
            self.reconcile_signal,
            self.exit_signal,
            self.wmi_failure_signal,
            self.gpu_reset_signal,
            self.probe_signal,
            self.refresh_signal,
        ]
    }
}

fn arm_debounce(timer: HANDLE) -> Result<(), Error> {
    let due_time = -DEBOUNCE_100NS;
    // SetWaitableTimerEx replaces any previous schedule for this timer. Period zero is an actual
    // one-shot; no high-resolution flag is used and tolerance permits timer coalescing.
    //
    // SAFETY: timer is live; due_time remains valid for the call; no APC/wake context is supplied.
    unsafe { SetWaitableTimerEx(timer, &raw const due_time, 0, None, None, None, DEBOUNCE_TOLERANCE_MS) }
        .map_err(|ref error| Error::windows(Stage::CreateDebounceTimer, error))
}

fn arm_log_flush(timer: HANDLE) -> Result<(), Error> {
    let due_time = -LOG_FLUSH_100NS;
    // SAFETY: same contract as arm_debounce.
    unsafe { SetWaitableTimerEx(timer, &raw const due_time, 0, None, None, None, LOG_FLUSH_TOLERANCE_MS) }
        .map_err(|ref error| Error::windows(Stage::CreateDebounceTimer, error))
}

fn drain_messages() -> Result<bool, Error> {
    let mut message = MSG::default();
    loop {
        // SAFETY: message is writable; all thread messages are requested and removed.
        if !unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            return Ok(true);
        }
        if message.message == WM_QUIT {
            return Ok(false);
        }
        // SAFETY: PeekMessageW initialized the message.
        unsafe {
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

fn reconcile(
    snapshot: &SnapshotReader,
    diag: &mut Diagnostics,
    log_flush_timer: HANDLE,
) -> Result<(), Error> {
    let Some(brightness) = snapshot.current_brightness() else {
        return Ok(());
    };
    let balance = hdr_balance(brightness);
    let outcome = apply_sdr_white_level(encode_sdr_white_level(balance));

    // Record the outcome. If a new error surfaces and no flush is already
    // scheduled, arm the back-off timer so we emit at most one Event Log
    // entry per quiet interval rather than flooding on every brightness event.
    if diag.record(&outcome) {
        arm_log_flush(log_flush_timer)?;
    }
    Ok(())
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn manual_reset_event() -> Result<Self, Error> {
        // SAFETY: default security, manual reset, initially nonsignaled, unnamed.
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map(Self)
            .map_err(|ref error| Error::windows(Stage::CreateEvent, error))
    }

    fn waitable_timer() -> Result<Self, Error> {
        // Flags zero creates a normal-resolution auto-reset timer. It is never configured to wake
        // the system and its period is always zero.
        //
        // SAFETY: default security and unnamed timer.
        unsafe { CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0) }
            .map(Self)
            .map_err(|ref error| Error::windows(Stage::CreateDebounceTimer, error))
    }

    #[must_use]
    const fn get(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: owned live handle, closed once after all consumers have stopped.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

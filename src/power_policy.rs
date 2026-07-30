use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, PROCESS_MODE_BACKGROUND_BEGIN,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
    ProcessPowerThrottling, SetPriorityClass, SetProcessInformation, SetThreadInformation,
    THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
    THREAD_POWER_THROTTLING_STATE, ThreadPowerThrottling,
};

use crate::win32::{Error, Stage};

const PROCESS_THROTTLING_FLAGS: u32 =
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION;

pub(crate) fn apply_process_policy() -> Result<(), Error> {
    let throttling = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_THROTTLING_FLAGS,
        StateMask: PROCESS_THROTTLING_FLAGS,
    };

    // SAFETY: current-process pseudo-handle is valid; packet matches the selected information
    // class and remains alive for the call.
    unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const throttling).cast::<c_void>(),
            u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>()).unwrap_or(0),
        )
    }
    .map_err(|ref error| Error::windows(Stage::ProcessPowerPolicy, error))?;

    // SAFETY: current-process pseudo-handle is valid. Background mode intentionally remains
    // active until process exit.
    unsafe { SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN) }
        .map_err(|ref error| Error::windows(Stage::ProcessBackgroundMode, error))
}

pub(crate) fn apply_to_current_event_thread() -> Result<(), Error> {
    let throttling = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
    };

    // SAFETY: current-thread pseudo-handle is valid; packet matches the selected information
    // class and remains alive for the call.
    unsafe {
        SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            (&raw const throttling).cast::<c_void>(),
            u32::try_from(size_of::<THREAD_POWER_THROTTLING_STATE>()).unwrap_or(0),
        )
    }
    .map_err(|ref error| Error::windows(Stage::EventThreadPowerPolicy, error))
}

#[cfg(test)]
mod tests {
    use super::PROCESS_THROTTLING_FLAGS;
    use windows::Win32::System::Threading::{
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
    };

    #[test]
    fn process_policy_controls_and_enables_both_required_mechanisms() {
        assert_ne!(
            PROCESS_THROTTLING_FLAGS & PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            0
        );
        assert_ne!(
            PROCESS_THROTTLING_FLAGS & PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            0
        );
        assert_eq!(PROCESS_THROTTLING_FLAGS.count_ones(), 2);
    }
}

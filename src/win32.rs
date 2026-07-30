use windows::Win32::Foundation::GetLastError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Error {
    stage: Stage,
    code: i32,
}

impl Error {
    #[must_use]
    pub(crate) fn last(stage: Stage) -> Self {
        // SAFETY: callers invoke this immediately after a failing Win32 call.
        let code = unsafe { GetLastError().0.cast_signed() };
        Self { stage, code }
    }

    #[must_use]
    pub(crate) fn windows(stage: Stage, error: &windows::core::Error) -> Self {
        Self {
            stage,
            code: error.code().0,
        }
    }

    #[must_use]
    pub(crate) const fn condition(stage: Stage) -> Self {
        Self { stage, code: 0 }
    }

    #[must_use]
    pub(crate) const fn exit_code(self) -> u8 {
        self.stage as u8 * 2 + (self.code != 0) as u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Stage {
    InteractiveSession = 1,
    ProcessPowerPolicy = 2,
    ProcessBackgroundMode = 3,
    EventThreadPowerPolicy = 4,
    SingleInstance = 5,
    RegisterWindowClass = 6,
    CreateMessageWindow = 7,
    CreateEvent = 8,
    CreateDebounceTimer = 9,
    RegisterPowerNotifications = 10,
    RegisterSessionNotifications = 11,
    RegisterDisplayNotifications = 12,
    WmiSnapshot = 13,
    WmiSubscription = 14,
    EventWait = 15,
    GpuResetSubscription = 16,
    ProbeConfiguration = 17,
    ProbeLog = 18,
}

#[cfg(test)]
mod tests {
    use super::{Error, Stage};

    #[test]
    fn each_failure_stage_has_a_stable_nonzero_exit_code() {
        let stages = [
            Stage::InteractiveSession,
            Stage::ProcessPowerPolicy,
            Stage::ProcessBackgroundMode,
            Stage::EventThreadPowerPolicy,
            Stage::SingleInstance,
            Stage::RegisterWindowClass,
            Stage::CreateMessageWindow,
            Stage::CreateEvent,
            Stage::CreateDebounceTimer,
            Stage::RegisterPowerNotifications,
            Stage::RegisterSessionNotifications,
            Stage::RegisterDisplayNotifications,
            Stage::WmiSnapshot,
            Stage::WmiSubscription,
            Stage::EventWait,
            Stage::GpuResetSubscription,
            Stage::ProbeConfiguration,
            Stage::ProbeLog,
        ];

        for (index, stage) in stages.iter().copied().enumerate() {
            let code = i32::try_from(index).unwrap();
            let error = Error { stage, code };
            assert_ne!(error.exit_code(), 0);
            assert_eq!(error.stage, stage);
            assert_eq!(error.code, code);
            assert_eq!(error.exit_code() / 2, stage as u8);
        }
    }
}

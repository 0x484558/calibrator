use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent};
use windows::core::w;

use crate::win32::{Error, Stage};

fn refresh_event_manual_reset() -> bool {
    false
}

pub(crate) enum Acquire {
    Acquired(InstanceGuard),
    AlreadyRunning,
}

pub(crate) struct InstanceGuard {
    mutex: HANDLE,
    refresh_event: HANDLE,
}

impl InstanceGuard {
    
    pub(crate) fn acquire(request_refresh: bool) -> Result<Acquire, Error> {
        // Create/open the shared event before the mutex. This closes the startup race: whichever
        // process becomes primary already has a live event object that a loser can signal.
        // Auto reset consumes one pending refresh at wait completion. A secondary launch during
        // reconciliation leaves the event signaled for a subsequent fresh pass; no handler-side
        // reset can erase it.
        let refresh_event = unsafe {
            CreateEventW(
                None,
                refresh_event_manual_reset(),
                false,
                w!("Local\\Calibrator.ThinkPadX915p.Refresh"),
            )
        }
        .map_err(|ref error| Error::windows(Stage::SingleInstance, error))?;

        // SAFETY: default security, non-owned mutex, static NUL-terminated name.
        let mutex =
            match unsafe { CreateMutexW(None, false, w!("Local\\Calibrator.ThinkPadX915p")) } {
                Ok(handle) => handle,
                Err(ref error) => {
                    // SAFETY: refresh_event was created/opened above and remains owned here.
                    let _ = unsafe { CloseHandle(refresh_event) };
                    return Err(Error::windows(Stage::SingleInstance, error));
                }
            };

        // SAFETY: this is the first Win32 call after CreateMutexW.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            if request_refresh {
                // SAFETY: shared auto-reset event remains live in the primary process.
                if let Err(ref error) = unsafe { SetEvent(refresh_event) } {
                    // SAFETY: both handles are owned by this secondary process.
                    let _ = unsafe { CloseHandle(mutex) };
                    let _ = unsafe { CloseHandle(refresh_event) };
                    return Err(Error::windows(Stage::SingleInstance, error));
                }
            }
            // SAFETY: both handles are owned by this secondary process.
            let _ = unsafe { CloseHandle(mutex) };
            let _ = unsafe { CloseHandle(refresh_event) };
            Ok(Acquire::AlreadyRunning)
        } else {
            Ok(Acquire::Acquired(Self {
                mutex,
                refresh_event,
            }))
        }
    }

    pub(crate) const fn refresh_event(&self) -> HANDLE {
        self.refresh_event
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: both owned live handles are closed once, after the event loop has stopped.
        let _ = unsafe { CloseHandle(self.refresh_event) };
        let _ = unsafe { CloseHandle(self.mutex) };
    }
}

#[cfg(test)]
mod tests {
    use super::refresh_event_manual_reset;

    #[test]
    fn refresh_event_is_auto_reset_to_preserve_signals_during_reconciliation() {
        assert!(!refresh_event_manual_reset());
    }
}

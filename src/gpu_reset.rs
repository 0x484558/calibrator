use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, HANDLE};
use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtNext, EvtSubscribe, EvtSubscribeToFutureEvents,
};
use windows::core::{HRESULT, w};

use crate::win32::{Error, Stage};

const BATCH_SIZE: usize = 16;

/// Pull subscription for Windows' canonical TDR recovery event.
///
/// Event 4101 from provider `Display` is emitted when the display driver stopped responding and
/// recovered. The subscription's signal handle is part of the daemon's blocking wait set; no
/// polling, callback thread, or recurring wakeup is introduced.
pub(crate) struct GpuResetSubscription {
    handle: EVT_HANDLE,
}

impl GpuResetSubscription {
    pub(crate) fn create(signal: HANDLE) -> Result<Self, Error> {
        // SAFETY: local System channel subscription, future events only, pull mode selected by
        // providing a signal event and no callback.
        let handle = unsafe {
            EvtSubscribe(
                None,
                Some(signal),
                w!("System"),
                w!("*[System[Provider[@Name='Display'] and (EventID=4101)]]"),
                None,
                None,
                None,
                EvtSubscribeToFutureEvents.0,
            )
        }
        .map_err(|ref error| Error::windows(Stage::GpuResetSubscription, error))?;
        Ok(Self { handle })
    }

    pub(crate) fn drain(&self) -> Result<bool, Error> {
        let mut observed = false;
        loop {
            let mut events = [0isize; BATCH_SIZE];
            let mut returned = 0;
            // SAFETY: subscription is live; output array and count are writable; timeout zero
            // drains only already-signaled events and never polls on a schedule.
            match unsafe { EvtNext(self.handle, &mut events, 0, 0, &raw mut returned) } {
                Ok(()) => {
                    observed |= returned != 0;
                    for raw in events.into_iter().take(returned as usize) {
                        // SAFETY: each returned event handle is caller-owned and closed once.
                        let _ = unsafe { EvtClose(EVT_HANDLE(raw)) };
                    }
                }
                Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_ITEMS.0) => {
                    return Ok(observed);
                }
                Err(ref error) => {
                    return Err(Error::windows(Stage::GpuResetSubscription, error));
                }
            }
        }
    }
}

impl Drop for GpuResetSubscription {
    fn drop(&mut self) {
        // SAFETY: owned subscription handle, closed once.
        let _ = unsafe { EvtClose(self.handle) };
    }
}

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::StationsAndDesktops::{
    GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::WSF_VISIBLE;

use crate::win32::{Error, Stage};

pub(crate) fn ensure_interactive_window_station() -> Result<(), Error> {
    // SAFETY: returns the process-owned station handle; caller must not close it.
    let station = unsafe { GetProcessWindowStation() }
        .map_err(|ref error| Error::windows(Stage::InteractiveSession, error))?;

    let mut flags = USEROBJECTFLAGS::default();
    let mut bytes_needed = 0;
    // SAFETY: the station handle is live; output storage has the exact UOI_FLAGS layout.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(station.0),
            UOI_FLAGS,
            Some((&raw mut flags).cast::<c_void>()),
            u32::try_from(size_of::<USEROBJECTFLAGS>()).unwrap_or(0),
            Some(&raw mut bytes_needed),
        )
    }
    .map_err(|ref error| Error::windows(Stage::InteractiveSession, error))?;

    if flags.dwFlags & WSF_VISIBLE as u32 == 0 {
        return Err(Error::condition(Stage::InteractiveSession));
    }

    Ok(())
}

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Devices::Display::GUID_DEVINTERFACE_MONITOR;
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    RegisterSuspendResumeNotification, UnregisterPowerSettingNotification,
    UnregisterSuspendResumeNotification,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::SystemServices::{GUID_SESSION_DISPLAY_STATUS, PowerMonitorOn};
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETFOCUS,
    NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DBT_CONFIGCHANGED,
    DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVNODES_CHANGED, DBT_DEVTYP_DEVICEINTERFACE,
    DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyMenu,
    DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW, HDEVNOTIFY, IDI_APPLICATION, LoadIconW,
    MF_STRING, PBT_APMRESUMEAUTOMATIC, PBT_POWERSETTINGCHANGE, PostMessageW, RegisterClassW,
    RegisterDeviceNotificationW, RegisterWindowMessageW, SetForegroundWindow, SetWindowLongPtrW,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, UnregisterClassW,
    UnregisterDeviceNotification, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DEVICECHANGE,
    WM_DISPLAYCHANGE, WM_LBUTTONDBLCLK, WM_NCCREATE, WM_NCDESTROY, WM_NULL, WM_POWERBROADCAST,
    WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use crate::win32::{Error, Stage};

const TRAY_CALLBACK: u32 = WM_APP + 1;
const TRAY_ICON_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const TOOLTIP: &str = "HDR calibrator — double-click to exit";
const PROBE_TOOLTIP: &str = "HDR probe — use tray menu to record or exit";
const COMMAND_PROBE: usize = 1;
const COMMAND_EXIT: usize = 2;

#[derive(Clone, Copy)]
pub(crate) struct WindowSignals {
    pub(crate) reconcile: HANDLE,
    pub(crate) probe: HANDLE,
    pub(crate) exit: HANDLE,
}

pub(crate) struct Tray {
    window: HWND,
    instance: HINSTANCE,
    registrations: Option<Registrations>,
    context: Box<WindowContext>,
    icon_added: bool,
}

const PROBE_POS_NONE: u32 = u32::MAX;

struct WindowContext {
    signals: WindowSignals,
    probe_mode: bool,
    taskbar_created_msg: u32,
    probe_position: AtomicU32,
}

struct Registrations {
    window: HWND,
    suspend_resume: Option<HPOWERNOTIFY>,
    display_power: Option<HPOWERNOTIFY>,
    display_device: Option<HDEVNOTIFY>,
    session: bool,
}

impl Tray {
    pub(crate) fn create(signals: WindowSignals, probe_mode: bool) -> Result<Self, Error> {
        // SAFETY: None requests this executable's module.
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|ref error| Error::windows(Stage::RegisterWindowClass, error))?;
        let instance = HINSTANCE(module.0);
        // Register the dynamic TaskbarCreated message broadcast by Explorer on launch/restart.
        // SAFETY: static wide string.
        let taskbar_created_msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

        let mut context = Box::new(WindowContext {
            signals,
            probe_mode,
            taskbar_created_msg,
            probe_position: AtomicU32::new(PROBE_POS_NONE),
        });

        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("CalibratorMessageWindow"),
            ..Default::default()
        };
        // SAFETY: all pointers are static and callback uses the required ABI.
        if unsafe { RegisterClassW(&raw const window_class) } == 0 {
            return Err(Error::last(Stage::RegisterWindowClass));
        }

        // Create an invisible 0×0 tool window at the top level so that it receives
        // WM_DISPLAYCHANGE broadcasts (which HWND_MESSAGE windows never see) while
        // remaining absent from the taskbar and alt-tab list (WS_EX_TOOLWINDOW) and
        // never taking focus while idle (WS_EX_NOACTIVATE + no WS_VISIBLE).
        // WS_POPUP satisfies the CreateWindowExW requirement for a sizeable top-level
        // window without adding any visible chrome.
        let window = match unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("CalibratorMessageWindow"),
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                Some((&raw mut *context).cast::<c_void>()),
            )
        } {
            Ok(window) => window,
            Err(ref error) => {
                // SAFETY: this process registered the class and created no window.
                let _ = unsafe { UnregisterClassW(w!("CalibratorMessageWindow"), Some(instance)) };
                return Err(Error::windows(Stage::CreateMessageWindow, error));
            }
        };

        let registrations = match Registrations::create(window) {
            Ok(registrations) => registrations,
            Err(error) => {
                // SAFETY: window and class are owned by this thread.
                let _ = unsafe { DestroyWindow(window) };
                let _ = unsafe { UnregisterClassW(w!("CalibratorMessageWindow"), Some(instance)) };
                return Err(error);
            }
        };

        let mut tray = Self {
            window,
            instance,
            registrations: Some(registrations),
            context,
            icon_added: false,
        };
        // Tray creation is best-effort: if Explorer is not running yet, startup still succeeds.
        tray.icon_added = ensure_tray_icon(tray.window, &tray.context);
        Ok(tray)
    }

    pub(crate) fn set_probe_position(&mut self, position: Option<u8>) {
        let encoded = match position {
            Some(pos) => u32::from(pos),
            None => PROBE_POS_NONE,
        };
        self.context.probe_position.store(encoded, Ordering::Relaxed);
        self.icon_added = ensure_tray_icon(self.window, &self.context);
    }

    fn notification_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
            hWnd: self.window,
            uID: TRAY_ICON_ID,
            ..Default::default()
        }
    }

    fn delete_icon(&mut self) {
        if self.icon_added {
            let data = self.notification_data();
            // SAFETY: identifies the icon owned by this Tray.
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const data) };
            self.icon_added = false;
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // Registrations must be removed while their recipient HWND remains live.
        drop(self.registrations.take());
        self.delete_icon();
        // SAFETY: window/class are owned by this thread and destroyed/unregistered once.
        let _ = unsafe { DestroyWindow(self.window) };
        let _ = unsafe { UnregisterClassW(w!("CalibratorMessageWindow"), Some(self.instance)) };

        // Keep explicit read so the context's required lifetime is apparent to maintenance and
        // static analysis.
        let _ = &self.context;
    }
}

impl Registrations {
    fn create(window: HWND) -> Result<Self, Error> {
        let mut registrations = Self {
            window,
            suspend_resume: None,
            display_power: None,
            display_device: None,
            session: false,
        };
        let recipient = HANDLE(window.0);

        // SAFETY: recipient is a live HWND and remains live through unregister.
        registrations.suspend_resume = Some(
            unsafe { RegisterSuspendResumeNotification(recipient, DEVICE_NOTIFY_WINDOW_HANDLE) }
                .map_err(|ref error| Error::windows(Stage::RegisterPowerNotifications, error))?,
        );
        // SAFETY: same recipient lifetime; GUID is static.
        registrations.display_power = Some(
            unsafe {
                RegisterPowerSettingNotification(
                    recipient,
                    &GUID_SESSION_DISPLAY_STATUS,
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                )
            }
            .map_err(|ref error| Error::windows(Stage::RegisterPowerNotifications, error))?,
        );
        // SAFETY: live window; only this interactive session is requested.
        unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) }
            .map_err(|ref error| Error::windows(Stage::RegisterSessionNotifications, error))?;
        registrations.session = true;

        let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
            dbcc_size: u32::try_from(size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>()).unwrap_or(0),
            dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
            dbcc_classguid: GUID_DEVINTERFACE_MONITOR,
            ..Default::default()
        };
        // SAFETY: filter has documented layout/type and is consumed during registration.
        registrations.display_device = Some(
            unsafe {
                RegisterDeviceNotificationW(
                    recipient,
                    (&raw const filter).cast::<c_void>(),
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                )
            }
            .map_err(|ref error| Error::windows(Stage::RegisterDisplayNotifications, error))?,
        );

        Ok(registrations)
    }
}

impl Drop for Registrations {
    fn drop(&mut self) {
        if let Some(handle) = self.display_device.take() {
            // SAFETY: handle came from RegisterDeviceNotificationW and is released once.
            let _ = unsafe { UnregisterDeviceNotification(handle) };
        }
        if self.session {
            // SAFETY: registration belongs to this still-live window.
            let _ = unsafe { WTSUnRegisterSessionNotification(self.window) };
            self.session = false;
        }
        if let Some(handle) = self.display_power.take() {
            // SAFETY: matching unregister for the owned registration handle.
            let _ = unsafe { UnregisterPowerSettingNotification(handle) };
        }
        if let Some(handle) = self.suspend_resume.take() {
            // SAFETY: matching unregister for the owned registration handle.
            let _ = unsafe { UnregisterSuspendResumeNotification(handle) };
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: lParam is CREATESTRUCTW for WM_NCCREATE. lpCreateParams is the stable
        // WindowContext pointer supplied to CreateWindowExW.
        let create = unsafe { &*(l_param.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }

    // SAFETY: GWLP_USERDATA is either zero during earliest creation or the live context pointer.
    let context = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const WindowContext };

    if message == TRAY_CALLBACK && !context.is_null() {
        // SAFETY: context remains live during synchronous message handling.
        if let Some(result) = handle_tray_callback(window, w_param, l_param, unsafe { &*context }) {
            return result;
        }
    }

    if !context.is_null()
        && unsafe { (*context).taskbar_created_msg != 0 }
        && message == unsafe { (*context).taskbar_created_msg }
    {
        // Re-add tray icon when Explorer starts or restarts.
        let _ = ensure_tray_icon(window, unsafe { &*context });
        return LRESULT(0);
    }

    match message {
        WM_POWERBROADCAST if !context.is_null() => {
            let msg_param = u32::try_from(w_param.0).unwrap_or(0);
            if msg_param == PBT_APMRESUMEAUTOMATIC {
                // One automatic notification is delivered for every resume; deliberately ignore
                // PBT_APMRESUMESUSPEND to avoid double reconciliation for user-present resumes.
                let _ = unsafe { SetEvent((*context).signals.reconcile) };
            } else if msg_param == PBT_POWERSETTINGCHANGE && l_param.0 != 0 {
                // SAFETY: lParam points to POWERBROADCAST_SETTING for this message only.
                let setting = unsafe { &*(l_param.0 as *const POWERBROADCAST_SETTING) };
                if setting.PowerSetting == GUID_SESSION_DISPLAY_STATUS
                    && setting.DataLength >= u32::try_from(size_of::<u32>()).unwrap_or(0)
                {
                    // Data may be unaligned because it is a trailing byte array.
                    let state =
                        unsafe { std::ptr::read_unaligned(setting.Data.as_ptr().cast::<u32>()) };
                    if state == PowerMonitorOn.0 as u32 {
                        let _ = unsafe { SetEvent((*context).signals.reconcile) };
                    }
                }
            }
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE
            if !context.is_null()
                && u32::try_from(w_param.0).unwrap_or(0) == WTS_SESSION_UNLOCK =>
        {
            let _ = unsafe { SetEvent((*context).signals.reconcile) };
            LRESULT(0)
        }
        WM_DEVICECHANGE
            if !context.is_null()
                && matches!(
                    u32::try_from(w_param.0).unwrap_or(0),
                    DBT_DEVICEARRIVAL
                        | DBT_DEVICEREMOVECOMPLETE
                        | DBT_DEVNODES_CHANGED
                        | DBT_CONFIGCHANGED
                ) =>
        {
            let _ = unsafe { SetEvent((*context).signals.reconcile) };
            LRESULT(0)
        }
        // Top-level tool window receives this broadcast on HDR toggle or resolution change.
        WM_DISPLAYCHANGE if !context.is_null() => {
            let _ = unsafe { SetEvent((*context).signals.reconcile) };
            LRESULT(0)
        }
        WM_CLOSE => LRESULT(0),
        WM_NCDESTROY => {
            // SAFETY: clear the non-owning pointer before default destruction finishes.
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                DefWindowProcW(window, message, w_param, l_param)
            }
        }
        _ => {
            // SAFETY: unhandled messages are forwarded unchanged.
            unsafe { DefWindowProcW(window, message, w_param, l_param) }
        }
    }
}

fn handle_tray_callback(
    window: HWND,
    w_param: WPARAM,
    l_param: LPARAM,
    context: &WindowContext,
) -> Option<LRESULT> {
    let packed = u32::try_from(l_param.0.cast_unsigned() & 0xffff_ffff).unwrap_or(0);
    let event = packed & 0xffff;
    let icon_id = packed >> 16;
    if icon_id == TRAY_ICON_ID && event == WM_LBUTTONDBLCLK {
        // SAFETY: daemon owns exit signal for longer than message window.
        let _ = unsafe { SetEvent(context.signals.exit) };
        return Some(LRESULT(0));
    }
    if icon_id == TRAY_ICON_ID && event == NIN_KEYSELECT {
        let signal = if context.probe_mode {
            context.signals.probe
        } else {
            context.signals.exit
        };
        let _ = unsafe { SetEvent(signal) };
        return Some(LRESULT(0));
    }
    if icon_id == TRAY_ICON_ID && event == WM_CONTEXTMENU {
        // Version-4 notifications encode signed screen coordinates in wParam.
        let packed_position = u32::try_from(w_param.0 & 0xffff_ffff).unwrap_or(0);
        let x = i32::from(i16::from_ne_bytes(
            u16::try_from(packed_position & 0xffff)
                .unwrap_or(0)
                .to_ne_bytes(),
        ));
        let y = i32::from(i16::from_ne_bytes(
            u16::try_from(packed_position >> 16)
                .unwrap_or(0)
                .to_ne_bytes(),
        ));
        // SAFETY: context remains live for this synchronous explicit-interaction menu.
        show_context_menu(window, x, y, context);
        return Some(LRESULT(0));
    }
    None
}

fn show_context_menu(window: HWND, x: i32, y: i32, context: &WindowContext) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    if context.probe_mode {
        // SAFETY: menu is live and text is static for the synchronous menu lifetime.
        let _ = unsafe { AppendMenuW(menu, MF_STRING, COMMAND_PROBE, w!("Record current value")) };
    }
    // SAFETY: same menu/text lifetime.
    let _ = unsafe { AppendMenuW(menu, MF_STRING, COMMAND_EXIT, w!("Exit")) };

    // Windows requires the notification-icon owner window to be the foreground window
    // before TrackPopupMenuEx; without this the menu stays open when the user clicks
    // elsewhere. This is genuine foreground ownership for an explicit user interaction,
    // not background promotion while idle.
    // SAFETY: window is live; SetForegroundWindow may silently fail on non-foreground
    // processes but the call is always required for correct menu behaviour.
    let _ = unsafe { SetForegroundWindow(window) };

    // TPM_RETURNCMD|TPM_NONOTIFY returns the explicit selection without posting command messages.
    let command = unsafe {
        TrackPopupMenuEx(
            menu,
            (TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON).0,
            x,
            y,
            window,
            None,
        )
    };

    // SAFETY: window is live; WM_NULL with zero params is always valid.
    let _ = unsafe { PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0)) };

    // SAFETY: this function owns the menu and destroys it exactly once after tracking ends.
    let _ = unsafe { DestroyMenu(menu) };

    let cmd_id = usize::try_from(command.0).unwrap_or(0);
    let signal = match cmd_id {
        COMMAND_PROBE if context.probe_mode => Some(context.signals.probe),
        COMMAND_EXIT => Some(context.signals.exit),
        _ => None,
    };
    if let Some(signal) = signal {
        // SAFETY: daemon owns the signal for longer than the message window.
        let _ = unsafe { SetEvent(signal) };
    }

    let data = NOTIFYICONDATAW {
        cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
        hWnd: window,
        uID: TRAY_ICON_ID,
        ..Default::default()
    };
    // Return keyboard focus bookkeeping to the notification area.
    let _ = unsafe { Shell_NotifyIconW(NIM_SETFOCUS, &raw const data) };
}

fn ensure_tray_icon(window: HWND, context: &WindowContext) -> bool {
    let Ok(icon) = (unsafe { LoadIconW(None, IDI_APPLICATION) }) else {
        return false;
    };

    let raw_pos = context.probe_position.load(Ordering::Relaxed);
    let position = u8::try_from(raw_pos).ok();

    let tooltip = if context.probe_mode {
        match position {
            Some(pos) => format!("HDR probe — set slider to {pos}, then use tray menu"),
            None => PROBE_TOOLTIP.to_owned(),
        }
    } else {
        TOOLTIP.to_owned()
    };

    let mut data = NOTIFYICONDATAW {
        cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
        hWnd: window,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: TRAY_CALLBACK,
        hIcon: icon,
        ..Default::default()
    };
    write_wide_truncated(&mut data.szTip, &tooltip);

    // SAFETY: packet identifies this live window and shared icon; Shell consumes it synchronously.
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const data) }.as_bool() {
        return true;
    }

    if unsafe { Shell_NotifyIconW(NIM_ADD, &raw const data) }.as_bool() {
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &raw const data) };
        return true;
    }

    false
}

fn write_wide_truncated<const N: usize>(destination: &mut [u16; N], text: &str) {
    if N == 0 {
        return;
    }
    destination.fill(0);
    let mut length = 0;
    for character in text.chars() {
        let mut encoded = [0; 2];
        let encoded = character.encode_utf16(&mut encoded);
        if length + encoded.len() >= N {
            break;
        }
        destination[length..length + encoded.len()].copy_from_slice(encoded);
        length += encoded.len();
    }
    destination[length] = 0;
}

#[cfg(test)]
mod tests {
    use super::write_wide_truncated;

    #[test]
    fn wide_string_is_terminated() {
        let mut buffer = [99; 8];
        write_wide_truncated(&mut buffer, "abc");
        assert_eq!(&buffer[..4], &[u16::from(b'a'), u16::from(b'b'), u16::from(b'c'), 0]);
    }

    #[test]
    fn wide_string_truncates_without_splitting_utf16_units() {
        let mut buffer = [99; 3];
        write_wide_truncated(&mut buffer, "A😀Z");
        assert_eq!(buffer, [u16::from(b'A'), 0, 0]);
    }

    #[test]
    fn zero_sized_destination_is_supported() {
        let mut buffer = [];
        write_wide_truncated(&mut buffer, "ignored");
    }
}

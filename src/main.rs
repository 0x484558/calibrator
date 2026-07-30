#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod calibration;
#[cfg(windows)]
mod daemon;
#[cfg(windows)]
mod display;
#[cfg(windows)]
mod gpu_reset;
#[cfg(windows)]
mod power_policy;
#[cfg(windows)]
mod probe;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod single_instance;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod win32;
#[cfg(windows)]
mod wmi_monitor;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match daemon::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // This executable intentionally has no console or visible error UI. Preserve a
            // diagnosable failure category for process supervisors and debuggers.
            std::process::ExitCode::from(error.exit_code())
        }
    }
}

#[cfg(not(windows))]
compile_error!("calibrator is a Windows-only interactive-session daemon");

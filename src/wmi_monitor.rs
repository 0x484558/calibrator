use std::ffi::c_void;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use futures::channel::oneshot;
use futures::{FutureExt, StreamExt, pin_mut, select_biased};
use serde::Deserialize;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::SetEvent;
use wmi::WMIConnection;

use crate::power_policy::apply_to_current_event_thread;
use crate::win32::{Error, Stage};

const NAMESPACE: &str = "ROOT\\WMI";
const EVENT_QUERY: &str = "SELECT Active, Brightness, InstanceName FROM WmiMonitorBrightnessEvent";
const SNAPSHOT_QUERY: &str =
    "SELECT Active, CurrentBrightness, InstanceName FROM WmiMonitorBrightness WHERE Active = TRUE";

#[derive(Deserialize)]
struct BrightnessEvent {
    #[serde(rename = "Active")]
    active: bool,
    #[serde(rename = "Brightness")]
    brightness: u8,
    #[serde(rename = "InstanceName")]
    _instance_name: String,
}

#[derive(Deserialize)]
struct BrightnessSnapshot {
    #[serde(rename = "Active")]
    active: bool,
    #[serde(rename = "CurrentBrightness")]
    current_brightness: u8,
    #[serde(rename = "InstanceName")]
    _instance_name: String,
}

pub(crate) struct SnapshotReader {
    connection: WMIConnection,
}

impl SnapshotReader {
    pub(crate) fn create() -> Result<Self, Error> {
        let connection = WMIConnection::with_namespace_path(NAMESPACE)
            .map_err(|_| Error::condition(Stage::WmiSnapshot))?;
        Ok(Self { connection })
    }

    #[must_use]
    pub(crate) fn current_brightness(&self) -> Option<u8> {
        let records: Vec<BrightnessSnapshot> = self.connection.raw_query(SNAPSHOT_QUERY).ok()?;
        let mut active = records
            .into_iter()
            .filter(|record| record.active && record.current_brightness <= 100);
        let first = active.next()?.current_brightness;

        // Multiple active WMI records are ambiguous unless they agree. Never associate an
        // arbitrary brightness with the internal HDR target.
        if active.all(|record| record.current_brightness == first) {
            Some(first)
        } else {
            None
        }
    }
}

pub(crate) struct WmiWatcher {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl WmiWatcher {
    pub(crate) fn start(brightness_signal: HANDLE, failure_signal: HANDLE) -> Result<Self, Error> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let brightness_signal_value = brightness_signal.0 as usize;
        let failure_signal_value = failure_signal.0 as usize;

        let thread = thread::Builder::new()
            .name("calibrator-wmi".to_owned())
            .spawn(move || {
                let brightness_signal = HANDLE(brightness_signal_value as *mut c_void);
                let failure_signal = HANDLE(failure_signal_value as *mut c_void);
                if apply_to_current_event_thread().is_err() {
                    let _ = ready_tx.send(Err(Error::condition(Stage::EventThreadPowerPolicy)));
                    return;
                }

                let Ok(connection) = WMIConnection::with_namespace_path(NAMESPACE) else {
                    let _ = ready_tx.send(Err(Error::condition(Stage::WmiSubscription)));
                    return;
                };
                let Ok(events) = connection.async_raw_notification::<BrightnessEvent>(EVENT_QUERY) else {
                    let _ = ready_tx.send(Err(Error::condition(Stage::WmiSubscription)));
                    return;
                };
                if ready_tx.send(Ok(())).is_err() {
                    return;
                }

                let events = events.fuse();
                let shutdown = shutdown_rx.fuse();
                pin_mut!(events, shutdown);

                futures::executor::block_on(async {
                    loop {
                        select_biased! {
                            _ = shutdown => break,
                            item = events.next() => {
                                if let Some(Ok(event)) = item {
                                    if event.active && event.brightness <= 100 {
                                        // SAFETY: daemon owns this event until after the listener
                                        // has been stopped and joined.
                                        let _ = unsafe { SetEvent(brightness_signal) };
                                    }
                                } else {
                                    // SAFETY: same lifetime guarantee as brightness_signal.
                                    let _ = unsafe { SetEvent(failure_signal) };
                                    break;
                                }
                            }
                        }
                    }
                });
            })
            .map_err(|_| Error::condition(Stage::WmiSubscription))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shutdown: Some(shutdown_tx),
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Error::condition(Stage::WmiSubscription))
            }
        }
    }
}

impl Drop for WmiWatcher {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

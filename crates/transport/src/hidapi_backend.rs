//! Native Windows `hidapi` backend implementing [`HidEnumerator`] and [`HidTransport`].

use std::ffi::CString;
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use hidapi::{BusType, DeviceInfo as HidapiDeviceInfo, HidApi, HidDevice};

use crate::descriptor::{HidBusType, HidDeviceDescriptor};
use crate::error::{TransportError, TransportResult};
use crate::traits::{HidEnumerator, HidTransport};

/// Native OS HID enumerator backed by `hidapi`.
#[derive(Debug, Default)]
pub struct HidapiEnumerator;

impl HidapiEnumerator {
    pub const fn new() -> Self {
        Self
    }
}

impl HidEnumerator for HidapiEnumerator {
    fn enumerate(&self) -> TransportResult<Vec<HidDeviceDescriptor>> {
        let api = HidApi::new().map_err(|err| TransportError::InitFailed(err.to_string()))?;
        let mut devices: Vec<HidDeviceDescriptor> =
            api.device_list().map(descriptor_from_hidapi).collect();

        // Sort deterministically by VID, PID, Interface #, Usage Page, Usage ID, Path.
        devices.sort_by(|a, b| {
            (
                a.vid,
                a.pid,
                a.interface_number,
                a.usage_page,
                a.usage_id,
                &a.path,
            )
                .cmp(&(
                    b.vid,
                    b.pid,
                    b.interface_number,
                    b.usage_page,
                    b.usage_id,
                    &b.path,
                ))
        });

        Ok(devices)
    }

    fn open_path(
        &self,
        descriptor: &HidDeviceDescriptor,
    ) -> TransportResult<Box<dyn HidTransport>> {
        let api = HidApi::new().map_err(|err| TransportError::InitFailed(err.to_string()))?;
        let c_path =
            CString::new(descriptor.path.as_str()).map_err(|err| TransportError::OpenFailed {
                path: descriptor.path.clone(),
                reason: format!("path contains interior NUL byte: {err}"),
            })?;

        let device =
            api.open_path(c_path.as_c_str())
                .map_err(|err| TransportError::OpenFailed {
                    path: descriptor.path.clone(),
                    reason: err.to_string(),
                })?;

        Ok(Box::new(HidapiTransport::new(descriptor.clone(), device)))
    }
}

/// Live USB HID transport wrapping an opened `hidapi::HidDevice`.
///
/// `Mutex<HidDevice>` provides `Sync` safely without `unsafe` because `hidapi::HidDevice` is `Send`.
pub struct HidapiTransport {
    descriptor: HidDeviceDescriptor,
    device: Mutex<HidDevice>,
}

impl fmt::Debug for HidapiTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HidapiTransport")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

impl HidapiTransport {
    pub fn new(descriptor: HidDeviceDescriptor, device: HidDevice) -> Self {
        Self {
            descriptor,
            device: Mutex::new(device),
        }
    }
}

impl HidTransport for HidapiTransport {
    fn descriptor(&self) -> &HidDeviceDescriptor {
        &self.descriptor
    }

    fn send_output_report(&mut self, report: &[u8]) -> TransportResult<usize> {
        let device = self
            .device
            .lock()
            .map_err(|_| TransportError::Io("HID device mutex poisoned".to_owned()))?;
        device
            .write(report)
            .map_err(|err| TransportError::Io(err.to_string()))
    }

    fn read_input_report(&mut self, buf: &mut [u8], timeout: Duration) -> TransportResult<usize> {
        let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let device = self
            .device
            .lock()
            .map_err(|_| TransportError::Io("HID device mutex poisoned".to_owned()))?;
        let bytes_read = device
            .read_timeout(buf, timeout_ms)
            .map_err(|err| TransportError::Io(err.to_string()))?;
        if bytes_read == 0 {
            return Err(TransportError::Timeout {
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            });
        }
        Ok(bytes_read)
    }

    fn drain_input_reports(&mut self) -> TransportResult<usize> {
        const MAX_DRAIN_REPORTS: usize = 512;
        let device = self
            .device
            .lock()
            .map_err(|_| TransportError::Io("HID device mutex poisoned".to_owned()))?;
        let mut discard = [0u8; 512];
        let mut drained = 0usize;
        for _ in 0..MAX_DRAIN_REPORTS {
            let bytes_read = device
                .read_timeout(&mut discard, 0)
                .map_err(|err| TransportError::Io(err.to_string()))?;
            if bytes_read == 0 {
                break;
            }
            drained += 1;
        }
        Ok(drained)
    }

    fn send_feature_report(&mut self, report: &[u8]) -> TransportResult<()> {
        let device = self
            .device
            .lock()
            .map_err(|_| TransportError::Io("HID device mutex poisoned".to_owned()))?;
        device
            .send_feature_report(report)
            .map_err(|err| TransportError::Io(err.to_string()))
    }

    fn get_feature_report(&mut self, buf: &mut [u8]) -> TransportResult<usize> {
        let device = self
            .device
            .lock()
            .map_err(|_| TransportError::Io("HID device mutex poisoned".to_owned()))?;
        device
            .get_feature_report(buf)
            .map_err(|err| TransportError::Io(err.to_string()))
    }
}

fn descriptor_from_hidapi(info: &HidapiDeviceInfo) -> HidDeviceDescriptor {
    let path = info.path().to_string_lossy().into_owned();
    let bus_type = match info.bus_type() {
        BusType::Usb => HidBusType::Usb,
        BusType::Bluetooth => HidBusType::Bluetooth,
        BusType::I2c => HidBusType::I2c,
        BusType::Spi => HidBusType::Spi,
        BusType::Unknown => HidBusType::Unknown,
    };

    HidDeviceDescriptor::new(
        info.vendor_id(),
        info.product_id(),
        info.usage_page(),
        info.usage(),
        info.interface_number(),
        path,
    )
    .with_release_number(info.release_number())
    .with_bus_type(bus_type)
    .with_strings(
        info.manufacturer_string().map(str::to_owned),
        info.product_string().map(str::to_owned),
        info.serial_number().map(str::to_owned),
    )
}

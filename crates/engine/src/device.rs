//! The audio device as the engine sees it.

use std::fmt;

/// Why a device call failed. The runner treats every failure as "device gone" and retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceError(pub String);

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DeviceError {}

/// One output device. Levels are the Windows scalar, 0.0..=1.0.
pub trait VolumeDevice {
    fn volume(&self) -> Result<f64, DeviceError>;
    fn set_volume(&mut self, level: f64) -> Result<(), DeviceError>;
    fn muted(&self) -> Result<bool, DeviceError>;
    fn set_muted(&mut self, muted: bool) -> Result<(), DeviceError>;
}

/// Opens devices by endpoint id. It runs on the runner thread, so it and its devices need
/// not be `Send`. It must never fall back to another device, such as the default one.
pub trait DeviceOpener {
    type Device: VolumeDevice;

    fn open(&mut self, endpoint_id: &str) -> Result<Self::Device, DeviceError>;
}

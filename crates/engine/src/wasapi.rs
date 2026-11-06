//! Windows implementation of [`VolumeDevice`] over `IAudioEndpointVolume`.

use std::marker::PhantomData;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, ENDPOINT_HARDWARE_SUPPORT_VOLUME, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::core::{HSTRING, PWSTR};

use crate::config::PersistedState;
use crate::device::{DeviceError, DeviceOpener, VolumeDevice};
use crate::runner::Runner;

impl From<windows::core::Error> for DeviceError {
    fn from(error: windows::core::Error) -> Self {
        DeviceError(error.to_string())
    }
}

/// A render endpoint the shell can offer. Store `id`: it survives renames and reordering.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
}

/// COM for the current thread, released on drop. Not `Send`: COM init is per thread.
struct Com {
    uninit: bool,
    _not_send: PhantomData<*const ()>,
}

impl Com {
    fn init() -> Result<Self, DeviceError> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // A UI thread may already be in an STA. COM is usable there, but it is not ours to end.
        let uninit = hr != RPC_E_CHANGED_MODE;
        if uninit {
            hr.ok()?;
        }
        Ok(Self {
            uninit,
            _not_send: PhantomData,
        })
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.uninit {
            unsafe { CoUninitialize() };
        }
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator, DeviceError> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?)
}

/// Copies and frees a COM-allocated string.
fn take_pwstr(text: PWSTR) -> Result<String, DeviceError> {
    let result = unsafe { text.to_string() };
    unsafe { CoTaskMemFree(Some(text.0 as *const _)) };
    result.map_err(|error| DeviceError(error.to_string()))
}

fn describe(device: &IMMDevice) -> Result<Endpoint, DeviceError> {
    let id = take_pwstr(unsafe { device.GetId() }?)?;
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }?;
    let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }?;
    let name = take_pwstr(unsafe { PropVariantToStringAlloc(&value) }?)?;
    Ok(Endpoint { id, name })
}

/// Lists active render endpoints. Safe to call from any thread, including a UI thread.
pub fn list_endpoints() -> Result<Vec<Endpoint>, DeviceError> {
    let _com = Com::init()?;
    let endpoints = unsafe { enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }?;
    let count = unsafe { endpoints.GetCount() }?;
    (0..count)
        .map(|index| describe(&unsafe { endpoints.Item(index) }?))
        .collect()
}

/// Opens endpoints by id. Create it on the thread that will use the devices. COM starts on
/// first use, so a failure surfaces as an `open` error and is retried like any other.
#[derive(Default)]
pub struct WasapiOpener {
    // A tuple drops in order, so COM outlives the enumerator.
    session: Option<(IMMDeviceEnumerator, Com)>,
}

impl DeviceOpener for WasapiOpener {
    type Device = WasapiDevice;

    fn open(&mut self, endpoint_id: &str) -> Result<WasapiDevice, DeviceError> {
        let (enumerator, _) = match self.session.take() {
            Some(session) => self.session.insert(session),
            None => {
                let com = Com::init()?;
                self.session.insert((enumerator()?, com))
            }
        };
        let device = unsafe { enumerator.GetDevice(&HSTRING::from(endpoint_id)) }?;
        let endpoint = unsafe { device.Activate(CLSCTX_ALL, None) }?;
        Ok(WasapiDevice { device, endpoint })
    }
}

/// Starts the engine on real audio. It only ever opens `config.device_id`.
pub fn spawn_runner(state: PersistedState) -> Runner {
    Runner::spawn(state, WasapiOpener::default)
}

pub struct WasapiDevice {
    device: IMMDevice,
    endpoint: IAudioEndpointVolume,
}

impl WasapiDevice {
    /// Whether volume is applied by the device rather than by the Windows audio engine.
    /// Software endpoints never touch the device on a write, so they are fast and exact.
    pub fn hardware_volume(&self) -> Result<bool, DeviceError> {
        let support = unsafe { self.endpoint.QueryHardwareSupport() }?;
        Ok(support & ENDPOINT_HARDWARE_SUPPORT_VOLUME != 0)
    }
}

impl VolumeDevice for WasapiDevice {
    // Also checks the endpoint state: an unplugged endpoint's volume object can keep answering.
    fn volume(&self) -> Result<f64, DeviceError> {
        if unsafe { self.device.GetState() }? != DEVICE_STATE_ACTIVE {
            return Err(DeviceError("device is not active".into()));
        }
        Ok(f64::from(unsafe {
            self.endpoint.GetMasterVolumeLevelScalar()
        }?))
    }

    fn set_volume(&mut self, level: f64) -> Result<(), DeviceError> {
        let level = level as f32;
        Ok(unsafe {
            self.endpoint
                .SetMasterVolumeLevelScalar(level, std::ptr::null())
        }?)
    }

    fn muted(&self) -> Result<bool, DeviceError> {
        Ok(unsafe { self.endpoint.GetMute() }?.as_bool())
    }

    fn set_muted(&mut self, muted: bool) -> Result<(), DeviceError> {
        Ok(unsafe { self.endpoint.SetMute(muted, std::ptr::null()) }?)
    }
}

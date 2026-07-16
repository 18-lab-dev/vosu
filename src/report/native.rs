#[cfg(target_os = "linux")]
use std::{fs, path::Path};

use super::{DeviceType, GraphicsAdapter, GraphicsDeviceType, MachineData};

pub(super) fn nonempty_or_unknown(value: String) -> String {
    if value.trim().is_empty() {
        "Unknown".into()
    } else {
        value
    }
}

pub(super) async fn enumerate_graphics_adapters() -> Vec<GraphicsAdapter> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .map(|adapter| {
            let info = adapter.get_info();
            let device_type = match info.device_type {
                wgpu::DeviceType::Cpu => GraphicsDeviceType::Cpu,
                wgpu::DeviceType::IntegratedGpu => GraphicsDeviceType::IntegratedGpu,
                wgpu::DeviceType::DiscreteGpu => GraphicsDeviceType::DiscreteGpu,
                wgpu::DeviceType::VirtualGpu => GraphicsDeviceType::VirtualGpu,
                _ => GraphicsDeviceType::Other,
            };
            GraphicsAdapter {
                name: info.name,
                device_type,
            }
        })
        .collect()
}

pub(super) fn native_machine_base() -> MachineData {
    let mut system = sysinfo::System::new_all();
    system.refresh_cpu_all();
    let cpu = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_owned())
        .filter(|cpu| !cpu.is_empty())
        .unwrap_or_else(|| "Unknown".into());
    let os = sysinfo::System::long_os_version()
        .or_else(sysinfo::System::name)
        .unwrap_or_else(|| "Unknown".into());
    let data = MachineData {
        system_type: native_device_type(),
        os,
        arch: std::env::consts::ARCH.into(),
        cpu,
        adapters: Vec::new(),
        fallback_gpu_names: Vec::new(),
    };
    #[cfg(windows)]
    {
        let (system_type, fallback_gpu_names) = windows_wmi_hardware();
        MachineData {
            system_type,
            fallback_gpu_names,
            ..data
        }
    }
    #[cfg(not(windows))]
    data
}

#[cfg(target_os = "linux")]
fn native_device_type() -> DeviceType {
    classify_linux_device(
        Path::new("/sys/class/power_supply"),
        &[
            Path::new("/sys/class/dmi/id/chassis_type"),
            Path::new("/sys/devices/virtual/dmi/id/chassis_type"),
        ],
    )
}

#[cfg(not(target_os = "linux"))]
fn native_device_type() -> DeviceType {
    DeviceType::Unknown
}

#[cfg(target_os = "linux")]
pub(super) fn classify_linux_device(power_supply: &Path, chassis_paths: &[&Path]) -> DeviceType {
    if fs::read_dir(power_supply)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("BAT"))
    {
        return DeviceType::Laptop;
    }
    let chassis = chassis_paths
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .and_then(|value| value.trim().parse::<u8>().ok());
    match chassis {
        Some(8 | 9 | 10 | 11 | 14 | 30 | 31 | 32) => DeviceType::Laptop,
        Some(3 | 4 | 5 | 6 | 7 | 13 | 15 | 16 | 17 | 23 | 24 | 35 | 36) => DeviceType::Desktop,
        _ => DeviceType::Unknown,
    }
}

#[cfg(windows)]
fn windows_wmi_hardware() -> (DeviceType, Vec<String>) {
    use serde::Deserialize;
    use wmi::WMIConnection;

    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Enclosure {
        chassis_types: Option<Vec<i32>>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Battery {
        #[serde(rename = "Name")]
        _name: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct VideoController {
        name: Option<String>,
    }

    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let connection = WMIConnection::new()?;
        let enclosures: Vec<Enclosure> =
            connection.raw_query("SELECT ChassisTypes FROM Win32_SystemEnclosure")?;
        let batteries: Vec<Battery> = connection.raw_query("SELECT Name FROM Win32_Battery")?;
        let videos: Vec<VideoController> =
            connection.raw_query("SELECT Name FROM Win32_VideoController")?;
        let laptop_chassis = enclosures
            .iter()
            .flat_map(|item| item.chassis_types.iter().flatten())
            .any(|kind| matches!(kind, 8 | 9 | 10 | 11 | 14 | 30 | 31 | 32));
        let device = if !batteries.is_empty() || laptop_chassis {
            DeviceType::Laptop
        } else if enclosures.is_empty() {
            DeviceType::Unknown
        } else {
            DeviceType::Desktop
        };
        let gpu_names = videos.into_iter().filter_map(|video| video.name).collect();
        Ok((device, gpu_names))
    })();
    result.unwrap_or((DeviceType::Unknown, Vec::new()))
}

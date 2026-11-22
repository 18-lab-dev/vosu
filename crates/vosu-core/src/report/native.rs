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

#[cfg(not(windows))]
pub(super) fn enumerate_graphics_adapters() -> Vec<GraphicsAdapter> {
    #[cfg(target_os = "linux")]
    {
        linux_graphics_adapters()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        vec![GraphicsAdapter {
            name: "Unavailable (native display adapter probe unsupported)".into(),
            device_type: GraphicsDeviceType::Other,
        }]
    }
}

#[cfg(target_os = "linux")]
fn linux_graphics_adapters() -> Vec<GraphicsAdapter> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return vec![unavailable_adapter("sysfs unavailable")];
    };
    let mut adapters = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.strip_prefix("card").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
            })
        })
        .map(|entry| {
            let card = entry.file_name().to_string_lossy().into_owned();
            let device = entry.path().join("device");
            let product = fs::read_to_string(device.join("product_name"))
                .ok()
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty());
            let vendor = fs::read_to_string(device.join("vendor")).ok();
            let id = fs::read_to_string(device.join("device")).ok();
            let driver = fs::read_link(device.join("driver")).ok().and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            });
            let mut details = Vec::new();
            if let Some(vendor) = vendor {
                details.push(format!("vendor {}", vendor.trim()));
            }
            if let Some(id) = id {
                details.push(format!("device {}", id.trim()));
            }
            if let Some(driver) = driver {
                details.push(format!("driver {driver}"));
            }
            let name = match product {
                Some(product) if details.is_empty() => product,
                Some(product) => format!("{product} ({})", details.join(", ")),
                None if details.is_empty() => {
                    format!("Linux display adapter {card} (metadata unavailable)")
                }
                None => format!("Linux display adapter {card} ({})", details.join(", ")),
            };
            GraphicsAdapter {
                name,
                device_type: GraphicsDeviceType::Other,
            }
        })
        .collect::<Vec<_>>();
    adapters.sort_by(|left, right| left.name.cmp(&right.name));
    if adapters.is_empty() {
        adapters.push(unavailable_adapter("no display adapters detected"));
    }
    adapters
}

#[cfg(target_os = "linux")]
fn unavailable_adapter(reason: &str) -> GraphicsAdapter {
    GraphicsAdapter {
        name: format!("Unavailable ({reason})"),
        device_type: GraphicsDeviceType::Other,
    }
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
        let adapters = if fallback_gpu_names.is_empty() {
            vec![GraphicsAdapter {
                name: "Unavailable (WMI returned no display adapters)".into(),
                device_type: GraphicsDeviceType::Other,
            }]
        } else {
            fallback_gpu_names
                .iter()
                .cloned()
                .map(|name| GraphicsAdapter {
                    name,
                    device_type: GraphicsDeviceType::Other,
                })
                .collect()
        };
        MachineData {
            system_type,
            adapters,
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

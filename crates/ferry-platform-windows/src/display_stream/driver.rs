//! 安装和移除 ScreenFerry 创建的虚拟显示设备。由管理员安装程序调用。

use std::path::Path;

use ferry_platform::{Error, Result};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows::core::{HSTRING, PCWSTR, w};

use super::os;

const HARDWARE_ID: PCWSTR = w!(r"Root\MttVDD");
const INSTANCE_PREFIX: &str = "ROOT\\SCREENFERRYDISPLAY\\";

pub(super) fn owns_adapter() -> Result<bool> {
    Ok(find_adapters(false)?.1.is_some())
}

struct Devices(HDEVINFO);

impl Drop for Devices {
    fn drop(&mut self) {
        // SAFETY: The list belongs to this installer call.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn device_info() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    }
}

/// Register a virtual adapter only if an MttVDD adapter is not already present.
pub fn install_display_driver(inf: &Path) -> Result<()> {
    if !crate::is_elevated() {
        return Err(Error::PermissionDenied("安装扩展屏驱动需要管理员权限"));
    }
    if !inf.is_absolute()
        || !inf.is_file()
        || !inf
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("MttVDD.inf"))
    {
        return Err(Error::Failed("扩展屏安装包缺少 MttVDD.inf".into()));
    }
    if find_adapters(true)?.1.is_some() {
        // Reinstalling/upgrading must also leave our existing output idle.
        // Never detach an adapter installed and owned by another application.
        if owns_adapter()? {
            super::surface::detach_installed_monitor()?;
        }
        return Ok(());
    }
    // SAFETY: SetupAPI types are initialized with their documented sizes. The package is installed
    // through Windows, which checks the INF and files against the signed catalog.
    unsafe {
        let list = Devices(
            SetupDiCreateDeviceInfoList(Some(&GUID_DEVCLASS_DISPLAY), None)
                .map_err(|e| os("创建虚拟屏设备列表", e))?,
        );
        let mut device = device_info();
        SetupDiCreateDeviceInfoW(
            list.0,
            w!("ScreenFerryDisplay"),
            &GUID_DEVCLASS_DISPLAY,
            w!("ScreenFerry Virtual Display"),
            None,
            DICD_GENERATE_ID,
            Some(&mut device),
        )
        .map_err(|e| os("创建 ScreenFerry 虚拟屏设备", e))?;
        let hardware: Vec<u8> = "Root\\MttVDD\0\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        SetupDiSetDeviceRegistryPropertyW(list.0, &mut device, SPDRP_HARDWAREID, Some(&hardware))
            .map_err(|e| os("设置虚拟屏硬件 ID", e))?;
        SetupDiCallClassInstaller(DIF_REGISTERDEVICE, list.0, Some(&device))
            .map_err(|e| os("登记 ScreenFerry 虚拟屏", e))?;
        let path = HSTRING::from(inf.as_os_str());
        let mut reboot = Default::default();
        if let Err(error) = UpdateDriverForPlugAndPlayDevicesW(
            None,
            HARDWARE_ID,
            &path,
            // This is an interactive administrator installer. Windows must be able to
            // ask the user to trust the signed driver publisher on first installation.
            // NONINTERACTIVE rejects that confirmation with ERROR_AUTHENTICODE_TRUST_NOT_ESTABLISHED.
            Default::default(),
            Some(&mut reboot),
        ) {
            let _ = SetupDiCallClassInstaller(DIF_REMOVE, list.0, Some(&device));
            return Err(os("安装签名虚拟屏驱动", error));
        }
    }
    let mut last_error = None;
    for _ in 0..20 {
        match super::surface::detach_installed_monitor() {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if let Some(error) = last_error {
        return Err(error);
    }
    Ok(())
}

pub fn remove_display_driver() -> Result<()> {
    if !crate::is_elevated() {
        return Err(Error::PermissionDenied("移除扩展屏驱动需要管理员权限"));
    }
    let (devices, found) = find_adapters(false)?;
    if let Some(device) = found {
        // SAFETY: This particular device instance was registered under ScreenFerry's instance name.
        unsafe { SetupDiCallClassInstaller(DIF_REMOVE, devices.0, Some(&device)) }
            .map_err(|e| os("移除 ScreenFerry 虚拟屏", e))?;
    }
    Ok(())
}

fn find_adapters(any_mttvdd: bool) -> Result<(Devices, Option<SP_DEVINFO_DATA>)> {
    // SAFETY: The lists, records and output buffers remain valid throughout the calls.
    unsafe {
        let devices = Devices(
            SetupDiGetClassDevsW(
                Some(&GUID_DEVCLASS_DISPLAY),
                PCWSTR::null(),
                None,
                if any_mttvdd {
                    DIGCF_PRESENT
                } else {
                    Default::default()
                },
            )
            .map_err(|e| os("查找虚拟屏驱动", e))?,
        );
        for index in 0.. {
            let mut device = device_info();
            match SetupDiEnumDeviceInfo(devices.0, index, &mut device) {
                Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_NO_MORE_ITEMS.0) => {
                    break;
                }
                Err(e) => return Err(os("枚举显示设备", e)),
                Ok(()) => {}
            }
            let mut id = [0; 512];
            if SetupDiGetDeviceInstanceIdW(devices.0, &device, Some(&mut id), None).is_ok()
                && widestr(&id)
                    .to_ascii_uppercase()
                    .starts_with(INSTANCE_PREFIX)
            {
                return Ok((devices, Some(device)));
            }
            if any_mttvdd {
                let mut ids = [0_u8; 1024];
                let mut size = 0;
                if SetupDiGetDeviceRegistryPropertyW(
                    devices.0,
                    &device,
                    SPDRP_HARDWAREID,
                    None,
                    Some(&mut ids),
                    Some(&mut size),
                )
                .is_ok()
                {
                    let ids: Vec<u16> = ids[..size as usize]
                        .chunks_exact(2)
                        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                        .collect();
                    if ids
                        .split(|&ch| ch == 0)
                        .any(|id| String::from_utf16_lossy(id).eq_ignore_ascii_case(r"Root\MttVDD"))
                    {
                        return Ok((devices, Some(device)));
                    }
                }
            }
        }
        Ok((devices, None))
    }
}

fn widestr(raw: &[u16]) -> String {
    String::from_utf16_lossy(&raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())])
}

//! Windows 显示布局事务：同时提交物理屏和虚拟屏，保存可恢复的原始主屏与位置。

use ferry_platform::{Error, Result};
use ferry_types::{DisplayOptions, DisplayPrimary, DisplayScreen, Rect};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, POINTL};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::core::{PCWSTR, w};

use super::{display_mode, os};
mod ccd;

#[derive(Clone, Serialize, Deserialize)]
struct Screen {
    name: String,
    x: i32,
    y: i32,
    primary: bool,
}

pub(super) struct Topology(Vec<Screen>);

impl Topology {
    pub fn capture(excluded: &[u16; 32]) -> Result<Self> {
        crate::display::ensure_dpi_aware();
        let mut screens = Vec::new();
        for index in 0..64 {
            let mut device = DISPLAY_DEVICEW {
                cb: size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            // SAFETY: initialized, writable device record, no borrowed device name.
            if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) }.as_bool() {
                break;
            }
            if device.DeviceName == *excluded
                || !device
                    .StateFlags
                    .contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP)
            {
                continue;
            }
            if let Some(mode) = display_mode(
                PCWSTR(device.DeviceName.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                EDS_RAWMODE,
            ) {
                // SAFETY: current display modes contain their desktop position.
                let point = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
                screens.push(Screen {
                    name: name(&device.DeviceName),
                    x: point.x,
                    y: point.y,
                    primary: device.StateFlags.contains(DISPLAY_DEVICE_PRIMARY_DEVICE),
                });
            }
        }
        if screens.is_empty() {
            return Err(Error::Failed("未找到可用实体屏幕".into()));
        }
        Ok(Self(screens))
    }

    pub fn screens(&self) -> Vec<DisplayScreen> {
        self.0
            .iter()
            .filter_map(|s| {
                let id = s.name.strip_prefix(r"\\.\DISPLAY")?.parse().ok()?;
                let mode = current(&s.name).ok()?;
                Some(DisplayScreen {
                    id,
                    bounds: Rect::new(s.x, s.y, mode.dmPelsWidth, mode.dmPelsHeight),
                    primary: s.primary,
                })
            })
            .collect()
    }

    pub fn apply(
        &self,
        virtual_name: &[u16; 32],
        mut mode: DEVMODEW,
        options: DisplayOptions,
        owned: bool,
    ) -> Result<POINTL> {
        let screens = self.screens();
        let anchor = match options.source_screen {
            Some(id) => screens.iter().find(|s| s.id == id),
            None => screens.iter().find(|s| s.primary).or(screens.first()),
        }
        .ok_or_else(|| Error::Failed("方案指定的来源屏幕已断开".into()))?;
        let extension = ferry_core::display_extension_bounds(
            anchor.bounds,
            mode.dmPelsWidth,
            mode.dmPelsHeight,
            options.side,
        )
        .ok_or_else(|| Error::Failed("扩展屏坐标超出范围".into()))?;
        if screens.iter().any(|s| overlaps(s.bounds, extension)) {
            return Err(Error::Failed(
                "所选位置已被其他屏幕占用，请选择其他屏幕或方向".into(),
            ));
        }
        if owned {
            save(&self.0)?;
        }
        // Activate IddCx in a valid secondary position first. CCD then applies
        // all positions and the new origin together, preserving physical modes.
        let right = screens
            .iter()
            .map(|s| s.bounds.x + s.bounds.width as i32)
            .max()
            .unwrap_or(0);
        mode.dmFields |= DM_POSITION;
        mode.Anonymous1.Anonymous2.dmPosition = POINTL {
            x: right,
            y: anchor.bounds.y,
        };
        stage(&name(virtual_name), &mode)?;
        commit()?;
        let origin = if options.primary == DisplayPrimary::Viewer {
            (extension.x, extension.y)
        } else {
            (anchor.bounds.x, anchor.bounds.y)
        };
        let position = POINTL {
            x: extension.x - origin.0,
            y: extension.y - origin.1,
        };
        let mut positions: Vec<_> = self
            .0
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    POINTL {
                        x: s.x - origin.0,
                        y: s.y - origin.1,
                    },
                )
            })
            .collect();
        positions.push((name(virtual_name), position));
        ccd::apply(&positions, None)?;
        tracing::info!(
            ?options,
            x = position.x,
            y = position.y,
            "已应用 Windows 扩展屏布局"
        );
        Ok(position)
    }

    pub fn restore(
        &self,
        virtual_name: &[u16; 32],
        previous: Option<&DEVMODEW>,
        owned: bool,
    ) -> Result<()> {
        let primary = self
            .0
            .iter()
            .find(|s| s.primary)
            .or(self.0.first())
            .ok_or_else(|| Error::Failed("缺少恢复屏幕".into()))?;
        let origin = if self.0.iter().any(|s| s.primary) {
            (0, 0)
        } else {
            (primary.x, primary.y)
        };
        let positions: Vec<_> = self
            .0
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    POINTL {
                        x: s.x - origin.0,
                        y: s.y - origin.1,
                    },
                )
            })
            .collect();
        let virtual_name = name(virtual_name);
        ccd::apply(&positions, Some(&virtual_name))?;
        if let Some(previous) = previous {
            stage(&virtual_name, previous)?;
            commit()?;
        }
        if owned {
            clear()?;
        }
        Ok(())
    }

    pub fn recover(virtual_name: &[u16; 32]) -> Result<()> {
        match load()? {
            Some(screens) => Self(screens).restore(virtual_name, None, true),
            None => Self::capture(virtual_name)?.restore(virtual_name, None, true),
        }
    }
    pub fn needs_recovery() -> Result<bool> {
        load().map(|saved| saved.is_some())
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    i64::from(a.x) < i64::from(b.x) + i64::from(b.width)
        && i64::from(b.x) < i64::from(a.x) + i64::from(a.width)
        && i64::from(a.y) < i64::from(b.y) + i64::from(b.height)
        && i64::from(b.y) < i64::from(a.y) + i64::from(a.height)
}

fn name(raw: &[u16]) -> String {
    String::from_utf16_lossy(&raw[..raw.iter().position(|&v| v == 0).unwrap_or(raw.len())])
}
fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain([0]).collect()
}
fn current(name: &str) -> Result<DEVMODEW> {
    display_mode(
        PCWSTR(wide(name).as_ptr()),
        ENUM_CURRENT_SETTINGS,
        EDS_RAWMODE,
    )
    .ok_or_else(|| Error::Failed(format!("屏幕 {name} 当前不可用")))
}
fn stage(name: &str, mode: &DEVMODEW) -> Result<()> {
    let wide = wide(name);
    // SAFETY: the name and DEVMODE remain alive through this staged change.
    let result = unsafe {
        ChangeDisplaySettingsExW(
            PCWSTR(wide.as_ptr()),
            Some(mode),
            None,
            CDS_UPDATEREGISTRY | CDS_NORESET,
            None,
        )
    };
    if result != DISP_CHANGE_SUCCESSFUL {
        return Err(Error::Failed(format!(
            "暂存虚拟屏失败：{name}（{}）",
            result.0
        )));
    }
    Ok(())
}
fn commit() -> Result<()> {
    // SAFETY: commit the staged topology without borrowed buffers.
    let result = unsafe { ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None) };
    if result != DISP_CHANGE_SUCCESSFUL {
        return Err(Error::Failed(format!("提交屏幕布局失败（{}）", result.0)));
    }
    Ok(())
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key belongs exclusively to this guard and is closed once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
fn key() -> Result<Key> {
    let mut handle = HKEY::default();
    // SAFETY: only this user's ScreenFerry display transaction is opened/created.
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\ScreenFerry\\DisplaySession"),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        )
    }
    .ok()
    .map_err(|e| os("打开屏幕恢复记录", e))?;
    Ok(Key(handle))
}
fn save(screens: &[Screen]) -> Result<()> {
    let bytes = serde_json::to_vec(screens).map_err(|e| Error::Failed(e.to_string()))?;
    let key = key()?;
    // SAFETY: bytes point to a valid serialized transaction, owned for the call.
    unsafe { RegSetValueExW(key.0, w!("Topology"), None, REG_BINARY, Some(&bytes)) }
        .ok()
        .map_err(|e| os("保存屏幕恢复记录", e))
}
fn load() -> Result<Option<Vec<Screen>>> {
    let key = key()?;
    let mut size = 0;
    let mut kind = REG_VALUE_TYPE::default();
    // SAFETY: initial call queries size and type only.
    let result = unsafe {
        RegQueryValueExW(
            key.0,
            w!("Topology"),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    result.ok().map_err(|e| os("查询屏幕恢复记录", e))?;
    if kind != REG_BINARY || size > 65536 {
        return Err(Error::Failed("屏幕恢复记录无效".into()));
    }
    let mut bytes = vec![0; size as usize];
    // SAFETY: allocated output matches the queried bounded size.
    unsafe {
        RegQueryValueExW(
            key.0,
            w!("Topology"),
            None,
            None,
            Some(bytes.as_mut_ptr()),
            Some(&mut size),
        )
    }
    .ok()
    .map_err(|e| os("读取屏幕恢复记录", e))?;
    bytes.truncate(size as usize);
    let screens: Vec<Screen> =
        serde_json::from_slice(&bytes).map_err(|e| Error::Failed(e.to_string()))?;
    if screens.is_empty()
        || screens.len() > 64
        || screens.iter().any(|s| !s.name.starts_with(r"\\.\DISPLAY"))
    {
        return Err(Error::Failed("屏幕恢复记录无效".into()));
    }
    Ok(Some(screens))
}
fn clear() -> Result<()> {
    let key = key()?;
    // SAFETY: remove only this user's completed display transaction value.
    let result = unsafe { RegDeleteValueW(key.0, w!("Topology")) };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    result.ok().map_err(|e| os("清除屏幕恢复记录", e))
}

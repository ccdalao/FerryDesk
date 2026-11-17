//! 对已激活的输出以 CCD 一次提交完整坐标；原点对应 Windows 主屏。

use ferry_platform::{Error, Result};
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, POINTL};

pub(super) fn apply(positions: &[(String, POINTL)], remove: Option<&str>) -> Result<()> {
    // SAFETY: all arrays have bounded sizes returned by Windows. Mode indices
    // are checked before accessing sourceMode. Active paths are preserved except
    // for the explicitly named session output; no other outputs are disabled.
    unsafe {
        for _ in 0..4 {
            let (mut path_count, mut mode_count) = (0, 0);
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
                .ok()
                .map_err(|e| super::os("查询显示路径大小", e))?;
            if path_count == 0 || path_count > 256 || mode_count > 1024 {
                return Err(Error::Failed("Windows 显示路径数量无效".into()));
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let queried = QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            );
            if queried == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            queried.ok().map_err(|e| super::os("查询当前显示路径", e))?;
            paths.truncate(path_count as usize);
            modes.truncate(mode_count as usize);
            let mut selected = Vec::new();
            for path in paths {
                let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                    header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                        r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                        size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                        adapterId: path.sourceInfo.adapterId,
                        id: path.sourceInfo.id,
                    },
                    ..Default::default()
                };
                let queried = DisplayConfigGetDeviceInfo(&mut source.header);
                if queried != 0 {
                    return Err(Error::Failed(format!("查询显示路径名称失败：{queried}")));
                }
                let name = super::name(&source.viewGdiDeviceName);
                if remove == Some(name.as_str()) {
                    continue;
                }
                let index = path.sourceInfo.Anonymous.modeInfoIdx as usize;
                let mode = modes
                    .get_mut(index)
                    .filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
                    .ok_or_else(|| Error::Failed("显示路径缺少有效来源模式".into()))?;
                if let Some((_, position)) =
                    positions.iter().find(|(requested, _)| *requested == name)
                {
                    mode.Anonymous.sourceMode.position = *position;
                }
                selected.push(path);
            }
            if selected.is_empty() {
                return Err(Error::Failed("不能移除全部显示路径".into()));
            }
            selected.sort_by_key(|path| {
                let mode = &modes[path.sourceInfo.Anonymous.modeInfoIdx as usize];
                let point = mode.Anonymous.sourceMode.position;
                point.x != 0 || point.y != 0
            });
            let result = SetDisplayConfig(
                Some(&selected),
                Some(&modes),
                SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE,
            );
            if result != 0 {
                return Err(Error::Failed(format!(
                    "提交 Windows 显示布局失败：{result}"
                )));
            }
            return Ok(());
        }
    }
    Err(Error::Failed("显示布局持续变化，请稍后重试".into()))
}

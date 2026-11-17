# Windows 扩展屏鼠标回传回归

## 复现依据

2026-11-17，旧版 0.3.0 的真实 Win32 窗口测试复现了接收端静止时来源鼠标被重置到屏幕中央的路径。

单独刷新 Viewer 的对照组没有回传移动。在同一桌面创建与 CursorHider 相同的 1×1 不激活置顶窗口，与 Viewer 交替调整层级，40 轮产生 39 条 `PointerMotion(960,540)`。测试没有移动硬件鼠标。

用这些消息更新模拟来源位置时，来源位置从 `(1517,733)` 被覆盖为 `(960,540)`。这是实际 Windows 消息与生产 Viewer 的实验，来源端坐标在该测试中是模型，没有模拟成一次完整的双机实测。

对消息队列即时调用 `GetCurrentInputMessageSource`，上述消息的来源是 `IMO_SYSTEM`，设备类型为 `IMDT_UNAVAILABLE`。

旧版日志保存在本机构建缓存 `target/tmp/pointer-before.log` 与 `target/tmp/pointer-before-origin.log`。修复后的同场景在 `target/tmp/pointer-after.log`，回传鼠标事件为 0，来源位置模型保持不变。

## 修复约束

- 窗口出现、层级变化和隐藏器维护都不代表用户的控制意图，不能生成网络输入。
- 接收窗口在 `PeekMessage` 后即时检查来源，只接纳 Windows 标识的硬件输入。
- 本程序通过 `SendInput` 注入的键鼠带固定 `dwExtraInfo` 标记。即使 UIAccess 将注入分类为硬件，也禁止再次回传。
- 隐藏器仅在显式隐藏时停车，定时维护只调整窗口与透明光标，不再定位。
- 不使用不平衡的 `ShowCursor` 计数隐藏光标。
- 仅在桌面来源端排除普通共享到同一设备的边缘路由。接收端保留通往来源实体屏的普通共享通道，避免反向扩展无法跨屏。

## 运行回归

```powershell
cargo test -p ferry-platform-windows --test suite display_stream::stationary_viewer_does_not_overwrite_source_pointer -- --ignored --exact --nocapture --test-threads=1
```

该测试临时显示接收窗口并保存和恢复本机光标，请保持硬件鼠标静止。未使用网络或修改配对。

## 实体鼠标正向测试

用户配合运行 `physical_viewer_motion_and_drag_are_forwarded`。首次在旧后台共享仍活动时收到真实移动，但未收齐拖动序列，未将其视为通过。随后通过现有 IPC 临时暂停旧共享以隔离后台，测试结束自动恢复原共享状态。

隔离测试记录到 426 次连续真实移动、1 次左键按下、99 次按住期间的移动、1 次左键松开，测试通过。原始鼠标消息的 `originId=IMO_HARDWARE`，`deviceType=IMDT_MOUSE`。测试恢复前台窗口和光标。日志位于本机构建缓存 `target/tmp/v031-physical-mouse-diagnostic.log`。

反向默认布局和上方布局的引擎集成测试也已验证通过普通共享进入来源实体屏并返回。该项使用模拟平台。

## 验证边界

自动化的系统消息回归和单机实体鼠标测试不能证明完整双机交互已经验收。安装两端后，还需分别验证来源电脑的物理鼠标自由跨入虚拟屏，以及接收电脑的本机鼠标操作扩展窗口、按住拖动与松开，并重复两种方向和停止恢复。普通测试进程的注入过滤已经验证，尚未在具有 UIAccess 的测试进程中独立验证标签过滤分支。

接收窗口有意拒绝普通非 UIAccess 宏程序和第三方远控通过 SendInput 注入的操作，避免它们形成再次回传。

## API 依据

- [Microsoft 鼠标移动说明](https://learn.microsoft.com/en-us/windows/win32/learnwin32/mouse-movement) 说明窗口变化也会产生鼠标移动消息，即使鼠标没有移动。
- [GetCurrentInputMessageSource](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getcurrentinputmessagesource) 提供当前输入来源。
- [INPUT_MESSAGE_ORIGIN_ID](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ne-winuser-input_message_origin_id) 区分硬件、注入与系统来源，UIAccess 的注入可能标记为硬件。

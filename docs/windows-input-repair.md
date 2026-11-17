# Windows 键鼠权限与延迟修复

这批修改保留键位映射、屏幕布局、坐标换算和现有线协议。修复对象为主控捕获权限、被控服务身份核验，以及输入后台的阻塞和无线低延迟提示。

## 三种情形

| 情形 | 处理方式 |
|---|---|
| 主控的管理员窗口在前台，鼠标无法切到被控端 | 主控使用 `screenferry elevate`，经本机 UAC 确认，以管理员权限运行捕获后台 |
| 被控端管理员窗口或 UAC 提示出现，拒绝进入 | 被控端部署修复的服务程序，并单独授权主控设备；普通配对不等于 UAC 授权 |
| 热点无线链路出现 100–200 ms 尖峰 | 申请 WLAN 媒体流模式、关闭本输入进程的执行速度节流，隔离剪贴板阻塞；用同步 ICMP 与应用 RTT 检查实际效果 |

管理员模式针对普通桌面上的管理员窗口。主控自身的 UAC 安全桌面捕获仍不属于当前输入代理的职责。管理员后台会受到 Windows 对普通资源管理器拖放到管理员窗口的限制，文件仍可用 `send` 发送。

## 修复的依据

- [微软 Mouse Without Borders 服务模式](https://learn.microsoft.com/en-us/windows/powertoys/mouse-without-borders)：SYSTEM 服务可处理被控端管理员程序和锁屏。
- [OpenProcessToken](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-openprocesstoken)：进程句柄查询权限与令牌自己的 DACL 是两个检查。服务在代理启动前，对实际主令牌设置登录用户的 `TOKEN_QUERY`，不授予复制、修改或冒充权限。
- [CREATOR OWNER](https://learn.microsoft.com/en-us/windows/win32/secauthz/well-known-sids)：Windows 默认 `HKLM\SOFTWARE` 上的可继承占位 SID，不应被误判成普通用户对现有对象的写权限；现有对象 owner 仍必须是可信管理员或 SYSTEM。
- [桌面访问权](https://learn.microsoft.com/en-us/windows/win32/winstation/desktop-security-and-access-rights)：代理打开当前输入桌面时申请 `DESKTOP_JOURNALPLAYBACK`；仅 READ/WRITEOBJECTS 不足以让 `SendInput` 执行输入。
- [WLAN_INTF_OPCODE](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/ne-wlanapi-wlan_intf_opcode)：媒体流模式按 WLAN 客户端申请，断开连接时自动清除；本实现重连后再次申请，退出时释放请求，不关闭系统自动连接或修改网卡永久配置。
- [SetProcessInformation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessinformation)：对输入后台明确关闭执行速度节流。

## 构建与部署

构建后台和服务，不需要 Flutter：

```powershell
cargo build --release --locked -p ferry-daemon -p ferry-service
```

本批验证包在 `target/repair/`，包含两个新 EXE、原项目许可证和本说明，不是正式安装器。已有 Flutter 设置界面可以继续使用，因为没有改变设置界面 IPC。

可直接升级的第二轮安装包在 `target/installer/ScreenFerry-0.2.0-repair2-windows-x64-setup.exe`。安装器沿用原 AppId，保留配置、配对及以前的安装任务选择。设置界面来自本机已安装的同版 0.2.0；后台与服务由本次源码重新构建。安装后开始菜单新增“ScreenFerry（管理员模式）”，供主控使用，无须手敲 `elevate`。被控端要保留“允许已配对的电脑操作 Windows 权限提示（UAC）”安装选项，并完成设备授权。

包内 `Install-Repair.ps1` 会核对程序 SHA-256、正常退出旧后台、备份旧 EXE 后替换；失败时尝试恢复旧程序。请先在解压目录打开管理员 PowerShell，用原登录账户提升权限：

```powershell
# 主控（默认从正在运行的进程识别安装目录）
.\Install-Repair.ps1 -Mode Controller -WhatIf
.\Install-Repair.ps1 -Mode Controller

# 被控端（已经安装安全输入服务时使用）
.\Install-Repair.ps1 -Mode Receiver -WhatIf
.\Install-Repair.ps1 -Mode Receiver
```

可以用 `-InstallPath '实际安装目录'` 明确指定后台位置。服务始终放在系统盘受保护的默认目录。脚本不删除配置或配对、不自动重启后台；完成后由原普通用户打开原快捷方式，主控再运行下述 `elevate`。已有自定义数据目录的实例请先手动退出。

先退出原后台，再将对应程序复制到原安装位置。部署被控端服务需要在管理员终端停止旧服务后替换，保留旧 EXE 以便回退。不要只从解压目录注册服务：服务映像必须位于受保护的系统盘 `Program Files\ScreenFerry`。

### 主控管理员窗口

将修复的 `screenferry.exe` 放回原位置，在普通终端执行：

```powershell
& '实际路径\screenferry.exe' elevate
```

由 Windows 弹出 UAC 确认；取消时原后台继续运行，批准并启动管理员子进程后才请求旧后台退出。请使用原登录账户提升权限；用另一管理员账户启动会改变用户配置和身份环境。默认启动模式和开机启动方式不变。

### 被控端服务与授权

主控正在运行时，从普通 PowerShell 找到真实路径并导出公开授权码：

```powershell
$sf = (Get-Process -Name screenferry -ErrorAction Stop | Select-Object -First 1).Path
& $sf input-key
```

授权码由设备指纹、公钥组成，不含私钥或密码。被控端管理员通过可信渠道核对完整输出。被控端普通终端用 `whoami /user` 取得实际运行后台的用户 SID，管理员终端执行：

```powershell
& "$env:ProgramFiles\ScreenFerry\screenferry-service.exe" authorize '<普通用户SID>' '<主控公开授权码>'
```

重新连接后协商安全输入能力。没有授权时继续按普通权限连接，无法操作 UAC。详细验收见 [Windows 安全输入验证](input-troubleshooting.md)。

## 本次验证与实测边界

- 慢剪贴板读取 2 秒时输入、心跳仍然可用的真实 TLS 回归场景通过。
- Windows/MSVC 核心编译、完整工作区 Clippy、工程结构检查通过；后台和服务的 release 优化构建成功。
- 第二轮新增真实 Windows 默认注册表 ACL 的只读回归测试及非法身份参数测试，完整工作区 387 项通过、2 项原有测试忽略。
- 原安装会话的日志出现安全输入身份查询拒绝访问，以及 1 秒 55 次管理员窗口输入拒绝。
- 原安装会话同步短测：21 个 ICMP 样本无丢包，最高 237 ms；同窗口应用 RTT 最高 221.90 ms。这说明尖峰至少涉及网络路径或端点响应，不能归因于映射逻辑。
- WLAN API 在本机成功接受媒体流请求。另一次短测基线最大 4 ms、媒体流阶段最大 16 ms，两阶段均无超过 20 ms 的尖峰，因此这次样本不能证明优化幅度。
- 2026-11-14 已在本机授权主控设备并部署修复：已授权设备健康检查和 Bind 返回成功与挑战；未知设备延迟 100 ms 读取仍收到 Unauthorized。补齐桌面回放访问权后，用户双机实测确认“现在可以操作 UAC”。这是本机的一次交互验收，不代表全部锁屏、不同账户或桌面切换压力测试已完成。
- 不将编译、模拟平台测试或短时间无尖峰等同于延迟已彻底消失。仍需记录最小化、断网、停止服务的结果及更长时间的延迟分布。

### UAC 第二轮故障定位

第一次安装修复版后，“令牌身份查询拒绝访问”消失，但仍无法控制 UAC。进一步实测发现：

1. `authorize` 被默认注册表 `CREATOR OWNER` ACE 误判挡住，没有生成管理员授权。
2. 代理发出 Unauthorized 后立即 `DisconnectNamedPipe`，尚未读取的回复被丢弃，后台只看到“管道另一端无进程”。现等待客户端读完关闭或租约回收，不使用可能无限等待的 FlushFileBuffers。
3. 授权成功后，服务事件日志揭示 `SendInput` 返回访问拒绝。输入桌面句柄漏申请 `DESKTOP_JOURNALPLAYBACK`，补齐后双机 UAC 操作实测通过。

诊断事件位于 Windows “应用程序”日志，源为 `ScreenFerryInput`，仅含阶段和错误，不记录键鼠正文。

### 管理员窗口与副屏切换补充（2026-11-14）

普通共享和副屏都遇到管理员窗口时，两台电脑各自从托盘选择“以管理员权限重启后台”，使用原登录账户确认 UAC。托盘显示当前管理员状态；取消 UAC 时旧后台继续运行，设置界面经 Explorer 保持普通权限。本次运行结束后恢复默认启动权限，不创建永久提权计划任务。主控捕获与副屏直接输入使用后台令牌；已授权的 SYSTEM 普通共享输入通道继续使用原签名与租约机制。副屏视频不采集 UAC 安全桌面。

未提权的副屏输入遇到管理员窗口时，明确提示权限不足并退出本次拓展，随后回收媒体窗口、虚拟屏和光标。两个方向切换会先等待本机旧任务完成回收再发起新会话，反向拓展保留进入被控实体屏幕的普通贴边通道。接收窗口隐藏本地重复光标，DXGI 按光标是否已在图像中决定是否另行合成。

驱动保留安装，虚拟显示目标只在拓展期间启用。后台启动及没有活动拓展时每 3 秒检查一次本程序拥有的残留目标；普通共享布局排除该目标。实际子进程创建虚拟屏后直接退出的回归已通过：退出后 Windows 显示范围确实仍扩展，调用恢复后回到原范围。空闲、无配对设备时的自动恢复模拟测试通过；7 项 Windows 显示显式测试全部通过。以上不能替代两台电脑上管理员窗口、UAC 和连续换向的交互验收。

优先比较同一时间窗口的 ICMP RTT、应用 RTT 和日志 `slowest_injection_ms`，再分别排查无线/驱动、运行时等待和 Windows 输入注入。单独“连接正常”不能证明输入权限可用。

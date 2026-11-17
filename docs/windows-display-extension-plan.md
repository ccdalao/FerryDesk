# Windows 双机扩展屏方案

本文前半部分保留初始选型和验收目标；当前实现及实测边界见末尾“原生集成进度”，Apollo 复用决策另见 [ADR 0018](adr/0018-reuse-apollo-native-media-core.md)。

## 第一版体验

第一版面向 Windows ↔ Windows，把「1K 100 Hz」按 **1920×1080、100 Hz** 规划。主控和被控可以交换谁提供屏幕，键鼠角色由当前 ScreenFerry 会话决定。一次开启一个方向、借用对方的一块屏幕，两台电脑各自继续运行原来的 Windows 桌面。

| 设备菜单里的操作 | Windows 在哪里新增扩展屏 | 画面在哪里显示 |
|---|---|---|
| 用被控屏幕扩展主控 | 主控 | 被控选定的物理屏 |
| 用主控屏幕扩展被控 | 被控 | 主控选定的物理屏 |

在扩展桌面的一端，Windows 显示设置可排列新增的屏幕，应用窗口可以拖过去。提供屏幕的一端用无边框窗口显示画面。Windows 虚拟显示驱动在系统上安装一次，设备信任沿用 ScreenFerry 已有配对。设备页和托盘都提供三个独立操作：用被控屏扩展主控、用主控屏扩展被控、取消副屏拓展。三个操作分别支持设置全局快捷键。设备页显示当前方向、实际帧率和声音开关。互换方向时先结束原会话，再启动反向会话。

## 开源项目选型

| 项目 | 已核对的实现 | 在 ScreenFerry 中的用途 |
|---|---|---|
| Osplay（MIT） | Windows ↔ Windows 可用，扩展端用 Virtual Display Driver，Windows 使用 Media Foundation 硬件 H.264。其构建文档记载视频流目前最高 60 Hz。 [项目说明](https://github.com/akinalpfdn/osplay) · [Windows 验证记录](https://github.com/akinalpfdn/osplay/blob/main/docs/BUILDING.md) | 最贴近需求的产品参考。借鉴双向入口、虚拟屏生命周期和本机原生接收端。 |
| Apollo + Moonlight PC（GPL-3.0） | Apollo 用 SudoVDA 按需创建、移除虚拟屏，并给每个客户端固定显示器身份。Moonlight PC 官方标明最高 120 FPS。 [Apollo](https://github.com/ClassicOldSong/Apollo) · [Moonlight PC](https://github.com/moonlight-stream/moonlight-qt) · [Moonlight 高刷新说明](https://moonlight-stream.org/) | 优先用独立程序实测高刷新率、尾延迟及重连后的布局。准确记录客户端实际可选帧率和虚拟屏模式，两者均支持时测 100，否则记录实际模式。 |
| ScreenExtended（MIT） | Windows 虚拟屏、WebRTC 浏览器接收、WASAPI 回录复制声音。 [项目说明](https://github.com/MOAboAli/ScreenExtended) | 参考 WebRTC 和音频流程。当前资料未验证 Windows 双机可稳定到 1080p100，第一版计划采用原生窗口接收。 |
| Virtual Display Driver（MIT） | 独立的 Windows 10/11 虚拟显示组件，支持自定义分辨率和刷新率。Osplay 记录了打包指定版本签名驱动的做法。 [驱动项目](https://github.com/VirtualDrivers/Virtual-Display-Driver) · [Osplay 第三方组件记录](https://github.com/akinalpfdn/osplay/blob/main/docs/THIRD_PARTY.md) | 首选驱动候选。先验证指定驱动版本能否按会话增删屏幕、稳定复用屏幕身份、协商 1080p100，并在两机 Windows 上正常加载。 |

优先验证 **Virtual Display Driver + ScreenFerry 原生音视频**。Osplay 提供最相近的使用流程，Apollo + Moonlight 提供高刷实测基准。Apollo 和 Moonlight 作为独立测试工具使用。正式版锁定驱动及媒体依赖的版本，核对实际二进制、驱动目录签名、再分发条款和许可证随附要求。驱动加载单独在启用 Secure Boot 的干净机器上验证，沿用系统现有安全设置。只有现成签名驱动无法满足会话控制时，再评估基于 IddCx 自建驱动和正式签名的成本。 [微软 IddCx 文档](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview) · [微软驱动签名文档](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation)

## 集成到 ScreenFerry

### 一键启动与恢复

1. 双方经已配对的 TLS 会话协商扩展屏能力，检查驱动、GPU 编解码、目标物理屏当前刷新率和媒体连通性。使用现有设备证书及其信任记录，无需额外账号或配对码。启动只能针对已连上的设备，取消配对时一并关闭活动扩展屏。
2. 在桌面所属电脑按对端设备保留稳定的虚拟屏身份，创建时检查 Windows 已启用「扩展这些显示器」。只采集该虚拟屏。第一帧正确显示后才启用对应的输入路由、进入全屏。
3. 失败、停止或掉线时，两端独立结束会话，释放按键状态与鼠标捕获，退出全屏并移除本会话创建的屏幕。Windows 主屏始终可用。测试窗口能否回到本地可见区域，以及重连时能否恢复虚拟屏的位置和缩放。启动阶段设超时，断线允许短暂恢复，持续断线由看门狗清理。锁屏和桌面切换时暂停采集并归还本地输入，恢复后重新确认捕获目标。

### 画面、网络与键鼠

- **显示与采集**。Windows 一侧经 IddCx 驱动获得新的显示目标。原型先试 Windows Graphics Capture 获取该目标的 D3D11 纹理，同机对照 DXGI Desktop Duplication 的兼容性和帧间隔。虚拟屏建立、采集和编码均要在目标 GPU 上实测，尤其是双显卡设备。 [Windows Graphics Capture](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture) · [DXGI Desktop Duplication](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/desktop-duplication-api)
- **视频处理**。用 Media Foundation 硬件 H.264 低延迟编码，关闭 B 帧和前瞻（硬件接受时），尽量保持 GPU 纹理路径。接收端用硬件解码和原生 D3D11 窗口呈现。捕获端只在有变化时送帧并限制队列，接收端跳过过期的待呈现帧。解码参考链损坏时请求新关键帧。把 Windows 的虚拟屏刷新率、接收端实际呈现帧率和物理面板刷新率分别显示，硬件或链路不足时明确降至 90 或 60 FPS。 [微软 H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder) · [微软低延迟模式](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avlowlatencymode)
- **局域网连接**。发现、手动地址连接、配对、证书信任继续使用现有流程。带来新消息时增加能力位，老版本仍能做原有键鼠共享，沿用 `WIRE_FORMAT`。启动、停止和媒体协商走现有已认证的数据连接。独立媒体路径首选局域网直连 WebRTC（DTLS-SRTP/UDP），在已认证的 TLS 会话中绑定连接参数和媒体证书指纹，使用本地候选地址。先评估与现有 Rust 工程契合的 `webrtc-rs`，验证反馈、限速和长跑再定库。另用独立 TCP/TLS 媒体连接做同机对照，测丢包时的长尾。UDP 不通时给出原因并让原有键鼠会话继续。媒体码率主动降载，输入与媒体连接分开后仍要测共享热点上的带宽争用。 [ScreenFerry 线协议](protocol.md) · [webrtc-rs](https://github.com/webrtc-rs/webrtc)
- **键鼠路由**。扩展主控时，新增屏幕本来就在主控的 Windows 坐标系，主控鼠标直接从本机桌面走进虚拟屏。扩展被控时，主控接收窗口把有效画面内的坐标映射到被控虚拟屏，经现有输入通道传递；要覆盖 DPI 缩放、黑边、负坐标和多屏排列。只在新模式下让该会话的虚拟屏与接收窗口参与路由，临时避开同一物理边界上冲突的跨机切换。普通键鼠共享继续使用原来的布局规则、HID 映射、点击绝对坐标和 UAC 输入权限。接收窗口失焦、换向、停止或链路中断时释放已按下的键和鼠标键。视频采集在交互用户会话运行，安全桌面暂停画面，并保留本地退出入口。 [ScreenFerry 架构](architecture.md) · [安全输入设计](adr/0016-authenticate-secure-input-origin.md)

### 声音

声音跟着扩展桌面的所有者发往接收端，第一版默认关闭，开启后使用 WASAPI 回录该端选定播放设备的混音，以 Opus 立体声随视频发送。两端都能播放，接收端可独立静音和调音量；发送端切换默认播放设备时重绑定。用时间戳对齐音画并限制抖动缓冲，一端收到的音频只在本机播放。

例如被控电脑把浏览器拖到虚拟屏上，声音仍属于被控电脑的播放设备。第一版的「传声音」会同时传该播放设备上其他应用的声音。后续可以按浏览器等应用进程筛选，需按 Windows 构建号检查支持。只让声音在接收端响、发送端本地静音，可后续通过独立音频端点评估。 [WASAPI 回录](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording) · [微软按进程回录示例](https://learn.microsoft.com/en-us/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/)

### 仓库落点

后续实现由 `ferry-platform` 定义新增能力接口，`ferry-platform-windows` 操作驱动、采集、编解码和播放，macOS 初期报告平台能力不可用，mock 用于断线等场景测试。`ferry-proto` 定义能力和控制消息，`ferry-engine` 负责媒体会话、角色和回收，后台组装 Windows 实现。设置页经现有本机 IPC 发起操作，原生接收窗口由 Windows 端负责。音视频传输与现有 TCP 双通道的关系先写 ADR，由维护者确认后进入实现；本方案目前为选型草案。 [ScreenFerry 架构](architecture.md) · [TCP 传输决策](adr/0001-use-tcp-transport.md)

## 双机基准与验收

现有热点曾在同一短测窗口出现 ICMP 最大 237 ms、ScreenFerry RTT 最大 221.90 ms。两个数都是往返指标，音视频验收另测端到端。先用双有线跑基线，再用「主控有线、被控连主控热点」原样复测。每种网络测空闲、持续拖动窗口或滚动文字、播放带声视频、并发传文件和干扰恢复，两个方向分别记录。 [既有双机测试记录](windows-input-repair.md)

| 项目 | 第一版验收目标或测量方法 |
|---|---|
| 真扩展、双向切换 | 两边都能在 Windows 显示设置看到新增屏，各方向拖动窗口和操作文字。设置窗口关闭后仍可使用。反复启停、换向共 50 次，检查屏幕残留、窗口丢失、卡键和鼠标归还。 |
| 1080p100 | 使用接收端至少 100 Hz 的屏幕和两端支持该吞吐的 GPU。确认虚拟显示模式是 1920×1080 @ 100 Hz，连续变化画面每方向跑 10 分钟，平均至少 95 个不同的实际呈现帧/秒。报告 P95/P99 帧间隔、掉帧数及超过 100 ms 的冻结次数。 |
| 画面延迟 | 有线初始目标为输入动作到接收屏变化的 P95 ≤ 50 ms。用高帧率相机同时拍可识别的输入触发和接收屏，按方向分别测量。同步记录采集、编码、收到包、解码及呈现耗时。热点单列 P50/P95/P99，以及超过 100 ms 的次数和恢复时间。 |
| 键鼠优先 | 每种网络先记录原有 ScreenFerry 无媒体时的输入通道 RTT，再测串流、音频及文件传输。P95 增量初始目标 ≤ 5 ms；另实测点击、滚轮、松键和退出快捷键。原来的管理员窗口和 UAC 键鼠用法做回归。 |
| 音画与长跑 | 有线播放带闪光和短音的测试素材，绝对音画偏差初始目标 P95 ≤ 40 ms。持续运行至少 1 小时，检查热降频、内存和音画漂移；验证耳机切换、静音和结束后停止播放。 |
| 故障恢复 | 模拟热点中断、接收进程退出、GPU 重置和显示器拔插，检查两端能自行释放输入及清理虚拟屏。另测对端版本旧、驱动缺失和媒体端口不可达。 |

实施顺序为 **开源基准 → 单向视频原型 → 一键双向和键鼠联动 → 加入声音 → 双机压力测试与安装包验证**。先用两个方向的基准确认接收面板、GPU 和热点能支撑的档位，并以实际测得的帧率、尾延迟和恢复能力确定第一版默认配置。本文的性能数字为验收目标，目前尚未在你的两台电脑上测量新功能。

## 原生集成进度（2026-11-14）

最新生命周期修复：发起新方向前等待本机旧窗口及虚拟屏完整释放；源在主控时暂停普通贴边，主控显示被控的扩展桌面时保留进入被控实体屏幕的通道。接收窗口只显示视频中的光标，避免同时出现本地光标。启动及空闲时回收本程序异常遗留的显示目标，未连接设备也不会一直保留该目标。已经通过子进程异常退出后的真实显示范围恢复、连续启停及单光标验证；双机管理员窗口及长时间换向仍需交互验收。管理员后台入口和限制见 ADR 0019。

已经实现现有证书配对授权、独立 TLS 媒体连接、两个拓展方向与取消、设置页及托盘入口、可配置快捷键、系统 H.264、可选 WASAPI 声音回录和播放。安装包内含固定版本的签名 Virtual Display Driver，不启动或调用 Apollo、Moonlight。

此前已交付的慢版本使用 GDI 捕获、CPU 转换和系统软件 H.264。本轮改为 DXGI 桌面纹理 → GPU NV12 转换 → Apollo v0.4.6 原生 D3D11 NVENC → 独立 TLS 媒体连接 → 系统 D3D11 解码及交换链呈现。其他显卡尝试系统硬件编码，跨 GPU 时允许受控的纹理读回；不可用的硬件路径会记录原因。原生 NVENC 源码按原文导入，并保留来源、提交、哈希和许可证，整合程序按已确认的 GPLv3 分发。

媒体发送端限制在途帧数，接收和输入发送分离，避免积压画面阻塞输入。日志分别记录编码后端、采集编码统计和解码/呈现提交统计。目前仍使用 ScreenFerry 自身的 TLS 媒体协议，尚未复用 Apollo 的 UDP/FEC 或 Moonlight 接收协议，也没有动态码率。因此，不能把复用 NVENC 或虚拟屏 100 Hz 当作已经复现完整 Apollo 的双机效果。

本机独立 10 秒测试实际启用 Apollo NVENC，记录 919 次成功呈现提交（约 91.9 次/秒），采集调用时间戳至呈现调用返回的 P95 约 6.31ms。该数字来自源实例自身时钟，不经过网络，也不包含显示器扫描，不能代表两台电脑的实际可见帧率或端到端延迟。连续启停另做回归，不能用这次独立短测替代稳定性验收。

已完成 Rust 整仓测试、Clippy、结构检查、依赖安全与许可证检查，以及 Windows 本机 1080p100 编解码往返。界面操作测试验证了两个方向和取消请求；Flutter 静态检查通过。Flutter 整体测试为 116 项通过、1 项失败，失败项是原有 macOS 安装路径测试在 Windows 上的路径判断，本次未修改该测试或启动器。

下一轮需要在两台电脑上安装同一开发安装包，并选中“启用双机扩展屏”。先确认两个方向的 Windows 显示设置均出现新屏、能拖动窗口、接收窗口键鼠可用；再测取消、断线、连续换向和声音。首轮通过后，按上表进行热点持续画面测试。新实现的实际帧率、端到端延迟、驱动安装/卸载和音画同步仍待实机验收。

声音开关应用于下一次拓展。快捷键使用本机 `Ctrl+Alt+Shift+0–9`，默认关闭。接收窗口优先选择非主屏，没有时使用本机第一块屏幕，当前尚未提供选屏入口。UAC/锁屏期间暂停这条视频和输入通道，原有安全输入服务保持独立。

### 首次驱动安装修复

两台首次安装均报告失败。被控端复现记录显示 `0xE0000242`，SetupAPI 明确记录发布者尚未建立信任。原安装调用使用 `INSTALLFLAG_NONINTERACTIVE`，禁止了 Windows 首次发布者确认。修正为交互式管理员安装后，用户确认 Windows 驱动发布者，命令返回 0，`ROOT\SCREENFERRYDISPLAY\0000` 的 Virtual Display Driver 状态正常，初始化移出桌面完成。没有自动导入发布者或根证书。

安装结果另写入驱动目录 `install-result.txt`，后续失败提示显示该路径。主控端仍需用修正安装包重试；这次验证尚不代表已完成双机串流及性能验收。

安装成功后，在被控端运行真实驱动生命周期测试：创建虚拟屏、捕获并编码一个视频帧、结束后恢复原显示范围，测试通过（约 2.3 秒）。该测试作为需要已安装驱动的显式忽略测试保留，普通测试运行不会改变本机显示布局。

- [微软：驱动安装非交互标志遇到需要 UI 时直接失败](https://learn.microsoft.com/en-us/windows/win32/api/newdev/nf-newdev-updatedriverforplugandplaydevicesw)

# Windows 局域网扩展屏延迟修复与验证

日期：2026-11-15。适用于两个扩展方向，继续使用 ScreenFerry 的配对及独立 TCP/TLS 媒体连接。

## 实际发现

本机连接主控热点。80 次 ICMP 请求没有丢包：最初 20 次小包平均 17.30ms，其中两次为 133/184ms；后续 40 次小包平均 2.17ms、P95 4ms；20 次 1400 字节包平均 1.65ms。到主控的路由直接经过 WLAN，没有经过本机代理的 TUN。这是空闲 ICMP 测量，不能代替满载吞吐或视频延迟测量。

最近反向扩展日志已经是 DXGI、Apollo NVENC、GPU NV12，并非一直退回软件编码。发送多数为 95–97fps，也有约 51fps 的时间段；发送帧率不包含热点排队、解码、显示及输入响应，不能用它证明流畅。

代码中发现的问题：

- 来源端等待整帧写入时不读取回传输入，写超时 250ms 会直接结束扩展。
- 来源端提前编码并保存 pending/channel 帧，接收端入队后就确认，慢呈现时会保留多幅旧画面。
- 输入队列满就结束扩展。高报告率鼠标和慢视频可以共同触发，表现为软件仍运行但扩展退出。
- 接收端使用不合适的交换链延迟设置，且未按接收屏幕选择 GPU。
- DXGI 在未更新鼠标时返回的默认可见字段被当成光标状态更新，导致闪烁。

## 开源实现对照

- [Sunshine Windows 捕获](https://github.com/LizardByte/Sunshine/blob/master/src/platform/windows/display_base.cpp)：限制采集节奏，显示切换时重建捕获。
- [Moonlight D3D11 渲染](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/d3d11va.cpp)：根据显示 GPU 建立解码/呈现，使用 SyncInterval 0、足够的交换缓冲及受支持时的 tearing，避免不必要的 DWM 等待。
- [Moonlight 呈现队列](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/pacer/pacer.cpp)：限制旧画面排队。
- [Moonlight 解码恢复](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/VideoDepacketizer.c) 与 [Apollo 编码](https://github.com/ClassicOldSong/Apollo/blob/v0.4.6/src/video.cpp)：压缩参考链不能任意丢弃；恢复须通过 IDR 等机制重新同步。
- [Sunshine 网络排查](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html)：局域网稳定性、抖动及突发流量也会影响串流；不是只看带宽或空闲 ping。

本轮按这些处理原则修改 ScreenFerry 自身调度和适配，不引入其他软件的登录、服务或独立安装。原 Apollo NVENC 导入文件保持原文。

## 修改

1. 来源端读写独立运行，视频写阻塞不再阻止回传输入到达媒体线程。
2. 采集编码前取得额度，最多两个视频编码单元等待消费确认；没有额外的预编码 pending 帧。接收端将 Ping 标记与帧按序消费后回 Pong，不以“入队”代替“已消费”。此统计不是物理屏幕延迟。
3. 连续绝对鼠标移动合并为最新坐标，不跨越按键、按钮、滚轮合并。限制每次 Windows 消息泵及输入处理量，避免视频线程饿死。
4. 短时写阻塞、完整帧重组、无响应及消费停顿分别有 5 秒时限。延长的是故障判定时间，没有加大视频缓冲。持续断线仍会退出并还原布局。
5. 接收端按屏幕选择 GPU，使用五缓冲 flip-discard、非阻塞 SyncInterval 0，以及硬件支持时的 tearing；记录实际呈现、busy/occluded 次数。
6. 仅在 DXGI 报告鼠标更新时间时更新光标可见性。
7. 显示切换中的 DXGI 暂时错误重建捕获。NVENC 完成超时、busy 或实际设备丢失时释放旧编码器，在会话内重建并从新 IDR/SPS 恢复；30 秒最多三次，不吞掉权限和配置错误。
8. 音频恢复时每次最多排空 32 个 WASAPI 包，只保留末尾 20ms PCM，避免重新播放排队的旧声音。

## 回归方法

平台 mock 和真实 TLS 回环故障注入验证：

- 视频 TCP 发送停顿 600ms，恢复后得到完整编码单元。
- 慢接收端下，旧实现曾积压六幅已编码视频；新实现最多两幅。恢复后新采集的画面重新送达，编码单元序号始终连续，不靠丢 P 帧通过测试。
- 单向中继、4096 字节接收窗口及 2MiB 视频帧制造真实 TLS 写阻塞；视频仍阻塞时，最近一次反向按键约 15.37ms 到达。该数字来自测试平台和回环，不是双机输入延迟承诺。
- 两个方向分别注入 4096 次连续鼠标移动，再发送点击与按键，验证终点及事件顺序、会话继续运行。
- 验证取消后来源对象释放、音视频穿插不会破坏确认顺序。

独立媒体回归 7 项通过，日志：`target/tmp/lan-transport-tests.log`。取消后均明确验证来源对象已释放；音视频穿插测试至少消费 12 个视频与 6 个音频单元。

`cargo fmt --all --check`、工作区全目标 Clippy、`cargo test --workspace`（409 通过、11 个条件测试默认忽略）、`cargo xtask lint` 和 `cargo deny check` 通过。本轮未修改 Flutter。

本机原生 9 项全部通过，其中新增隐藏接收窗口消息压力测试初次由于失焦保护额外产生 mouse-up 而不符合测试预期；调整为验证所有 mouse-down、松开边界及最终无卡键后通过，没有修改实际失焦保护。日志分别为 `target/tmp/lan-native.log`（其余 8 项通过）与 `target/tmp/lan-native-input.log`（修正后的压力测试通过）。原生吞吐诊断本次 10 秒呈现提交 788 次，P50/P95 来源时间戳至呈现调用返回 6.21/9.24ms；来源日志 max_age 曾达约 719ms，该时间戳也可能包含 DXGI 重建耗时。仍有启动/调度尾部尖峰，不能把 P95 或成功提交率当成整个问题已消失。

## 验收边界

本机原生测试覆盖编解码、呈现、显示主副屏/四方向恢复、异常退出回收和消息洪峰。原生吞吐测试不经过热点，也不计物理面板扫描；不能替代两台电脑持续动态画面的测量。GPU 故障恢复路径尚未人为触发驱动复位验证。

两台都升级后，需要在原热点上分别测试拖动窗口、滚动长页面、光标移动，连续换向及取消，并保留两端的日志。音频真实播放、不同 GPU 和不同热点负载仍需双机验证。

Windows 媒体现在已增加经 TLS 认证的 UDP/FEC 快路径，并按 Apollo 的分片 pacing 规则平滑发送；能力协商或 UDP 失败时仍回到 TCP。该适配器尚未接入 Apollo 完整 RTP/NV_VIDEO_PACKET 与动态码率闭环，因此还不能承诺任意无线干扰下零延迟。下一轮应在同一热点对比 TCP、UDP 适配层和 Apollo 原程序，记录 UDP 丢包、FEC 恢复、发送排队、解码呈现和 P95/P99 端到端响应。

后续已完成 Apollo/Moonlight、RustDesk、WebRTC 与 QUIC 的源码调研，并确认当前两个消费确认额度会将确认抖动反馈为采集停顿。第一阶段独立 UDP 媒体已接入，实施边界和下一轮双机验证见 [局域网实时媒体通道方案](windows-lan-realtime-transport-plan.md) 与 [ADR 0021](adr/0021-apollo-moonlight-udp-media-adapter.md)。

## 后续原生实时媒体接入

2026-11-15 已用 Apollo RTP/NV_VIDEO_PACKET 与直接编译的 Moonlight RtpVideoQueue 取代第一阶段 SFA1 适配器，加入认证加密、探测、IDR 恢复、动态码率和同步 TCP 回退。本页前文的 TCP 回归与测量是历史基线；当前实现和本机真实验证以 [新验证报告](windows-realtime-media-validation.md) 为准。

# 0021. 复用 Apollo RTP/FEC 发送与 Moonlight 接收队列

- 状态：已采纳；媒体集成、故障注入及本机原生验证已完成。
- 日期：2026-11-15

用户要求以成熟开源媒体内核为基线融合进 ScreenFerry。第一阶段的 SFA1 自定义分片/接收状态机已经移除，替换为 Apollo v0.4.6 `stream.cpp` 的 RTP/NV_VIDEO_PACKET、八字节短帧头、多块 Reed–Solomon FEC 与批次 pacing。发送部分适配 ScreenFerry 的媒体帧和套接字生命周期；接收端直接编译固定提交的 Moonlight common-c `RtpVideoQueue.c`，运行上游重排、重复包过滤、FEC 恢复、丢帧通知和多块拼接逻辑。

`RtpVideoQueue.c` 的本地变更仅是 standalone 构建时使用回调 shim，并将 GF 表初始化放入线程安全的 call_once；不重写上游队列算法。完成帧回调把有序负载交给现有 Windows Media Foundation 硬解和 D3D11 呈现。帧丢失或呈现队列溢出后，运行时等待 H.264 IDR 并经控制通道请求新关键帧，防止跳过 P 帧后继续解码错误参考链。

在 `DISPLAY_REALTIME`（bit 12）能力下，媒体 TLS 连接协商会话号、双方临时端口和随机 256 位媒体密钥。采用 Windows CNG AES-256-GCM：对包加密并认证帧号，区分视频、音频和探测方向的 nonce 域；接收端使用 4096 包防重放窗口。连接 UDP socket 限定来源地址/端口。双方完成有时限的加密双向探测并通过 TLS 提交后才切到 UDP，任一端失败则双方继续 TCP。旧 bit 11 及旧消息定义保留，当前版本不再声明第一阶段 SFA1 的能力。

运行期间保留 TLS 控制、输入、保活与反馈；UDP 首帧 10 秒未到或运行后连续 3 秒没有完整帧或本地发送错误时，通过可靠控制消息建立回退边界，双方切回 TCP，来源请求 IDR。原生发包/FEC/pacing 在独立阻塞发送线程执行，与采集/编码及输入处理分离；最多一帧待发送，采集前取得发送额度。即使 send 阻塞，输入工作线程与 Tokio 控制/保活仍可继续。每 100ms 回报帧数、丢帧、恢复分片和呈现排队，控制码率在 1–18Mbps 之间快速下降、稳定五秒后谨慎上调。NVENC 通过参数重配置调整码率，系统编码器使用 ICodecAPI；若后端拒绝，则记录失败并限制发送预算。

采用不超过 64KiB/64 包且约 1ms 预算的批次，跨帧使用高精度等待计时器。qWAVE 按 Apollo 的方式从系统目录动态加载，申请失败只影响优先级并记录实际结果。没有新建 VPN、Wi-Fi Direct、额外服务或更改系统路由。音频沿用现有 WASAPI PCM 编解码格式，使用独立 RTP 序号、nonce 域和接收队列，不声称实现 Moonlight 的 Opus 音频协议。

C ABI、CNG、WinSock、qWAVE 和 unsafe 仍只在 Windows 平台 crate；其他平台通过 trait 返回 None 使用 TCP。ScreenFerry 的设备证书、配对、键鼠/UAC 授权、设置进程和主副屏方案继续由原运行时管理，不启用 Apollo 的账号、RTSP/Web UI 或输入服务。它是 ScreenFerry 的原生集成，不是对外兼容 Moonlight 客户端的完整 GameStream 服务。

固定提交、原文参照、局部补丁和文件哈希记录在原生模块 `upstream.json`，许可及对应源码随安装包交付。来源见 [第三方说明](../third-party-media.md)，验证见 [实时媒体验证报告](../windows-realtime-media-validation.md)。

2026-11-15 补充：首帧时限与运行期分开，避免编码器初始化引发误回退；12Mbps 起步，拥塞减半，媒体响应突增同样触发降码率。验证见 `windows-input-alignment-validation.md`。

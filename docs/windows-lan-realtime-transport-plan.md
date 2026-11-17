# Windows 局域网实时媒体通道实施状态

日期：2026-11-15。代码接入和本机测试已完成；两台真实电脑在热点上的持续测试尚待安装本包后进行。

## 已完成的媒体链路

1. **固定成熟基线**：Apollo v0.4.6 `0cd32abaaa141d262477d039ac447b38fe99c394` 与配套 Moonlight common-c `ad329b240f18826f320ce6a99226b36354b86b59`。保留原始源码、许可证、文件哈希和本地补丁说明。
2. **发送核心**：Apollo 原生 NVENC；RTP/NV_VIDEO_PACKET 分包、短帧头、多块 Reed–Solomon FEC 和批次 pacing。移除 SFA1 自定义分片/拼帧状态机。按包大小和实际码率预算限制突发，跨帧使用高精度计时器；动态加载系统 qWAVE 申请媒体优先级。
3. **接收核心**：直接编译上游 `RtpVideoQueue.c`，使用其重排、恢复和过期块推进。视频与音频分别维护 RTP 序号、nonce 域和队列；完成帧通过 C ABI 交给 Windows 硬解与 D3D11 呈现。
4. **会话安全与兼容**：沿用配对和 TLS；bit 12 协商临时端口、随机 AES-256-GCM 密钥，双向加密探测通过后提交 UDP。校验来源、包认证、帧字段与重放窗口。能力不匹配或 UDP 不通时回到原 TCP；没有第二套账号和配对流程。
5. **恢复与反馈**：丢帧/队列超限后等待 IDR 并请求关键帧；500ms 反馈帧数、丢帧、FEC 恢复和呈现排队。4–18Mbps 范围内快速降码率、五秒稳定后上调，NVENC 动态重配置与 ICodecAPI 系统编码均有入口。UDP 断流 1.5 秒后通过 TLS 同步回退 TCP，请求 IDR，保留原窗口和显示方案。
6. **打包**：单一 ScreenFerry 安装包包含后台、输入服务、Flutter 设置界面、签名虚拟显示驱动、上游许可证和完整对应源码。无需安装 Apollo。

## 上游依据

- [Apollo stream.cpp](https://github.com/ClassicOldSong/Apollo/blob/v0.4.6/src/stream.cpp)
- [Moonlight RTP queue](https://github.com/moonlight-stream/moonlight-common-c/blob/ad329b240f18826f320ce6a99226b36354b86b59/src/RtpVideoQueue.c)
- [Apollo Windows qWAVE](https://github.com/ClassicOldSong/Apollo/blob/v0.4.6/src/platform/windows/misc.cpp)
- [Sunshine Wi-Fi 突发排队排查](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html)

## 验证边界

本机加密 UDP、丢包、乱序、篡改、双向探测、真实 TLS 回退与原生采集/解码测试已执行。结果见 [验证报告](windows-realtime-media-validation.md)。本机回环延迟不等于两台电脑经过热点的延迟；1080p100 是请求档位，并非已保证的双机呈现帧率。

“专用通道”是独立套接字、发送预算和恢复策略，不表示独占无线带宽。当前沿用已验证能流畅运行 Apollo 的热点，不额外建立 VPN、Wi-Fi Direct 或虚拟网卡。真实热点上应在两台同时升级后分别持续运行两个方向，音频先关闭，记录两端日志；不能拿空闲 ping 或编码 FPS 代替该验收。

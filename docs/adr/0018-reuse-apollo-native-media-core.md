# 0018. 复用 Apollo 原生媒体模块，以 GPLv3 发布整合版

- 状态：已采纳。维护者于 2026-11-14 明确选择“复用 Apollo 核心，整合版按 GPLv3 开源”。
- 日期：2026-11-14

此前的软件媒体路径已由用户实测确认性能不合格。本轮优先接入用户已测试过的 Apollo v0.4.6 的原生硬件编码代码，固定提交 `0cd32abaaa141d262477d039ac447b38fe99c394`。代码放在 Windows 平台媒体实现的源文件子目录，所有 Rust/C ABI 调用仍限制在 Windows 平台 crate，原有分层、设置界面通信和键鼠/UAC 授权边界保持。

先接入 Apollo 的原生 D3D11 NVENC，保留可工作的 Quick Sync 备用路径、GPU 捕获和 GPU 呈现，并如实报告实际使用的后端。随后在独立 ADR 0021 中接入 Apollo RTP/NV 分包与 pacing、Moonlight RTP 队列和 Reed-Solomon 的原生实时媒体层；这仍不等同于已经复现完整 Apollo/Moonlight 的双机协议。安装包由 ScreenFerry 部署和管理，不要求用户另装或登录 Apollo。

整合程序声明 `GPL-3.0-only`，新增根 `LICENSE-GPL`。历史 ScreenFerry 代码的 MIT/Apache 文件和版权说明保留，不声称撤销原许可。上游模块逐字保留、记录版本与文件哈希，通过单独的 C ABI、日志、配置和清理适配层接入。NVENC SDK 头文件固定与 Apollo 相同的提交，保留其 MIT 许可证。构建和发布必须包含相应的许可、上游来源和完整对应源码/构建资料。

这是维护者已确认的第 3 节许可证文件说明扩展；不改变第 4–10 节的分层、unsafe、目录及配置迁移规则。

- [Apollo 许可证声明](https://github.com/ClassicOldSong/Apollo/blob/v0.4.6/CMakeLists.txt)
- [Apollo NVENC 模块](https://github.com/ClassicOldSong/Apollo/tree/v0.4.6/src/nvenc)

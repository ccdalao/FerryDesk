# ScreenFerry 集成 Apollo 串流核心的实施准备

## 已核对的源码

- 版本：用户此前测试过的 Apollo `v0.4.6`。
- 提交：`0cd32abaaa141d262477d039ac447b38fe99c394`。
- 本地研究副本：`target/upstream/apollo-v0.4.6/`。原生 NVENC 模块已保留原文导入 Windows 平台的媒体模块，并通过 C ABI 适配；没有修改系统安装的 Apollo。
- 根 CMake 明确声明 `GPL-3.0-only`，不是 MIT/Apache。维护者已明确确认整合程序采用 GPLv3；原有 MIT/Apache 文件保留，新增 GPL 许可和上游来源说明，安装包包含对应源码和构建资料。

## 原生实验的实际状态

最初交付的开发包没有使用 Apollo 核心，采用 GDI、CPU 图像转换和系统软件 H.264，用户已确认双机延迟和流畅度不达标。后续包已接入下述原生 NVENC 路径；2026-11-15 的双机反馈仍有卡顿，当前进一步修复调度、输入积压和呈现，见 [局域网延迟验证](windows-lan-latency-validation.md)。

本轮未交付的 GPU 媒体路径：DXGI 捕获、GPU NV12 转换、Apollo 原生 D3D11 NVENC（其他 GPU 可选系统硬件编码）、D3D11 解码和交换链呈现。被控电脑同机测试 10 秒，Apollo NVENC 路径记录到 919 次成功的呈现提交（约 91.9 次/秒），从源实例自身的采集调用时间戳到呈现调用返回 P95 约 6.31ms。此测试不经过网络，不计显示器扫描，不证明不同画面的可见刷新率或两台电脑的端到端延迟。早期使用测试入口时钟的延迟估计已废弃，以源实例时钟的校准结果为准。

这些改动暂保留用于对照、接收呈现和诊断；尚未把它们重新打包让用户安装。

## 复用范围

Apollo 不是独立的一个编码函数。已核对其 Windows GPU 捕获模块、`src/nvenc/` 的原生 D3D11 NVENC、`video.*` 的编码会话、`stream.*` 的传输以及帧节奏代码。其原始 CMake 构建整个主程序并强制构建 Web UI，不能原封不动作为现成的媒体库链接。

实施应优先保留成熟的捕获、厂商硬件编码、低延迟帧节奏、声音及媒体传输；接收侧还需要匹配的 Moonlight 协议核心和解码/呈现。只复制一个编码器，不能复现之前 Apollo 加 Moonlight 的完整效果。

## 产品边界

用户只安装和使用 ScreenFerry：

1. 一个 ScreenFerry 安装包部署后台、设置界面、受 ScreenFerry 管理的媒体核心及所需驱动和许可文件。
2. 设置页、托盘和三个快捷键保持“用被控屏幕扩展主控”“用主控屏幕扩展被控”“取消拓展”。
3. 不要求用户另装、登录或配置 Apollo，不显示其 Web 管理界面。内部核心进程的名称、生命周期和更新由 ScreenFerry 管理。
4. ScreenFerry 配对完成后，由已验证的当前设备会话交换、登记媒体身份。不能改为批准任何局域网配对请求，也不能把永久 PIN 写死在程序中。
5. 撤销配对、断开当前设备会话或取消拓展时，同时撤销相应媒体会话；原键鼠映射和已经修好的安全输入服务保持其边界。
6. 先评估复用目前已安装的签名 Virtual Display Driver，把 Apollo 的抓屏目标指向本次虚拟屏。SudoVDA 的具体再分发条件、签名及卸载边界需要独立核对，不能仅凭 Apollo 的 GPL 声明推定驱动二进制也具有同样许可。

## 实施与验收顺序

1. 确定整合程序的 GPLv3 发布方式，记录上游版本和所有依赖许可。
2. 建立去 Web UI 的核心构建和明确的本机控制接口；先验证原生 NVENC、Quick Sync/AMD 等硬件选择，避免静默回退软件编码。
3. 接入接收核心、声音和会话回收，随后接回 ScreenFerry 已有的双向控制入口。
4. 在两台电脑上用与此前相同的 1080p、至少 60FPS 配置对照 Apollo；测动态画面、实际呈现、交互拖滞、尾延迟、音画同步及后台状态，之后再测 100Hz。
5. 完成安装、升级、取消、断线、反复换向、UAC 输入回归，再提供新的安装包。

## 参考

- [Apollo v0.4.6 CMake：GPL-3.0-only 声明](https://github.com/ClassicOldSong/Apollo/blob/v0.4.6/CMakeLists.txt)
- [Apollo GPLv3 许可证](https://github.com/ClassicOldSong/Apollo/blob/v0.4.6/LICENSE)
- [Apollo Windows 捕获与硬件编码实现](https://github.com/ClassicOldSong/Apollo/tree/v0.4.6/src/platform/windows)
- [Apollo 原生 NVENC 实现](https://github.com/ClassicOldSong/Apollo/tree/v0.4.6/src/nvenc)
- [Moonlight 协议接收核心](https://github.com/moonlight-stream/moonlight-common-c)

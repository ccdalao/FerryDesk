# 原生媒体第三方代码 / Native media third-party notices

本整合程序按 GPL-3.0-only 分发。历史 ScreenFerry 的 MIT 与 Apache-2.0 文件及版权说明保留。This integration is distributed under GPL-3.0-only; historical MIT and Apache-2.0 notices remain included.

## Apollo

- 项目 / Project: https://github.com/ClassicOldSong/Apollo
- 版本 / Version: v0.4.6
- 提交 / Commit: `0cd32abaaa141d262477d039ac447b38fe99c394`
- 许可 / License: GPL-3.0-only，见 `LICENSE-GPL` 和原生模块 `upstream/LICENSE`。
- 来源：`src/nvenc/` 中独立 NVENC 基类、D3D11 原生实现、配置、颜色空间与帧结构。
- 上游算法及原始参照文件保留；NVENC 码率重配置、C ABI 和队列独立化的局部修改列入补丁清单。原始/整合文件 SHA-256 记录于 `crates/ferry-platform-windows/src/display_stream/apollo/upstream.json`。
- 新增独立 C ABI、日志、作用域清理、配置类型适配，不使用 Apollo 的账号、Web UI、键鼠注入或系统服务。核心调用保持在 ScreenFerry 媒体线程，编码器及 GPU 资源随会话释放。
- 当前复用范围包括原生 NVENC，以及 `stream.cpp` 的 RTP/NV_VIDEO_PACKET、短帧头、多块 FEC 和跨帧批次 pacing。`upstream/src/stream.cpp` 保留固定版本原始参照，`transport.cpp` 将发送逻辑接入 ScreenFerry 的媒体帧、套接字和 Windows CNG 加密。第一阶段 SFA1 自定义头已经移除。
- ScreenFerry 仍负责账号免配、TLS/端口/密钥协商、控制与输入；不会启动 Apollo RTSP、Web UI、ENet 或另一个服务。Moonlight 有序帧回调连接 Windows 系统硬解和 D3D11 呈现，不启用 common-c 全局 RTSP/控制/解码工作线程。音频沿用 WASAPI PCM，未实现 Moonlight Opus 协议。

Apollo sources and logic are retained, with LF line-ending normalization. Separate adapters connect its standalone native D3D11 NVENC implementation to ScreenFerry. This is not the entire Apollo/Moonlight pipeline.

## Moonlight common-c RTP queue and Reed-Solomon

- 项目 / Project: https://github.com/moonlight-stream/moonlight-common-c
- 固定提交 / Commit: `ad329b240f18826f320ce6a99226b36354b86b59`
- 编译文件 / Compiled files: `src/RtpVideoQueue.c`, `reedsolomon/rs.c`；配套头文件同样固定到该提交。
- 许可 / License: GPL-3.0，原始声明保留在 `crates/ferry-platform-windows/src/display_stream/apollo/upstream/moonlight-common-c/LICENSE.txt`。
- 复用范围：每个编码单元按块生成数据/校验分片，接收端在有界窗口内恢复丢失分片；调用前执行上游要求的一次性 GF 表初始化，并遵守 `marks` 的缺失分片语义。
- `RtpVideoQueue.c` 直接编译，原算法保留；standalone include 和一次性初始化是两个局部适配点。回调绑定通过线程本地上下文支持独立会话，完成数据按短帧头解包后交给现有解码器。ScreenFerry 对丢帧执行等待 IDR 与受限刷新请求，源文件与本地补丁均可由源码包审计。

## NV codec headers

- 项目 / Project: https://github.com/FFmpeg/nv-codec-headers
- 固定提交 / Commit: `22441b505d9d9afc1e3002290820909846c24bdc`，与上述 Apollo 的子模块一致。
- 文件 / File: `include/ffnvcodec/nvEncodeAPI.h`
- 许可 / License: NVIDIA 头文件中的 MIT 声明，单独保留于原生模块 `headers/LICENSE-MIT`。
- 编码运行时通过显卡驱动提供的系统 DLL 加载，不随安装包另行分发 NVIDIA 驱动。

## Virtual Display Driver

- 项目 / Project: https://github.com/VirtualDrivers/Virtual-Display-Driver
- 版本 / Version: 25.7.23
- 许可 / License: MIT，安装包的驱动文件夹随附原许可。
- 原始驱动 DLL、INF、CAT 不修改，Windows 验证原始签名。显示模式配置增加 1080p100 档位。

## 对应源码与构建 / Corresponding source and building

安装目录的 `source.zip` 包含本次整合的 Rust、Flutter、C++ 源码、锁文件、原生头文件、测试、许可及构建脚本。它不包含安装账号、配对私钥、运行时配置或本地构建缓存。

Windows：按 `rust-toolchain.toml` 安装 Rust，准备 Visual Studio C++、Windows SDK、Flutter 和 Inno Setup 7.1+；运行 `cargo build --release -p ferry-daemon -p ferry-service` 和 `cargo xtask bundle`。原生 Apollo 代码由 Windows 平台 crate 的 `build.rs` 自动静态编译，不需要用户安装 Apollo。GPU 驱动和 Windows 系统库由操作系统或硬件提供商提供。

`source.zip` contains the corresponding integration sources and build instructions, including native headers and adapters. Build the workspace using its pinned Rust toolchain, Visual Studio C++ and Windows SDK, Flutter, and Inno Setup. `cargo xtask bundle` builds and stages the application, licenses, and corresponding source archive.

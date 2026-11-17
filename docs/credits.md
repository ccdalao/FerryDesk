# FerryDesk 开源说明与致谢

FerryDesk 由小小小龙（GitHub `ccdalao`）维护，在 ScreenFerry 基础上扩展 Windows 双向副屏与相关设置。项目主页为 https://github.com/ccdalao/FerryDesk 。

本整合版采用 **GPL-3.0-only**，许可证全文见 [LICENSE-GPL](../LICENSE-GPL)。原 ScreenFerry 代码的 MIT、Apache-2.0 许可及原有版权声明继续保留，分别见 [LICENSE-MIT](../LICENSE-MIT) 和 [LICENSE-APACHE](../LICENSE-APACHE)。维护者名称不替代上游作者和组件版权。

| 上游 | 用途与归属 |
|---|---|
| [icloudza / ScreenFerry](https://github.com/icloudza/screen-ferry) | 原键鼠共享、配对、剪贴板、文件和界面基础，原许可为 MIT OR Apache-2.0 |
| [Apollo](https://github.com/ClassicOldSong/Apollo) | 复用的原生编码与媒体适配代码，GPL-3.0-only |
| [Moonlight common-c](https://github.com/moonlight-stream/moonlight-common-c) | RTP 接收与 FEC 等代码，许可与来源见第三方清单 |
| [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) | 签名虚拟显示驱动，保留随驱动分发的许可及发布者信息 |

具体版本、源文件、修改边界和第三方许可证见 [媒体组件说明](third-party-media.md)。安装包中包含完整对应源码 `source.zip` 以及相关许可证。

独立更新只从 `ccdalao/FerryDesk` 的 Releases 获取，并使用本分支自己的发布公钥验签。原上游发布签名不作为本分支更新授权。

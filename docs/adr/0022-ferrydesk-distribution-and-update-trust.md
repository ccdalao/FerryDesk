# 0022 FerryDesk 独立分发与更新信任

状态：已采用。

用户确认在 `ccdalao/FerryDesk` 建立独立公开仓库，继续使用 Apollo 整合版已确认的 GPL-3.0-only 许可。原 ScreenFerry 及第三方许可、版权、来源记录保留。

## 决定

- 产品名称为 FerryDesk，个人构建标记通过打包参数提供。窗口标题、项目主页、反馈、下载、安装器发布者使用此分发身份。
- Rust crate、后台和服务的可执行文件名、服务名、配对、配置与缓存技术目录保持兼容。主程序默认目录改为 `Program Files\FerryDesk`；安全输入服务保留受保护的技术目录。
- 独立更新从 `ccdalao/FerryDesk` Releases 获取，使用独立生成的日常与备用 minisign 公钥。私钥及口令不提交源码，通过仓库 Actions Secrets 使用。
- 首个独立版本需要手动安装以建立新的发布信任。后续更新维持清单签名、文件大小、哈希、版本及回滚检查。
- 当前维护与验证目标为 Windows。依据 ADR 0014 的未配置 macOS 签名例外，CI 可只发布 Windows 安装包、`update.json`、`update.json.minisig` 三个附件；配置 macOS 签名、公证后再增加两个 macOS 附件。安装包仍包含对应源码与许可。
- Release 附件由 CI 生成，先形成草稿，核对版本、附件和签名后公开。不将本机包替换已公开的版本附件。

关于页展示界面实际构建版本。连接旧后台时显示完成升级提示，避免借用旧后台的版本或更新结果误导用户。

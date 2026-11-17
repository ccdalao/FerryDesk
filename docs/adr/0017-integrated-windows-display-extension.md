# 0017. Windows 双机扩展屏接入现有设备会话

- 状态：已采纳，维护者在 2026-11-14 授权集成，确认以现有配对授权并提供两个方向及取消操作。
- 日期：2026-11-14

## 决策

扩展屏由 ScreenFerry 常驻后台管理，Flutter 设备页、托盘和全局快捷键提交同一组操作。两个方向以现有键鼠角色命名。首次安装通过 Windows 安装程序登记签名虚拟显示驱动，普通使用不需要额外账号或配对。

新增可选能力 DISPLAY_STREAM，媒体连接继续使用 TCP + TLS 1.3，由原键鼠客户端发起，同样核对设备证书、信任记录和当前输入会话号。控制消息绑定随机扩展屏请求号，媒体连接只认领一次；旧版本不协商该能力，原有共享继续工作。

媒体格式首版采用系统 H.264 与可选的 48 kHz 双声道 16 位 PCM。媒体有界排队、分块有上限，超时关闭媒体会话。配置 schema 7 增加声音偏好和三个本机快捷键，缺省关闭。

Windows 实现放在 ferry-platform-windows/display_stream，同名平台接口与 mock、macOS 文件一并建立。引擎通过 DisplayBackend 注入。Windows 设置页保留单独进程，媒体任务属于后台，设置界面关闭不终止串流。接收窗口输入只作用于本次虚拟屏，切换、断线与失焦时释放按键。主控向被控屏幕扩展时暂停该设备的普通贴边进入，结束后恢复；主控显示被控扩展桌面时保留贴边通道，使主控仍能进入被控实体屏幕。

安装程序打包 Virtual Display Driver 25.7.23 的原始签名二进制，下载文件校验固定 SHA-256，随附 MIT 许可证。卸载仅移除 ScreenFerry 创建的设备实例。

## 验证边界

1080p100 是请求档位，最终帧率依赖主机编码、转换、接收显示和网络。当前实现的捕获和显示经过 CPU 内存，系统 H.264 MFT 使用软件编码路径。达到目标前需进行双机持续动态画面测试，并以发送和呈现统计报告实际结果。

## 参考

- [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)
- [微软间接显示驱动文档](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)
- [微软 H.264 编码器文档](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)

# 0019：Windows 管理员后台的显式入口

状态：本地开发实现；双机管理员窗口交互待验收。

## 背景

用户报告普通共享和双向副屏都会在管理员窗口前台失效，并要求参考 [上游 issue #7](https://github.com/icloudza/screen-ferry/issues/7)。已有安全输入服务负责已授权设备的 SYSTEM 注入，但普通后台的主控钩子和独立副屏输入路径仍有权限边界。

## 决策

复用现有 `elevate` 命令，在 Windows 托盘增加显式入口和当前提权状态。由 Windows UAC 获取用户同意后，新后台先请求原实例正常退出，等待媒体窗口、虚拟显示和按键回收，再使用相同数据目录运行。取消 UAC 保留旧实例。设置进程经 Explorer 的 Shell COM 对象启动，避免继承管理员令牌；Explorer 不可用时返回错误。

不改变进程清单、默认启动权限或自启登记方式，不引入永久提权计划任务。选择只作用于本次后台生命周期。用户希望两个方向都能操作普通管理员窗口时，两端均需启用管理员后台。输入权限检查提供准确提示，媒体错误会结束会话并清理资源。

安全输入服务的设备授权、签名和租约保持有效。管理员后台不代替服务，也不让普通媒体请求获得 SYSTEM 权限。副屏视频仍只采集普通桌面的虚拟显示器；UAC 安全桌面控制继续由原安全输入会话承担。

## 影响

用户可以直接从托盘切换，无需寻找 EXE 或运行 PowerShell。管理员后台承载网络及媒体，因此仍默认采用普通权限。管理员模式下，普通 Explorer 向后台边缘窗口拖文件会受到 UIPI 限制；需用文件发送命令等已有入口。若未来需要跨登录保留该模式，应另行设计明确的启用、禁用和卸载流程。

参考：[SendInput 的 UIPI 约束](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)、[微软 Execute in Explorer 示例](https://learn.microsoft.com/en-us/windows/win32/shell/samples-execinexplorer)。

# 线协议

- 没有单独的协议版本：协议跟着软件走，软件只有一个版本号。新旧版本靠能力位协商互连（第 3、7 节）。
- 实现：`crates/ferry-proto`
- 测试：`crates/ferry-proto/tests/suite/`；fuzz：`fuzz/fuzz_targets/decode.rs`

本文是协议的权威说明。实现与本文不一致时，以本文为准修正实现。

---

## 1. 连接

每对设备之间建立两条常驻的 TCP 连接，均运行在 TLS 1.3 之上，用同一套设备证书双向认证。TLS 握手使用 ALPN 标识 `screenferry`，不是本协议的连接在 TLS 阶段就会被拒绝。首次连接时另有一条短暂的配对连接，配对完成即关闭；每次传文件另开一条文件连接，传完即关闭；服务端邀请客户端时另有一条邀请连接，客户端答应后即关闭。

| 连接 | 内容 | 单帧消息体上限 |
|---|---|---|
| 输入连接 | 键鼠事件、屏幕切换、显示器布局、心跳 | 4 KiB |
| 数据连接 | 剪贴板通告与内容、分块传输、心跳 | 64 KiB + 64 字节 |
| 配对连接 | 配对码所需的随机数与双方的确认 | 4 KiB |
| 文件连接 | 一次文件传输的清单与内容 | 64 KiB + 64 字节 |
| 邀请连接 | 服务端请客户端连过来 | 4 KiB |
| 扩展屏媒体连接 | 一次扩展屏会话的音视频分块 | 64 KiB + 64 字节 |

各条连接独立收发，大文件不会阻塞键鼠事件和剪贴板。除邀请连接外，所有连接都由客户端发起，服务端只监听一个端口（缺省 24810）。

扩展屏协商 `DISPLAY_REALTIME` 后，媒体连接仍先由客户端建立 TLS 认证和发送 `DisplayOpen`，随后在这条连接上交换临时 UDP 端口与随机媒体密钥。完成双向加密探测和 TLS 提交后，媒体使用原生 RTP/FEC UDP 通道，`Ping`、`Pong`、`DisplayInput`、反馈和会话结束仍走媒体 TCP；能力未协商或探测不通过时，双方继续使用 `DisplayPacket` 的 TCP 路径。

### 身份与信任

- 每台设备有一张自签名证书（ECDSA P-256）作为身份，以证书 DER 编码的 SHA-256 作为**指纹**。
- 证书没有 CA 可以验证。TLS 握手只证明对方持有证书对应的私钥，双方都接受任何证书；握手后再按指纹查本机的信任列表。
- 接受方只为信任列表中的设备建立输入连接，否则回 `Reject(NOT_PAIRED)`；数据连接还必须与输入连接来自同一指纹。
- 发起方发现对方指纹不在信任列表中，或收到 `Reject(NOT_PAIRED)` 时，先建立配对连接配对，成功后再重新连接。

### 局域网发现

- 接受方用 mDNS / DNS-SD 广播服务类型 `_screenferry._tcp.local.`，端口即监听端口。
- 实例名为 `设备名 (指纹短形式前 9 个字符)`，避免同名设备冲突。TXT 记录：

  | 键 | 值 |
  |---|---|
  | `fp` | 证书指纹，64 位小写十六进制 |
  | `name` | 设备名 |
  | `v` | 格式标记，固定为 `1` |

- 广播内容不受保护，只用来找到地址。发起方连上后必须核对 TLS 证书的指纹与 `fp` 一致，不一致即放弃这台，也不对它发起配对。
- 发起方优先选择指纹在信任列表中的设备；信任列表为空、且局域网里恰好只有一台时才自动选它（随后照常配对）。`v` 与本机不同的记录忽略。

### 按地址连接与邀请

有的网络不转发 mDNS（公司网络、开了设备隔离的 Wi‑Fi），所以也可以不靠发现：

- **按地址连接**：客户端直接连用户给的地址。和发现到的地址一样核对证书、按需配对；地址不在任何广播里也照常进行。
- **记住地址**：双方都在信任列表里记下对方最后一次连上的地址（服务端只记 IP）。客户端在局域网里找不到已配对的服务端时，轮流试记下的地址。
- **邀请**：被控制的那台常常没有键盘鼠标，没法在它上面输入地址，所以服务端也能发起。客户端另外监听一个邀请端口（缺省 24811），只接受邀请连接：

  ```
  服务端                                          客户端（邀请端口）
    ── Hello { channel: 邀请, session: 服务端的监听端口 } ──▶
    ◀───────────────────────── Welcome 或 Reject ──
                        （关闭邀请连接）
    ◀── Hello { channel: 输入, session: 0 } ──    （客户端连到 服务端 IP:端口）
  ```

  - 客户端正连着另一台服务端时回 `Reject(BUSY)`；`session` 不是 1–65535 时回 `Reject(UNKNOWN_SESSION)`；其余回 `Welcome`。
  - 回了 `Welcome` 后，客户端连到“邀请连接的来源 IP + `session` 给的端口”，连上的必须是发出邀请的那台（指纹相同）。已经连着它时不再重连。
  - 邀请本身不要求双方已配对：它只让客户端去连一个地址，之后照常核对指纹，没配对时两边核对配对码。
  - 服务端开始监听后，自动邀请信任列表里记着地址、此刻没连上的客户端。

## 2. 握手

```
发起方                                        接受方
  ── Hello { channel: 输入, session: 0 } ──────▶
  ◀──────────── Welcome { session: S } 或 Reject ──
  ── Hello { channel: 数据, session: S } ──────▶
  ◀──── Welcome { session: S } 或 Reject(UNKNOWN_SESSION) ──
```

- 每条连接的第一条消息必须是 `Hello`，接受方以 `Welcome` 或 `Reject` 回应。握手完成前，除这三条和 `Bye` 外的消息都是协议违规。
- 先建立输入连接，拿到会话号 `S`；数据连接、文件连接和扩展屏媒体连接带上 `S`，接受方据此把它们归到同一会话。
- **格式标记**：`Hello`、`Welcome`、`Reject` 开头的 `format` 固定为 `1`（实现里是 `WIRE_FORMAT`）。它不随软件版本变化，也不是功能开关；
  已发布的版本要求双方相等，不等时接受方回 `Reject(INCOMPATIBLE_FORMAT)`。新功能不动它，只加能力位。
- **能力**：`Welcome` 中的能力是双方能力的交集。只能发送已协商能力范围内的消息：
  - `PointerDelta` 需要 `RELATIVE_POINTER`
  - 剪贴板消息需要 `CLIPBOARD`
  - 文件消息和文件连接需要 `FILES`
  - `PeerSide`、`DropHint` 需要 `EDGE_DROP`
  - `OpenSettings` 需要 `REMOTE_SETTINGS`
  - `AppInfo` 需要 `APP_INFO`：输入连接握手后，双方各发一次自己的程序版本（ADR 0014）
  - `InputBlocked` 需要 `INPUT_BLOCKED`（bit 8）：被控端收不了键鼠时请控制端收回光标，连接保持。对方没有这项能力时，被控端只能结束会话。
  - `SecureChallenge`、`SecureInput` 需要 `SECURE_INPUT`（bit 7）：只有管理员另行授权远端的被控端才声明此能力。
  - `DisplayControl`、扩展屏媒体连接及其消息需要 `DISPLAY_STREAM`（bit 9）。
  - `DisplayControl` 的屏幕清单和带布局的启动变体还需 `DISPLAY_PLANS`（bit 10）；旧启动消息继续保持原字节格式。
  - `DISPLAY_UDP`（bit 11）及其旧消息号保留给开发阶段 SFA1 通道；当前版本不再声明该能力。
  - 原生 Apollo/Moonlight 媒体协商、探测、反馈及回退需要 `DISPLAY_REALTIME`（bit 12）；未协商时继续 TCP 媒体连接，保留 `WIRE_FORMAT` 不变。
- `COMMAND_KEY` 说明发送方的快捷键以 Command 键为主（macOS），不是一项能力。服务端看客户端 `Hello` 里的这一位：
  两边一个是一个不是时，才按设置互换 Command 与 Ctrl。不认识这一位的旧版客户端都是 Windows，结果不变。
- `Hello`、`Welcome`、`Reject` 的消息体格式永久不变，新旧版本的双方总能完成协商。
- 配对连接的 `Hello` 中 `session` 填 0，`capabilities` 不起作用。
- 邀请连接的 `Hello` 中 `session` 是服务端的监听端口，`capabilities` 不起作用。

## 3. 帧格式

```
| 类型 u16 | 长度 u32 | 消息体（长度字节） |
```

- 全部小端。帧头 6 字节，长度只计消息体。
- 帧头一到就检查：未知类型、不属于本连接的类型、超过本连接上限的长度，都立即拒绝，不等消息体。
- 消息体必须被恰好读完，有多余字节或不够都是错误。
- **任何解码错误都是协议违规**：发送 `Bye(PROTOCOL_ERROR)` 后关闭连接。

## 4. 基本编码

| 类型 | 编码 |
|---|---|
| `u8` `u16` `u32` `u64` `i32` | 小端定长 |
| `bool` | 1 字节，只能是 0 或 1 |
| `str` | `u16` 字节数 + UTF-8 内容 |
| `bytes` | `u32` 字节数 + 内容 |
| `list<T>` | `u16` 元素数 + 逐个元素 |
| `[N]` | 恰好 N 字节，不带长度前缀 |

每个值只有一种合法编码，所以解码后再编码一定得到相同的字节。测试和 fuzz 都会检查这一点。

## 5. 消息

类型号的高字节表示类别：`0x00` 握手、心跳与配对，`0x01` 输入会话控制，`0x02` 剪贴板，`0x03` 传输，`0x04` 文件，`0x05` 扩展屏媒体。

| 类型号 | 消息 | 连接 | 消息体 |
|---|---|---|---|
| `0x0001` | Hello | 两者 | `format u16` · `channel u8` · `capabilities u64` · `session u64` · `name str` |
| `0x0002` | Welcome | 两者 | `format u16` · `capabilities u64` · `session u64` · `name str` |
| `0x0003` | Reject | 两者 | `reason u16` · `format u16` |
| `0x0004` | Bye | 两者 | `reason u16` |
| `0x0005` | Ping | 两者 | `nonce u64` |
| `0x0006` | Pong | 两者 | `nonce u64` |
| `0x0007` | PairCommit | 配对 | `commitment [32]` |
| `0x0008` | PairNonce | 配对 | `nonce [32]` |
| `0x0009` | PairResult | 配对 | `accepted bool` |
| `0x0100` | Displays | 输入 | `list<Display>`，最多 32 个 |
| `0x0101` | Enter | 输入 | `seq u32` · `position Point` · `locks u8` |
| `0x0102` | Leave | 输入 | `seq u32` |
| `0x0103` | PeerSide | 输入 | `side u8` |
| `0x0104` | DropHint | 输入 | `state u8` · `along u16` · `closeness u8` · `name str` · `items u32` · `bytes u64` |
| `0x0105` | OpenSettings | 输入 | 空 |
| `0x0106` | AppInfo | 输入 | `version str`，最多 64 字节 |
| `0x0107` | SecureChallenge | 输入 | `nonce [32]`，需要 `SECURE_INPUT` |
| `0x0108` | SecureInput | 输入 | `challenge [32]` · `sequence u64` · `body [32]` · `signature [64]`，需要 `SECURE_INPUT` |
| `0x0109` | InputBlocked | 输入 | 空，需要 `INPUT_BLOCKED`；被控端发给控制端 |
| `0x010A` | DisplayControl | 输入 | `kind u8` · `id u64`；仅启动时再带 `direction u8` · `audio bool` |
| `0x0110` | Key | 输入 | `code u32` · `action u8` |
| `0x0120` | PointerMotion | 输入 | `position Point` |
| `0x0121` | PointerDelta | 输入 | `dx i32` · `dy i32` |
| `0x0122` | Button | 输入 | `button u8` · `pressed bool` · `position Point` |
| `0x0123` | Wheel | 输入 | `dx i32` · `dy i32` · `position Point` |
| `0x0200` | ClipboardOffer | 数据 | `seq u32` · `list<ClipboardItem>`，最多 16 个 |
| `0x0201` | ClipboardRequest | 数据 | `seq u32` · `format u8` · `transfer u32` |
| `0x0300` | Chunk | 数据 | `transfer u32` · `last bool` · `data bytes`，最多 64 KiB |
| `0x0301` | Cancel | 数据 | `transfer u32` · `reason u16` |
| `0x0400` | FileOffer | 数据 | `id [16]` · `name str` · `items u32` · `files u32` · `bytes u64` |
| `0x0401` | FileAccept | 数据 | `id [16]` |
| `0x0402` | FileCancel | 数据 | `id [16]` · `reason u16` |
| `0x0410` | FileOpen | 文件 | `id [16]` |
| `0x0411` | FileList | 文件 | `list<FileEntry>`，最多 32 个 · `last bool` |
| `0x0412` | FileResume | 文件 | `index u32` · `offset u64` |
| `0x0413` | FileData | 文件 | `data bytes`，最多 64 KiB |
| `0x0414` | FileEnd | 文件 | `index u32` · `sha256 [32]` |
| `0x0415` | FileDone | 文件 | 无 |
| `0x0500` | DisplayOpen | 扩展屏媒体 | `id u64` |
| `0x0501` | DisplayPacket | 扩展屏媒体 | `track u8` · `timestamp_us u64` · `end bool` · `bytes`，每块最多 64 KiB |
| `0x0502` | DisplayInput | 扩展屏媒体 | 32 字节固定输入编码，与安全输入动作中的普通输入部分一致 |
| `0x0503` | DisplayUdpOffer | 扩展屏媒体 | `id u64` · `port u16` · `token u64`，由媒体来源端发送 |
| `0x0504` | DisplayUdpReady | 扩展屏媒体 | `id u64` · `port u16` · `token u64`，由媒体接收端回应 |
| `0x0505` | DisplayRealtimeOffer | 扩展屏媒体 | `id u64` · `port u16` · `key [32]`，仅在 TLS 上传递随机媒体密钥 |
| `0x0506` | DisplayRealtimeReady | 扩展屏媒体 | `id u64` · `port u16` |
| `0x0507` | DisplayRealtimeCommit | 扩展屏媒体 | `id u64` · `udp bool` |
| `0x0508` | DisplayRealtimeFeedback | 扩展屏媒体 | `frames u32` · `lost u32` · `recovered u32` · `queue_ms u16` · `keyframe bool` |
| `0x0509` | DisplayRealtimeFallback | 扩展屏媒体 | 空；请求可靠边界后的 TCP 回退 |

设备名 `name` 最多 128 字节。

### 原生实时媒体握手与恢复

双方能力交集有 bit 12 才执行：来源发 Offer（会话号、端口、随机 256 位 key），接收端配置 socket 并回 Ready（本地端口）；双方并行执行 400ms 内的加密双向探测，接收端再回 Ready（成功时本地端口，失败为 0）。来源综合两端探测结果回 Commit；`udp=false` 后双方使用 TCP。创建端点失败的 Offer/Ready 端口为 0，也必须完成同样的可靠消息序列。

UDP 视频负载采用 Apollo v0.4.6 的 RTP、NV_VIDEO_PACKET、八字节短帧头和最多四个 FEC 块。每个数据报外层为 32 字节 AES-GCM 前缀（nonce 12、frameNumber 4、tag 16），帧号参与认证；nonce 的最后字节区分视频/声音/两向探测，其余包含单调计数器。每个会话使用新随机密钥，接收端验证来源端点、认证及不超过 4096 包的防重放窗口后才调用 Moonlight 队列。音频沿用 ScreenFerry 原有 PCM 格式，使用独立 RTP/FEC 队列，不声明支持 Moonlight Opus。

Feedback 每 100ms 报告该周期完整帧数、不可恢复帧数、FEC 恢复分片数及最高本机呈现队列等待毫秒数；`keyframe=true` 表示需从 IDR 恢复。仅完整帧可交付，丢帧/队列溢出后等待独立解码 IDR。来源限制关键帧请求频率，根据反馈调节码率及 pacing；这些时间均是本机时长，不能解释成未校准的跨机单向延迟。

接收端 UDP 连续 1.5 秒无完整帧时发 Fallback，来源先通过 TLS 写 Commit（udp=false）再允许工作线程产出 TCP 编码单元并请求 IDR。来源本地 UDP 发送失败时也使用这一提交边界。接收端清空 UDP 待呈现队列并停止该通道，避免 TCP IDR 后又呈现旧 UDP 依赖帧。会话取消统一关闭 socket、工作线程、虚拟屏和接收窗口。

### 复合类型

| 类型 | 编码 |
|---|---|
| Point | `x i32` · `y i32` |
| Rect | `x i32` · `y i32` · `width u32` · `height u32` |
| Display | `id u32` · `bounds Rect` · `scale_percent u16` · `primary bool` |
| FileEntry | `path str` · `kind u8` · `size u64` · `modified u64` |
| ClipboardItem | `format u8` · `size u64` |

### 取值

| 字段 | 取值 |
|---|---|
| `channel` | 1 输入，2 数据，3 配对，4 文件，5 邀请，6 扩展屏媒体 |
| `capabilities` | 位 0 `RELATIVE_POINTER`，位 1 `CLIPBOARD`，位 2 `FILES`，位 3 `EDGE_DROP`，位 4 `COMMAND_KEY`，位 5 `REMOTE_SETTINGS`，位 6 `APP_INFO`，位 7 `SECURE_INPUT`，位 8 `INPUT_BLOCKED`；未知位保留，协商时自然被交集去掉 |
| `side` | 1 左，2 右，3 上，4 下 |
| DropHint `state` | 0 收起，1 靠近，2 在胶囊上 |
| `along` | 沿相邻那条边的位置，0–10000（左右边从上往下，上下边从左往右） |
| `closeness` | 有多靠近边缘，0–100 |
| `kind` | 1 文件，2 文件夹 |
| `path`、`name` | 最多 1024 字节；`path` 为相对路径，以 `/` 分隔 |
| `modified` | 自 1970 年起的毫秒数，0 表示不知道 |
| `code` | USB HID 扩展用法码：高 16 位用法页，低 16 位用法 ID。键盘页 `0x07`，消费者页 `0x0C` |
| `action` | 1 按下，2 松开，3 连发 |
| `locks` | 位 0 Caps Lock，位 1 Num Lock，位 2 Scroll Lock；其他位必须为 0 |
| `button` | 1 左，2 右，3 中，4 后退，5 前进，更大的编号是额外侧键；0 非法 |
| `Wheel.dx/dy` | 一格 = 120；高精度滚动可以是它的分数 |
| `scale_percent` | 一个逻辑单位等于多少个坐标单位（百分数）：Windows 为系统缩放比例，macOS 恒为 100。双方按 `对方 / 本机` 换算移动距离 |
| `format` | 1 纯文本（UTF-8，换行统一为 `\n`），2 HTML（UTF-8），3 RTF，4 PNG |
| Reject `reason` | 1 格式标记不同，2 会话不存在，3 正被其他设备控制，4 尚未配对 |
| Bye `reason` | 0 正常关闭，1 被新连接取代，2 协议违规，3 心跳超时 |
| Cancel、FileCancel `reason` | 0 主动取消，1 内容已过期，2 超过大小上限，3 源数据不可用 |

`format` 和各个 `reason` 以后可能新增取值，接收方遇到不认识的值应当按"未知"处理，不算协议违规。

## 6. 行为约定

### 心跳

收到 `Ping` 后尽快以相同 `nonce` 回 `Pong`。发送方用 `nonce` 携带发送时刻，以计算往返时间。发送间隔和超时由引擎决定：控制远端期间发送频率要高（建议 50–100 ms），既能及时发现断线，也能让对方网卡保持唤醒。

### 屏幕切换

- 光标进入对方屏幕时发 `Enter`，带递增的 `seq`、进入位置和本机锁定键状态，接收方据此同步 Caps Lock 等状态。
- 离开时发 `Leave`，带同一个 `seq`。
- 接收方收到 `Leave` **或者连接断开**时，必须释放所有仍按下的键和鼠标按键，杜绝卡键。

### 坐标

所有坐标都使用**接收方**的桌面坐标系：macOS 为点（point），Windows 为物理像素。接收方用 `Displays` 声明自己的坐标系。会话中途显示器布局变了（改分辨率、缩放、插拔显示器）时再发一次 `Displays`，对方以最新的一份为准。`Button` 和 `Wheel` 带上当时的光标位置，接收方先移动光标再执行。

### 签署输入（需要 `SECURE_INPUT`）

`0x0107 SecureChallenge` 的消息体为随机数 `[32]`；`0x0108 SecureInput` 为
`challenge [32] · sequence u64 · body [32] · signature [64]`，只能出现在输入连接。
双方协商 `SECURE_INPUT` 后，客户端先发 `Displays`、紧接第一条 `SecureChallenge`，再发可选的 `AppInfo`。
服务端取得初始挑战后才启动该会话的输出。后续挑战经输入连接更新，不重置序号。

`body` 为动作的原消息类型号 `u16` 加原消息体，再以零补足 32 字节；只允许 `Enter`、`Leave` 和键鼠 `Input`。
类型号 0、其余全零表示远端签署的保活，不允许通过此正文嵌入其他协议消息。
签名为 ring ECDSA P-256/SHA-256 FIXED 的 `r || s`，覆盖 ASCII 域
`ScreenFerry secure input proof v2`、一个 NUL 字节、challenge、sequence 的小端编码及完整 body。
控制端只为自己输出的动作签名，每 250 ms 发送签署保活；没有为客户端指定动作签名的接口。

代理使用管理员按用户 SID 另行登记的公钥验证，不信任普通配对配置。挑战由代理生成，每 250 ms 按需轮换，
有效 1 秒；每条管道从序号 1 连续递增。只有有效远端证明续期 1.5 秒租约，本机 Check 不续期。
代理负责完整动作的定位、锁定键、按键和光标执行；普通后台原样转发证明，不能拆改为原始特权输入。
签署会话收到未签署的 Enter/Leave/Input、无效证明或重放时关闭，不降级到原始输入。
未获管理员 UAC 授权的配对设备不协商此能力，仍使用普通权限输入。详见 ADR 0016。

### 按键

- 修饰键也是普通按键，按下与松开分别发送，不随每个按键附带修饰键掩码。
- 系统不会为注入的按键自动连发，所以连发由捕获端以 `action = 3` 显式发送。

### 剪贴板

```
A（复制的一方）                         B（粘贴的一方）
  ── ClipboardOffer { seq: 9, 文本 12B, PNG 1MB } ─▶
                    （B 上的用户按下粘贴）
  ◀──── ClipboardRequest { seq: 9, PNG, transfer: 3 } ──
  ── Chunk { transfer: 3, last: false, … } ───────▶
  ── Chunk { transfer: 3, last: true,  … } ───────▶
```

- 剪贴板一变就发 `ClipboardOffer`，只说明有哪些格式、各多大，不发内容。没有可共享的格式时发空列表，让对方丢掉之前的通告。
- 对方需要时才发 `ClipboardRequest` 拉取。传输号由请求方分配，在本连接内唯一。
- 何时“需要”由实现决定。当前实现：只有光标所在的一方（活动端）检查本机剪贴板并通告，光标离开的那一刻再检查一次；活动端手上有未拉取的通告就立即拉取。因此复制多次只传最后一次，光标不过去，对方的剪贴板不会被覆盖。
- 写入对方内容后，接收方须记下这次写入，不能把它当作新的复制再通告回去。
- 请求的 `seq` 已经过期时，回 `Cancel(STALE)`。
- 接收方觉得内容太大时，可以不请求，或者中途发 `Cancel(TOO_LARGE)`。
- 当前实现：两边各有一个单项大小上限（用户可调，缺省 32 MB），发送方不通告超过上限的格式，接收方不拉取超过自己上限的格式。
  发送方把一种格式发完就不再保留它：对方一次拉取全部格式，之后同一个 `seq` 再来请求一律回 `Cancel(STALE)`。
- 图片在线上一律是 PNG，HTML 一律是 UTF-8（完整文档或片段都可以）。各平台在读写系统剪贴板时自行转换。

### 文件

```
发送方                         接收方（这里假设接收方是客户端）
  ── FileOffer { id, "设计稿", 2 项, 37 个文件, 84 MB } ─▶     （数据连接）
  ◀───────────────────────── FileAccept { id } ──
                     （客户端新开一条文件连接）
  ◀── Hello { channel: 文件, session: S } … FileOpen { id } ──  （文件连接）
  ── FileList { 32 个条目, last: false } ────────▶
  ── FileList { 5 个条目,  last: true  } ────────▶
  ◀──────────────── FileResume { index: 0, offset: 0 } ──
  ── FileData … FileEnd { index: 1, sha256 } ──▶
  ── FileData … FileEnd { index: 2, sha256 } ──▶
  ◀───────────────────────────────── FileDone ──
```

- 控制消息走数据连接，内容走单独的文件连接。文件连接总由客户端打开：客户端发送时，收到 `FileAccept` 后打开；客户端接收时，回 `FileAccept` 后打开。服务端收到 `FileOpen` 时，`id` 必须对应本会话中已通告、已接受的传输，否则关闭连接。
- `id` 由发送方随机生成。会话中断后，发送方在下次连上时用同一个 `id` 重新发 `FileOffer`，接收方据此接着收。
- 清单按顺序列出所有条目，文件夹在它包含的条目之前。路径不能是绝对路径，不能含空段、`.` 或 `..`；接收方须按本机规则清洗文件名，不能信任对方给的名字。
- 接收方回 `FileResume` 说明从清单中第 `index` 个条目的第 `offset` 个字节开始；全新传输为 `(0, 0)`。之后发送方从这里开始，按清单顺序发出每个文件的内容：若干条 `FileData`（空文件没有），再加一条 `FileEnd`，附整个文件的 SHA-256。文件夹没有内容。
- 每个文件收到的字节数必须等于清单中的 `size`，SHA-256 必须一致，否则接收方关闭连接，这次传输失败。
- 全部收好、校验通过并放到最终位置后，接收方回 `FileDone`，双方关闭文件连接。
- 任一方都可以随时在数据连接上发 `FileCancel` 并关闭文件连接。

### 扩展屏

双方协商 `DISPLAY_PLANS` 后支持两个新变体：kind 5 为带布局启动，依次为 id u64、direction u8、audio bool、source_screen u32、viewer_screen u32、side u8、viewer_primary bool；屏幕值 u32::MAX 表示自动选择。kind 6 为清单，id 固定为 0，依次为 side u8、数量 u8（最多 16）和各屏幕的 id u32、bounds、primary bool。清单的 side 是对端相对发送方的位置。未协商时拒绝收到的新变体，也不发送它们，原有功能照常互连。

- 只有现有输入会话已协商 `DISPLAY_STREAM` 时才能发送 `DisplayControl` 和建立扩展屏媒体连接。老版本继续使用键鼠、文件和剪贴板。
- `kind` 为 0 时启动，1 时确认就绪，2 时结束，3 时报告无法启动，4 时确认首帧已显示。`direction` 为 0 时由请求方创建扩展屏，为 1 时由接收请求的一方创建扩展屏。`audio` 表示是否请求传输声音。
- 媒体 TCP 连接由键鼠客户端发起，两种显示方向共用此规则。接受方核对已配对的证书指纹、输入会话号和能力位。媒体 `Hello` 带输入会话号，`DisplayOpen` 带本次扩展屏请求号，接收运行时还须确认其对应当前等待的请求且只认领一次。
- `track` 为 0 表示视频，为 1 表示音频。同一编码单元包含一个或多个连续分块，最多 2 MiB，末块 `end` 为真，时间戳以本次媒体会话开始为原点，单位为微秒。媒体连接中断、帧超时或分块不一致时结束本次媒体连接。
- 视频为 H.264 Annex B；音频为 48 kHz、双声道、交错排列的小端 16 位 PCM。首版不协商其他媒体格式。
- 来源端在采集编码前取得额度，最多两个视频编码单元等待消费确认，不预先缓存下一幅旧画面。每个视频单元之后发送 `Ping`，接收端将该标记和媒体按序交给同一工作线程，消费到标记后才回 `Pong`。此确认表示解码输入已消费，不代表物理扫描已完成；解码器缓冲或跳过呈现时也须确认。既有 Ping/Pong 的线格式不变，可与原实现互连。
- 媒体发送和读取独立调度，输入不等待视频写入。连续绝对鼠标移动只保留最新位置，不跨越点击、按键或滚轮合并。媒体写入、完整单元重组、无响应及消费停顿各有 5 秒时限；短暂抖动不立即结束会话，也不因此扩大视频队列。
- `DisplayInput` 仅由画面接收端发送，坐标相对 1920×1080 虚拟屏，源端限制在该屏范围。它不承载安全输入签名或系统桌面操作；安全桌面期间暂停画面捕获和这条输入通道。主控扩展自身桌面时暂停该设备的贴边切换；主控显示对端桌面时保留进入对端实体屏的通道，并按本次扩展位置临时调整方向，结束后恢复普通布局。

### 边缘投递

- 服务端在会话开始和用户改变方向时发 `PeerSide`，告诉客户端“服务端在你的哪一侧”。客户端据此知道朝向服务端的是哪条边，在那里显示胶囊。
- 拖着文件靠近相邻的边缘时，发送方发 `DropHint`：`Near` 带上靠近程度，`Over` 表示已在胶囊上；离开边缘、放下或取消后发 `Gone`。接收方据此在同一位置提前滑出接收胶囊。提示只影响显示，真正的发送仍走“文件”一节的流程。
- `along` 用比例而不是坐标，两边分辨率不同也能对齐。
- 提示只在有明显变化时发（状态变化，或靠近程度变化 ≥ 5、位置变化 ≥ 50）；状态变化立即发，其余两条之间至少隔 50 毫秒。
- 接收方收到 `Gone` 后，接收胶囊再停留一会儿：之前是 `Over`（多半是放下了）留 2 秒，等文件传输开始；否则留 0.3 秒。

### 配对

数字比较，带承诺（类似蓝牙 SSP）。A 为接受方，B 为发起方，`fpA`、`fpB` 为双方证书指纹，均取自 TLS 握手：

```
A（接受方）                                  B（发起方）
  ── PairCommit { H(Na) } ─────────────────▶
  ◀──────────────────── PairNonce { Nb } ──
  ── PairNonce { Na } ─────────────────────▶   B 核对 H(Na)
        双方计算配对码，显示给各自的用户核对
  ◀──────────────▶ PairResult { accepted }（各发一次）
```

- `Na`、`Nb` 为 32 字节随机数。承诺 `H(Na) = SHA-256(域标签 · Na)`，配对码 `= SHA-256(域标签 · fpA · fpB · Na · Nb)` 的前 4 字节（小端）对 1 000 000 取余，显示为 `123 456`。每段输入前加 `u32` 长度，域标签见 `ferry-trust` 的 `pairing` 模块。
- 中间人与两边各建一条连接时，两边的指纹不同，配对码一般也不同。承诺迫使中间人在看到对方随机数之前定下自己的随机数，因此无法试算出两边相同的配对码，猜中概率为百万分之一。
- 用户确认或拒绝后立即发 `PairResult`，不等对方。任何一方拒绝即配对失败；双方都确认后，各自把对方指纹加入信任列表。
- B 收到的 `Na` 与承诺不符时直接放弃配对。

## 7. 演进规则

目标：任意两个已发布的版本都能互连，功能取双方都会的那部分。

1. 已发布消息的消息体格式不改。需要新字段时，定义新的消息类型。
2. 新的消息类型必须由新的能力位控制。对方没有声明这个能力时，就不发这类消息；收到未协商的消息按协议违规处理。
3. 新能力必须有不依赖它的退路：对方没有时，功能关掉或退回旧的做法，不能因此连不上。
4. 能力位和类型号一经分配不再复用、不改含义；废弃的保留不用。不认识的能力位原样保留，协商时自然被交集去掉。
5. 帧格式、`Hello`、`Welcome`、`Reject` 和格式标记 `1` 不改。它们一变，所有旧版本都连不上；
   真到非改不可的那天，才把格式标记加一，并在更新说明里写明两台要一起更新。

## 8. 与 Deskflow 的主要差异

| 方面 | Deskflow | ScreenFerry |
|---|---|---|
| 消息编码 | 4 字符消息码，按格式串解析 | `u16` 类型号，严格的规范编码，帧头即可拒绝非法帧 |
| 连接 | 一条连接承载所有数据 | 输入连接与数据连接分离 |
| 按键 | X11 keysym + 修饰键掩码 + 按键号 | USB HID 物理键，修饰键独立传输 |
| 剪贴板 | 进入屏幕时推送完整内容 | 先通告，粘贴时再按格式拉取，可取消 |
| 心跳 | 固定间隔保活 | `Ping`/`Pong` 带往返时间，频率由引擎按状态调整 |
| 版本兼容 | 主次版本号 | 能力位协商，没有单独的协议版本 |

### 物理接缝校准能力

`DISPLAY_ALIGNMENT`（bit 13）表示支持校准后的扩展屏方案。`DisplayControl` kind 7 保持 kind 5 的所有字段，在 `primary` 后追加 `offset i32`、`span u32`（本机接缝千分比），取值为 `100 <= span <= 10000`、`1-span <= offset <= 999`。整边对应 `offset=0,span=1000` 继续使用原 kind 5；kind 7 拒绝此冗余表示。仅双方协商此能力时发送 kind 7，否则校准方案提示对端升级，未校准方案仍正常互连。`WIRE_FORMAT` 不变。
`ALIGNMENT_RULER`（bit 14）表示支持两端实屏取点。取点控制消息使用 kind 8、9、10，分别表示显示尺子、交换两个点和结束。每个点是沿边缘的千分位坐标，只有两个边界相隔至少 50 个单位时才接受。含取点区间的扩展方案使用 kind 11，附加本机和远端各两个 u16 边界点。能力未协商时保留原有整边映射。

UDP 首个完整帧前的无帧期限为 10 秒，收到首帧后为 3 秒。发送端签署安全输入前合并连续绝对移动，点击、按键、滚轮、进入/离开和控制消息均为屏障；接收端不得丢弃已签署动作，证明序号必须连续。

//! 内置的发布公钥（minisign 格式，`.pub` 文件第二行的 base64）。
//!
//! 两把：日常发布用一把，私钥放在 CI 的 Secrets 里；另一把的私钥离线保存，日常那把泄露时，
//! 用它签一版把日常那把换掉。`cargo xtask update-keygen` 生成密钥对并改写下面的列表。
//! 列表为空时检查更新会提示“无法校验更新”。

/// 任何一把对得上，更新清单就算可信。
pub const RELEASE_KEYS: &[&str] = &[
    // update-keygen：开始
    // 日常发布用，生成于 2026-11-17
    "RWQPNaNu1YRxhA5UQuXVSgkWNtKAyPdG8/RZpDPL4ygWrRPuBRtu8nOD",
    // 备用，私钥离线保存，生成于 2026-11-17
    "RWSW8QkWiTZ11RND89Qvqoaou+Ot73zaTqwm1Y7CTt2UdXLf1HPs00rC",
    // update-keygen：结束
];

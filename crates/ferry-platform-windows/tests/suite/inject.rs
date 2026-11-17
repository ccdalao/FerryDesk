use ferry_platform_windows::is_input_peer_authorized;

#[test]
fn missing_policy_can_be_checked_through_the_standard_windows_software_acl() {
    // 真实 Windows 的 SOFTWARE 默认包含 CREATOR OWNER 的可继承 ACE。
    // 查询不存在的设备不写注册表，也不要求管理员或已安装输入服务。
    let peer = "0".repeat(64);
    assert!(
        !is_input_peer_authorized("S-1-5-18", &peer)
            .expect("默认注册表继承占位项不应被误判为普通用户写权限")
    );
}

#[test]
fn policy_query_rejects_invalid_identity_arguments() {
    assert!(is_input_peer_authorized("invalid-sid", &"0".repeat(64)).is_err());
    assert!(is_input_peer_authorized("S-1-5-18", "short").is_err());
}

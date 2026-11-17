use ferry_core::{MediaBitrate, MediaRecovery};

#[test]
fn lost_reference_chain_waits_for_idr_including_a_four_byte_start_code() {
    let mut recovery = MediaRecovery::default();
    assert!(!recovery.accept(&[0, 0, 1, 0x41, 3]));
    assert!(recovery.accept(&[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x65, 3]));
    assert!(recovery.accept(&[0, 0, 1, 0x41, 4]));
    recovery.lost();
    assert!(!recovery.accept(&[0, 0, 1, 0x67, 4]));
    assert!(recovery.waiting());
    assert!(recovery.accept(&[0, 0, 1, 0x65, 4]));
}

#[test]
fn short_jitter_and_successful_fec_do_not_destroy_desktop_quality() {
    let mut rate = MediaBitrate::default();
    assert_eq!(rate.current(), 24_000_000);
    for _ in 0..5 {
        assert_eq!(rate.observe(10, 0, 200, 25), None);
        assert_eq!(rate.observe_delay(35, 2), None);
    }
    assert_eq!(rate.current(), 24_000_000);
    assert_eq!(rate.observe(10, 1, 0, 0), None);
    assert_eq!(rate.observe(10, 0, 0, 2), None);
    assert_eq!(rate.current(), 24_000_000);
}
#[test]
fn persistent_congestion_has_a_quality_floor_cooldown_and_fast_recovery() {
    let mut rate = MediaBitrate::default();
    for _ in 0..300 {
        rate.observe(10, 2, 0, 150);
    }
    assert_eq!(rate.current(), 8_000_000);
    for _ in 0..200 {
        rate.observe(10, 0, 0, 2);
    }
    assert!(rate.current() >= 24_000_000);
    assert!(rate.current() <= 40_000_000);
}
#[test]
fn only_sustained_network_delay_reduces_bitrate() {
    let mut rate = MediaBitrate::default();
    assert_eq!(rate.observe_delay(100, 2), None);
    assert_eq!(rate.observe_delay(100, 2), Some(19_200_000));
    assert_eq!(rate.observe_delay(120, 2), None);
    assert_eq!(rate.observe_delay(3, 2), None);
}

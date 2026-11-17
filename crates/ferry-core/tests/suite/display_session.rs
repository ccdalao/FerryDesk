use std::time::{Duration, Instant};

#[test]
fn extension_alignment_changes_only_crossings_and_is_reversible() {
    use ferry_core::calibrated_display_crossing;
    use ferry_types::{EdgeAlignment, Point, Rect, Side};
    let local = Rect::new(0, 0, 1920, 1080);
    let remote = Rect::new(1920, 0, 1920, 1080);
    let a = EdgeAlignment {
        offset: 200,
        span: 600,
        contact: None,
    };
    let p = calibrated_display_crossing(
        local,
        remote,
        Side::Right,
        a,
        Point::new(1919, 540),
        Point::new(1921, 540),
    );
    assert_eq!(p, Point::new(1921, 540));
    let p = calibrated_display_crossing(
        local,
        remote,
        Side::Right,
        a,
        Point::new(1919, 216),
        Point::new(1921, 216),
    );
    assert_eq!(p, Point::new(1921, 0));
    assert_eq!(
        calibrated_display_crossing(local, remote, Side::Right, a, p, Point::new(1918, 0)),
        Point::new(1918, 216)
    );
    assert_eq!(
        calibrated_display_crossing(
            local,
            remote,
            Side::Right,
            a,
            Point::new(1919, 100),
            Point::new(1921, 100)
        ),
        Point::new(1919, 100)
    );
    assert_eq!(
        calibrated_display_crossing(
            local,
            remote,
            Side::Right,
            a,
            Point::new(2000, 200),
            Point::new(2001, 201)
        ),
        Point::new(2001, 201)
    );
}

use ferry_core::{DisplaySession, DisplaySessionKey};
use ferry_types::DisplayAction;

#[test]
fn extension_positions_share_exact_edges_in_all_four_directions() {
    use ferry_types::{Rect, Side};
    let anchor = Rect::new(-1920, -120, 1920, 1080);
    for (side, expected) in [
        (Side::Left, Rect::new(-3840, -120, 1920, 1080)),
        (Side::Right, Rect::new(0, -120, 1920, 1080)),
        (Side::Top, Rect::new(-1920, -1200, 1920, 1080)),
        (Side::Bottom, Rect::new(-1920, 960, 1920, 1080)),
    ] {
        assert_eq!(
            ferry_core::display_extension_bounds(anchor, 1920, 1080, side),
            Some(expected)
        );
    }
    assert!(
        ferry_core::display_extension_bounds(
            Rect::new(i32::MIN, 0, 10, 10),
            1920,
            1080,
            Side::Left
        )
        .is_none()
    );
}

#[test]
fn both_ends_agree_on_the_desktop_owner() {
    for (action, controller_is_source) in [
        (DisplayAction::ExtendController, true),
        (DisplayAction::ExtendControlled, false),
    ] {
        assert_eq!(action.local_source(true), Some(controller_is_source));
        assert_eq!(action.local_source(false), Some(!controller_is_source));
    }
    assert_eq!(DisplayAction::Stop.local_source(true), None);
    assert_eq!(DisplayAction::Stop.local_source(false), None);
}

#[test]
fn media_must_match_all_three_parts_of_the_current_session() {
    let key = DisplaySessionKey {
        fingerprint: [1; 32],
        input_session: 4,
        display_session: 9,
    };
    let now = Instant::now();
    let mut session = DisplaySession::starting(key, true, false, now);
    for wrong in [
        DisplaySessionKey {
            fingerprint: [2; 32],
            ..key
        },
        DisplaySessionKey {
            input_session: 5,
            ..key
        },
        DisplaySessionKey {
            display_session: 8,
            ..key
        },
    ] {
        assert!(!session.claim_media(wrong, now));
    }
    assert!(!session.first_frame(key, now));
    assert!(session.claim_media(key, now));
    assert!(!session.claim_media(key, now));
    assert!(session.first_frame(key, now));
    assert!(session.is_running());
    assert!(!session.expired(now + Duration::from_secs(60)));
}

#[test]
fn an_expired_start_cannot_take_over_the_screen_later() {
    let key = DisplaySessionKey {
        fingerprint: [1; 32],
        input_session: 4,
        display_session: 9,
    };
    let now = Instant::now();
    let mut session = DisplaySession::starting(key, false, true, now);
    let late = now + Duration::from_secs(11);
    assert!(session.expired(late));
    assert!(!session.claim_media(key, late));
    assert!(!session.first_frame(key, late));
}

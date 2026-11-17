use ferry_core::InputQueue;
use ferry_types::{InputEvent, KeyAction, KeyCode, MouseButton, Point};

fn motion(x: i32) -> InputEvent {
    InputEvent::PointerMotion {
        position: Point::new(x, 10),
    }
}

#[test]
fn high_rate_motion_keeps_latest_position_and_preserves_click_and_key_order() {
    let mut queue = InputQueue::new(8);
    for x in 0..10_000 {
        assert!(queue.push(motion(x)));
    }
    let click = InputEvent::Button {
        button: MouseButton::LEFT,
        pressed: true,
        position: Point::new(9999, 10),
    };
    let key = InputEvent::Key {
        code: KeyCode::keyboard(4),
        action: KeyAction::Down,
    };
    assert!(queue.push(click));
    for x in 10_000..20_000 {
        assert!(queue.push(motion(x)));
    }
    assert!(queue.push(key));
    assert_eq!(queue.pop(), Some(motion(9999)));
    assert_eq!(queue.pop(), Some(click));
    assert_eq!(queue.pop(), Some(motion(19_999)));
    assert_eq!(queue.pop(), Some(key));
    assert_eq!(queue.pop(), None);
}

#[test]
fn full_queue_never_silently_drops_a_key_release_or_relative_motion() {
    let mut queue = InputQueue::new(2);
    let down = InputEvent::Key {
        code: KeyCode::keyboard(4),
        action: KeyAction::Down,
    };
    let up = InputEvent::Key {
        code: KeyCode::keyboard(4),
        action: KeyAction::Up,
    };
    assert!(queue.push(down));
    assert!(queue.push(motion(1)));
    assert!(queue.push(motion(2)));
    assert!(!queue.push(up));
    assert!(!queue.push(InputEvent::PointerDelta { dx: 1, dy: 2 }));
    assert_eq!(queue.pop(), Some(down));
    assert!(queue.push(up));
    assert_eq!(queue.pop(), Some(motion(2)));
    assert_eq!(queue.pop(), Some(up));
}

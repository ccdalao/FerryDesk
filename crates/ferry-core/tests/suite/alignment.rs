use ferry_core::{alignment_from_points, alignment_ruler_scene};
use ferry_types::{EdgeAlignment, Lang, Rect};
#[test]
fn two_corresponding_points_determine_the_same_physical_seam_in_both_directions() {
    assert_eq!(
        alignment_from_points((200, 800), (0, 1000)),
        Some(EdgeAlignment {
            offset: 200,
            span: 600,
            contact: Some(ferry_types::EdgeContact {
                local: [200, 800],
                remote: [0, 1000]
            })
        })
    );
    assert_eq!(
        alignment_from_points((0, 1000), (200, 800)),
        Some(EdgeAlignment {
            offset: -333,
            span: 1666,
            contact: Some(ferry_types::EdgeContact {
                local: [0, 1000],
                remote: [200, 800]
            })
        })
    );
    assert_eq!(
        alignment_from_points((800, 200), (1000, 0)),
        Some(EdgeAlignment {
            offset: 200,
            span: 600,
            contact: Some(ferry_types::EdgeContact {
                local: [200, 800],
                remote: [0, 1000]
            })
        })
    );
    assert_eq!(alignment_from_points((200, 205), (0, 1000)), None);
    assert_eq!(alignment_from_points((0, 1000), (20, 21)), None);
    assert_eq!(alignment_from_points((0, 1001), (0, 1000)), None);
}
#[test]
fn ruler_covers_the_actual_screen_edge_with_clickable_endpoints_and_cancel() {
    for horizontal in [false, true] {
        let screen = Rect::new(-1920, 100, 1920, 1080);
        let (frame, scene) =
            alignment_ruler_scene(screen, 1.5, horizontal, Some(200), Some(800), Lang::Zh);
        assert_eq!(frame.right(), screen.right() as f32);
        assert_eq!(frame.bottom(), screen.bottom() as f32);
        assert!(scene.hits.iter().any(|h| h.id == 1));
        assert!(scene.hits.iter().any(|h| h.id == 2));
        assert!(scene.hits.iter().any(|h| h.id == 1000));
        assert!(scene.hits.iter().any(|h| h.id == 2000));
    }
}

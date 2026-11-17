//! 实屏尺子的两组对应点换算；与窗口、IPC 和网络无关。
use ferry_types::{Align, EdgeAlignment, Frame, Hit, Lang, Rect, Rgba, Scene, Shape, Weight};

/// 两块屏幕上各点两个处于相同实际高度的边界，返回远端相对本机的跨度。
pub fn alignment_from_points(local: (u16, u16), remote: (u16, u16)) -> Option<EdgeAlignment> {
    let (la, lb) = (local.0.min(local.1), local.0.max(local.1));
    let (ra, rb) = (remote.0.min(remote.1), remote.0.max(remote.1));
    if lb > 1000 || rb > 1000 || lb - la < 50 || rb - ra < 50 {
        return None;
    }
    let span = i64::from(lb - la) * 1000 / i64::from(rb - ra);
    let offset = i64::from(la) - i64::from(ra) * span / 1000;
    let value = EdgeAlignment {
        offset: i32::try_from(offset).ok()?,
        span: u32::try_from(span).ok()?,
        contact: Some(ferry_types::EdgeContact {
            local: [la, lb],
            remote: [ra, rb],
        }),
    };
    (value == value.clamped()).then_some(value)
}

pub(crate) fn aligned_edges(
    mut local: Rect,
    mut remote: Rect,
    vertical: bool,
    alignment: EdgeAlignment,
) -> (Rect, Rect) {
    fn portion(rect: &mut Rect, vertical: bool, offset: i32, span: u32) {
        let (origin, length) = if vertical {
            (&mut rect.y, &mut rect.height)
        } else {
            (&mut rect.x, &mut rect.width)
        };
        let len = i64::from(*length);
        *origin = (i64::from(*origin) + len * i64::from(offset) / 1000)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        *length = (len * i64::from(span) / 1000).clamp(1, i64::from(u32::MAX)) as u32;
    }
    if let Some(contact) = alignment.contact {
        for (rect, [start, end]) in [(&mut local, contact.local), (&mut remote, contact.remote)] {
            portion(rect, vertical, i32::from(start), u32::from(end - start));
        }
    } else {
        portion(&mut local, vertical, alignment.offset, alignment.span);
    }
    (local, remote)
}

/// 命中 1000..=2000 分别是接缝 0..=1000 千分位，1 重选，2 取消。
pub fn alignment_ruler_scene(
    screen: Rect,
    scale: f32,
    horizontal: bool,
    first: Option<u16>,
    second: Option<u16>,
    lang: Lang,
) -> (Frame, Scene) {
    let scale = scale.clamp(1.0, 4.0);
    let length = if horizontal {
        screen.width as f32
    } else {
        screen.height as f32
    } / scale;
    let width = 144.0;
    let (w, h) = if horizontal {
        (length, width)
    } else {
        (width, length)
    };
    let frame = if horizontal {
        Frame::new(
            screen.x as f32,
            screen.bottom() as f32 - width * scale,
            screen.width as f32,
            width * scale,
        )
    } else {
        Frame::new(
            screen.right() as f32 - width * scale,
            screen.y as f32,
            width * scale,
            screen.height as f32,
        )
    };
    let mut scene = Scene {
        width: w,
        height: h,
        ..Default::default()
    };
    let ink = Rgba::hex(0xeaf1ff, 255);
    let blue = Rgba::hex(0x69c0ff, 255);
    scene.shapes.push(Shape::RoundRect {
        frame: Frame::new(0.0, 0.0, w, h),
        radius: 0.0,
        fill: Rgba::hex(0x12243b, 245),
    });
    let guide = match (first, second) {
        (None, _) => lang.pick("① 点上界", "① Upper point"),
        (Some(_), None) => lang.pick("② 点下界", "② Lower point"),
        _ => lang.pick("等待另一台", "Waiting for peer"),
    };
    for (y, text) in [
        (8.0, lang.pick("实屏对齐", "Screen alignment")),
        (32.0, guide),
    ] {
        scene.shapes.push(Shape::Text {
            frame: Frame::new(6.0, y, 112.0, 22.0),
            text: text.into(),
            size: 12.0,
            weight: Weight::Semibold,
            color: ink,
            align: Align::Left,
        });
    }
    for (id, y, label) in [
        (1, 62.0, lang.pick("重新取点", "Reset")),
        (2, 92.0, lang.pick("取消", "Cancel")),
    ] {
        let b = Frame::new(6.0, y, 98.0, 25.0);
        scene.shapes.push(Shape::RoundRect {
            frame: b,
            radius: 5.0,
            fill: Rgba::hex(0x315474, 255),
        });
        scene.shapes.push(Shape::Text {
            frame: b,
            text: label.into(),
            size: 12.0,
            weight: Weight::Regular,
            color: ink,
            align: Align::Center,
        });
        scene.hits.push(Hit::click(b, id));
    }
    let point = |v: u16| f32::from(v) * (length - 1.0).max(1.0) / 1000.0;
    for v in (0..=1000).step_by(10) {
        let p = point(v);
        let major = v % 100 == 0;
        let points = if horizontal {
            vec![(p, h - 26.0), (p, h - if major { 46.0 } else { 34.0 })]
        } else {
            vec![(w - 26.0, p), (w - if major { 46.0 } else { 34.0 }, p)]
        };
        scene.shapes.push(Shape::Polyline {
            points,
            width: 1.0,
            color: blue,
        });
        if major {
            let label = if horizontal {
                Frame::new(p - 10.0, h - 65.0, 25.0, 20.0)
            } else {
                Frame::new(
                    w - 73.0,
                    (p - 10.0).clamp(0.0, (h - 20.0).max(0.0)),
                    25.0,
                    20.0,
                )
            };
            scene.shapes.push(Shape::Text {
                frame: label,
                text: (v / 10).to_string(),
                size: 10.0,
                weight: Weight::Regular,
                color: ink,
                align: Align::Center,
            });
        }
    }
    for v in 0..=1000_u16 {
        let p = point(v);
        let next = if v == 1000 { length } else { point(v + 1) };
        let hit = if horizontal {
            Frame::new(p, h - 46.0, (next - p).max(1.0), 46.0)
        } else {
            Frame::new(w - 46.0, p, 46.0, (next - p).max(1.0))
        };
        scene.hits.push(Hit::click(hit, 1000 + u32::from(v)));
    }
    for v in [first, second].into_iter().flatten() {
        let p = point(v);
        let points = if horizontal {
            vec![(p, 0.0), (p, h)]
        } else {
            vec![(0.0, p), (w, p)]
        };
        scene.shapes.push(Shape::Polyline {
            points,
            width: 2.0,
            color: Rgba::hex(0x61f2bb, 255),
        });
    }
    (frame, scene)
}

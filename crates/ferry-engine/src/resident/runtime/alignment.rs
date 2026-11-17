//! 经已配对输入连接同步两端实屏尺；取点只返回草稿，不直接改配置。
use super::*;
use ferry_platform::{Displays, Overlay, OverlayEvent};
use ferry_types::{EdgeAlignment, Rect};

pub(super) struct Ruler<O: Overlay> {
    key: DisplaySessionKey,
    pub(super) client: Option<(u64, u64)>,
    reversed: bool,
    overlay: O,
    input: mpsc::UnboundedReceiver<OverlayEvent>,
    screen: Rect,
    scale: f32,
    horizontal: bool,
    first: Option<u16>,
    second: Option<u16>,
    remote: Option<(u16, u16)>,
    deadline: Instant,
}

impl<H: Host> Runtime<H> {
    fn make_ruler(
        &self,
        key: DisplaySessionKey,
        screen: Option<u32>,
        horizontal: bool,
    ) -> Result<Ruler<H::Overlay>> {
        let screens = self
            .host
            .display_backend()
            .ok_or(Error::Protocol("本机不支持实屏尺"))?
            .screens()?;
        let screen = match screen {
            Some(id) => screens.iter().find(|s| s.id == id),
            None => screens.iter().find(|s| s.primary).or(screens.first()),
        }
        .ok_or(Error::Protocol("所选实体屏幕已断开"))?
        .bounds;
        let scale = self
            .host
            .displays()
            .displays()
            .ok()
            .and_then(|v| v.into_iter().find(|d| d.bounds == screen))
            .map_or(1.0, |d| f32::from(d.scale_percent) / 100.0);
        let (sender, input) = mpsc::unbounded_channel();
        let mut overlay = self.host.overlay();
        overlay.start(Box::new(move |event| {
            let _ = sender.send(event);
        }))?;
        Ok(Ruler {
            key,
            client: None,
            reversed: false,
            overlay,
            input,
            screen,
            scale,
            horizontal,
            first: None,
            second: None,
            remote: None,
            deadline: Instant::now() + Duration::from_secs(120),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn begin_ruler(
        &mut self,
        client: u64,
        id: u64,
        peer: &str,
        local_screen: Option<u32>,
        peer_screen: Option<u32>,
        horizontal: bool,
        reversed: bool,
    ) {
        let link = peer.parse::<Fingerprint>().ok().and_then(|p| {
            self.links
                .iter()
                .find(|l| l.fingerprint == p && l.alignment_ruler && self.peers.contains(&p))
        });
        let Some(link) = link else {
            self.reply(
                client,
                Notice::AlignmentResult {
                    id,
                    alignment: None,
                    error: Some(
                        self.lang
                            .pick(
                                "请将两台电脑升级后使用实屏取点",
                                "Upgrade both PCs to use screen point calibration",
                            )
                            .into(),
                    ),
                },
            );
            return;
        };
        let random = ring::rand::generate::<[u8; 8]>(&ring::rand::SystemRandom::new())
            .map(|r| u64::from_le_bytes(r.expose()))
            .unwrap_or_default();
        if random == 0 {
            self.reply(
                client,
                Notice::AlignmentResult {
                    id,
                    alignment: None,
                    error: Some("创建取点会话失败".into()),
                },
            );
            return;
        }
        let key = DisplaySessionKey {
            fingerprint: *link.fingerprint.as_bytes(),
            input_session: link.session,
            display_session: random,
        };
        self.end_ruler(None, None);
        match self.make_ruler(key, local_screen, horizontal) {
            Ok(mut ruler) => {
                ruler.client = Some((client, id));
                ruler.reversed = reversed;
                self.ruler = Some(ruler);
                if !self.send_display_control(
                    key,
                    DisplayControl::RulerStart {
                        id: random,
                        screen: peer_screen,
                        horizontal,
                    },
                ) {
                    self.end_ruler(None, Some("发送实屏尺请求失败".into()));
                    return;
                }
                self.render_ruler();
            }
            Err(error) => self.reply(
                client,
                Notice::AlignmentResult {
                    id,
                    alignment: None,
                    error: Some(error.to_string()),
                },
            ),
        }
    }

    fn render_ruler(&mut self) {
        if let Some(r) = self.ruler.as_mut() {
            let (frame, scene) = ferry_core::alignment_ruler_scene(
                r.screen,
                r.scale,
                r.horizontal,
                r.first,
                r.second,
                self.lang,
            );
            r.overlay.show(frame, r.scale, scene);
        }
    }

    pub(super) fn end_ruler(&mut self, value: Option<EdgeAlignment>, error: Option<String>) {
        let Some(mut ruler) = self.ruler.take() else {
            return;
        };
        ruler.overlay.hide();
        self.send_display_control(
            ruler.key,
            DisplayControl::RulerStop {
                id: ruler.key.display_session,
            },
        );
        if let Some((client, id)) = ruler.client {
            self.reply(
                client,
                Notice::AlignmentResult {
                    id,
                    alignment: value,
                    error,
                },
            );
        }
    }

    fn complete_ruler(&mut self) {
        let result = self
            .ruler
            .as_ref()
            .filter(|r| r.client.is_some())
            .and_then(|r| Some((r.first?, r.second?, r.remote?, r.reversed)));
        let Some((a, b, remote, reversed)) = result else {
            return;
        };
        match ferry_core::alignment_from_points((a, b), remote) {
            Some(alignment) => self.end_ruler(
                Some(if reversed {
                    alignment.reversed()
                } else {
                    alignment
                }),
                None,
            ),
            None => self.end_ruler(
                None,
                Some(
                    self.lang
                        .pick(
                            "两处边界过近，请重新选择对应位置",
                            "The boundaries are too close; select corresponding points again",
                        )
                        .into(),
                ),
            ),
        }
    }

    pub(super) fn ruler_control(
        &mut self,
        peer: Fingerprint,
        session: u64,
        control: DisplayControl,
    ) {
        if !self
            .links
            .iter()
            .any(|l| l.fingerprint == peer && l.session == session && l.alignment_ruler)
        {
            return;
        }
        let key = DisplaySessionKey {
            fingerprint: *peer.as_bytes(),
            input_session: session,
            display_session: control.id(),
        };
        match control {
            DisplayControl::RulerStart {
                id,
                screen,
                horizontal,
            } if id != 0 => {
                if let Some(existing) = &self.ruler {
                    if existing.key != key {
                        self.send_display_control(key, DisplayControl::RulerStop { id });
                    }
                    return;
                }
                match self.make_ruler(key, screen, horizontal) {
                    Ok(ruler) => {
                        self.ruler = Some(ruler);
                        self.render_ruler();
                    }
                    Err(error) => {
                        warn!(%error,"无法显示对端请求的实屏尺");
                        self.send_display_control(key, DisplayControl::RulerStop { id });
                    }
                }
            }
            DisplayControl::RulerPoints { start, end, .. } => {
                if let Some(r) = self.ruler.as_mut().filter(|r| r.key == key) {
                    r.remote = (start != end).then_some((start, end));
                    self.complete_ruler();
                }
            }
            DisplayControl::RulerStop { .. } => {
                if self.ruler.as_ref().is_some_and(|r| r.key == key) {
                    self.end_ruler(None, None);
                }
            }
            _ => {}
        }
    }

    pub(super) fn poll_ruler(&mut self) {
        let Some(r) = &self.ruler else {
            return;
        };
        let disconnected = !self.links.iter().any(|l| {
            l.fingerprint.as_bytes() == &r.key.fingerprint && l.session == r.key.input_session
        });
        let closed = r
            .client
            .is_some_and(|(id, _)| !self.clients.contains_key(&id));
        if Instant::now() >= r.deadline || disconnected || closed {
            self.end_ruler(
                None,
                Some(
                    self.lang
                        .pick(
                            "实屏取点已结束或连接中断",
                            "Screen calibration ended or the connection was lost",
                        )
                        .into(),
                ),
            );
            return;
        }
        for _ in 0..32 {
            let Some(r) = self.ruler.as_mut() else {
                break;
            };
            let Ok(event) = r.input.try_recv() else {
                break;
            };
            match event {
                OverlayEvent::Clicked(2) => {
                    self.end_ruler(None, None);
                    return;
                }
                OverlayEvent::Clicked(1) => {
                    r.first = None;
                    r.second = None;
                    let key = r.key;
                    self.send_display_control(
                        key,
                        DisplayControl::RulerPoints {
                            id: key.display_session,
                            start: 0,
                            end: 0,
                        },
                    );
                }
                OverlayEvent::Clicked(hit @ 1000..=2000) => {
                    let point = (hit - 1000) as u16;
                    if r.second.is_some() {
                        continue;
                    }
                    if r.first.is_none() {
                        r.first = Some(point);
                        r.second = None;
                    } else {
                        if r.first.is_some_and(|first| first.abs_diff(point) < 50) {
                            continue;
                        }
                        r.second = Some(point);
                        let key = r.key;
                        let start = r.first.expect("已有首个边界");
                        self.send_display_control(
                            key,
                            DisplayControl::RulerPoints {
                                id: key.display_session,
                                start,
                                end: point,
                            },
                        );
                    }
                }
                _ => continue,
            }
            self.render_ruler();
            self.complete_ruler();
        }
    }
}

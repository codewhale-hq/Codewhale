//! Terminal avatar selection projects the existing owner frame and clock.
use super::*;
use base64::Engine as _;
use codewhale_ratatui::{
    Paint,
    avatar::{Pack, RegisteredPack},
    avatar_sprite::Sprite,
    whale_girl,
};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
};

const VIEWS: [&str; 6] = [
    "whale",
    "cove",
    "braille",
    "pixels",
    "particles",
    "whale-girl",
];

#[derive(Default)]
pub(super) struct Avatars {
    pub key: String,
    pub view: Option<String>,
    pub action: Option<String>,
    catalog: Vec<RegisteredPack>,
    workspace: PathBuf,
    next: Option<Instant>,
    updated: Option<Instant>,
    pending: Option<Receiver<Vec<RegisteredPack>>>,
    decoded: Option<(String, Vec<u8>, usize, usize)>,
    images: std::collections::BTreeMap<String, Vec<u8>>,
    wanted: Option<(RegisteredPack, usize)>,
    image_pending: Option<Receiver<(String, Option<String>)>>,
    next_image: Option<Instant>,
}
impl Avatars {
    pub fn refresh(&mut self, workspace: &Path, now: Instant) -> bool {
        let mut changed = false;

        let received = self.image_pending.as_ref().map(|rx| rx.try_recv());
        if matches!(received, Some(Err(mpsc::TryRecvError::Disconnected))) {
            self.image_pending = None;
        }
        if let Some(Ok((key, encoded))) = received {
            self.image_pending = None;
            self.next_image = Some(now + Duration::from_secs(2));
            if let Some(encoded) = encoded
                && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&encoded)
            {
                if self.images.len() >= 4 {
                    self.images.clear();
                }
                self.images.insert(key, bytes);
                changed = true;
            }
        }
        if self.image_pending.is_none()
            && self.next_image.is_none_or(|at| now >= at)
            && let Some((entry, page)) = self.wanted.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let (tx, rx) = mpsc::channel();
            self.image_pending = Some(rx);
            let workspace = workspace.to_path_buf();
            let key = entry.image_key(page);
            runtime.spawn(async move {
                let png = codewhale_engine::extension_host::avatars::atlas(
                    workspace,
                    entry.handle,
                    page,
                    &entry.content_hash,
                )
                .await;
                let _ = tx.send((key, png));
            });
        }

        if self.workspace != workspace {
            self.workspace = workspace.into();
            self.images.clear();
            self.wanted = None;
            self.image_pending = None;
            self.next_image = None;
            self.catalog.clear();
            self.decoded = None;
            self.pending = None;
            self.next = None;
            self.updated = None;
            self.view = None;
            self.action = None;
            changed = true;
        }
        let received = self.pending.as_ref().map(|rx| rx.try_recv());
        if matches!(received, Some(Err(mpsc::TryRecvError::Disconnected))) {
            self.pending = None;
        }
        if let Some(Ok(value)) = received {
            self.pending = None;
            self.catalog = value;
            self.updated = Some(now);
            changed = true;
        }
        if self
            .updated
            .is_some_and(|at| now.saturating_duration_since(at) > Duration::from_secs(12))
        {
            self.catalog.clear();
            self.decoded = None;
            self.updated = None;
            changed = true;
        }
        if self.pending.is_none() && self.next.is_none_or(|at| now >= at) {
            self.next = Some(now + Duration::from_secs(5));
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let (tx, rx) = mpsc::channel();
                let workspace = workspace.to_path_buf();
                self.pending = Some(rx);
                runtime.spawn(async move {
                    let _ = tx
                        .send(codewhale_engine::extension_host::avatars::catalog(workspace).await);
                });
            }
        }
        changed
    }
    fn ring(&self) -> impl Iterator<Item = &str> {
        VIEWS
            .into_iter()
            .chain(self.catalog.iter().map(|p| p.key.as_str()))
    }
    pub fn choices(&self) -> String {
        self.ring().collect::<Vec<_>>().join("\n")
    }
    pub fn current(&self) -> &str {
        if self.key.is_empty() {
            "whale"
        } else {
            &self.key
        }
    }
    pub fn select(&mut self, key: &str) -> bool {
        if !self.ring().any(|view| view == key) {
            return false;
        }
        self.key = key.into();
        self.view = None;
        self.action = None;
        self.decoded = None;
        true
    }
    pub fn cycle(&mut self, forward: bool) {
        let ring: Vec<String> = self.ring().map(str::to_owned).collect();
        let at = ring
            .iter()
            .position(|view| view == self.current())
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % ring.len()
        } else {
            (at + ring.len() - 1) % ring.len()
        };
        self.select(&ring[next]);
    }
    fn pack(&self) -> Option<&Pack> {
        if self.key == "whale-girl" {
            Some(whale_girl::pack())
        } else {
            self.catalog
                .iter()
                .find(|p| p.key == self.key)
                .map(|p| &p.pack)
        }
    }
    pub fn preview(&mut self, value: &str, view: bool) -> bool {
        if value == "live" {
            self.action = None;
            self.view = None;
            return true;
        }
        let Some(pack) = self.pack() else {
            return false;
        };
        if view && pack.views.contains_key(value) {
            self.view = Some(value.into());
            self.action = None;
            true
        } else if !view && pack.actions.contains_key(value) {
            self.action = Some(value.into());
            self.view = None;
            true
        } else {
            false
        }
    }
    pub fn paint(
        &mut self,
        area: Rect,
        buf: &mut ratatui::buffer::Buffer,
        act: &str,
        clock: f64,
        reduced: bool,
        theme: &codewhale_ratatui::Theme,
    ) -> bool {
        let Some(sprite) = self.sprite(act, clock, reduced) else {
            return false;
        };
        sprite.paint(area, buf, theme);
        true
    }
    pub fn missing_atlas(&mut self, act: &str, clock: f64, reduced: bool) -> bool {
        self.pack().is_some() && self.sprite(act, clock, reduced).is_none()
    }
    fn sprite(&mut self, act: &str, clock: f64, reduced: bool) -> Option<Sprite<'_>> {
        let pack = self.pack()?;
        let sample = pack.sample(
            act,
            clock,
            reduced,
            self.view.as_deref(),
            self.action.as_deref(),
        );
        if self.key == "whale-girl" {
            return Sprite::new(pack, whale_girl::TERMINAL, 96, 96, sample.index).ok();
        }
        let entry = self.catalog.iter().find(|p| p.key == self.key)?;
        let page = entry.pack.page(sample.index);
        let cache_key = entry.image_key(page);
        let Some(png) = self.images.get(&cache_key) else {
            self.wanted = Some((entry.clone(), page));
            return None;
        };
        if self.decoded.as_ref().is_none_or(|d| d.0 != cache_key) {
            self.decoded = entry.pack.validate_png(png).ok().and_then(|()| {
                let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
                    .ok()?
                    .to_rgba8();
                let (w, h) = (
                    u32::from(entry.pack.tile_width),
                    u32::from(entry.pack.tile_height),
                );
                let mut pixels = Vec::new();
                for i in 0..u32::from(entry.pack.columns) * u32::from(entry.pack.rows) {
                    pixels.extend(
                        image::imageops::crop_imm(
                            &image,
                            i % u32::from(entry.pack.columns) * w,
                            i / u32::from(entry.pack.columns) * h,
                            w,
                            h,
                        )
                        .to_image()
                        .into_raw(),
                    );
                }
                Some((cache_key, pixels, w as usize, h as usize))
            });
        }
        let (_, pixels, w, h) = self.decoded.as_ref()?;
        Sprite::page(&entry.pack, pixels, *w, *h, sample.index).ok()
    }
}

/// Every specific act comes from typed owner activity. Unknown/stale work is
/// plain busy. This is the same acting mapper as the native character; no tool
/// names, labels or captions are classified by an avatar plugin.
pub(super) fn act(raster: Option<&Presentation>, loading: bool) -> (&'static str, f64) {
    use codewhale_protocol::engine_owner::{OwnerFreshness, OwnerPresence};
    use codewhale_ratatui::whale_motion::{Activity, Context, Presence, acting_for};
    let Some(r) = raster else {
        return (if loading { "busy" } else { "rest" }, 0.);
    };
    let fresh =
        r.frame_changed.elapsed() < Duration::from_millis(800) && r.scene.producer_connected;
    let owner = r
        .scene
        .activity
        .as_ref()
        .filter(|a| fresh && a.observed && a.freshness == OwnerFreshness::Fresh);
    let presence = match owner.map(|a| a.authoritative_presence) {
        Some(OwnerPresence::NeedsYou) => Presence::NeedsYou,
        Some(OwnerPresence::Done) => Presence::Done,
        Some(OwnerPresence::Working) => Presence::Working,
        _ if loading => Presence::Working,
        _ => Presence::Idle,
    };
    let activity = owner.map(|a| Activity {
        kind: a.activity_kind.map(|k| k.as_str().into()),
        observed: true,
        parallel: Some(f64::from(a.parallel_agent_count)),
        active: Vec::new(),
    });
    (
        acting_for(
            presence,
            activity.as_ref(),
            &Context {
                live: fresh,
                ..Context::default()
            },
        )
        .id(),
        r.scene.tick as f64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn avatar_previews_validate_names_and_selection_clears_them() {
        let mut avatars = Avatars::default();
        assert!(!avatars.select("plugin:missing:pack"));
        assert!(avatars.select("whale-girl"));
        assert!(avatars.preview("back", true));
        assert_eq!(avatars.view.as_deref(), Some("back"));
        assert!(!avatars.preview("not-an-action", false));
        assert!(avatars.preview("read", false));
        assert_eq!(avatars.action.as_deref(), Some("read"));
        assert!(avatars.view.is_none());
        assert!(avatars.preview("live", false));
        assert!(avatars.action.is_none());
        assert!(avatars.select("whale"));
        assert!(avatars.pack().is_none());
    }
    #[test]
    fn avatar_workspace_change_and_stale_discovery_drop_plugin_art() {
        let mut avatars = Avatars::default();
        let now = Instant::now();
        let entry = RegisteredPack {
            key: "plugin:test:girl".into(),
            handle: 1,
            content_hash: "reviewed".into(),
            pack: whale_girl::pack().clone(),
        };
        avatars.workspace = PathBuf::from("old");
        avatars.catalog.push(entry.clone());
        avatars.images.insert(entry.image_key(0), vec![1]);
        assert!(avatars.select(&entry.key));
        avatars.refresh(Path::new("new"), now);
        assert!(avatars.pack().is_none());
        assert!(avatars.images.is_empty());
        avatars.catalog.push(entry);
        avatars.updated = Some(now - Duration::from_secs(13));
        avatars.refresh(Path::new("new"), now);
        assert!(avatars.pack().is_none());
    }
}

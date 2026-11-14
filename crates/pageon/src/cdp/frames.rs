//! Frames: every iframe of a page target is a CDP frame, in one process (as Chrome
//! with site isolation off). Ids are deterministic; each new page document detaches the old
//! child frames and attaches the new ones.

use pageon_env::Env;
use serde_json::{Value as Json, json};

use super::{Session, hex};

/// A frame of a page target, by pageon's frame index (capture order; 0 is the main frame).
#[derive(Debug, Clone)]
pub(super) struct FrameInfo {
    pub(super) id: String,
    pub(super) parent: Option<usize>,
    pub(super) loader: String,
    pub(super) url: String,
    pub(super) name: String,
    pub(super) loaded: bool,
}

impl<E: Env> Session<'_, E> {
    /// Rebuilds the frame list from the target's current document (child frames get new ids).
    pub(super) fn rebuild_frames(&mut self, ti: usize) {
        let metas = self.targets[ti].tab.frames_meta();
        let mut frames = Vec::with_capacity(metas.len());
        for (i, m) in metas.into_iter().enumerate() {
            let (id, loader) = if i == 0 {
                (self.targets[ti].id.clone(), self.targets[ti].loader.clone())
            } else {
                let n = self.next_id();
                (hex(0xF7_0000, n), hex(0x10_AD00, n))
            };
            frames.push(FrameInfo {
                id,
                parent: m.parent,
                loader,
                url: m.url,
                name: m.name,
                loaded: m.loaded,
            });
        }
        self.targets[ti].frames = frames;
    }

    /// A CDP `Page.Frame`.
    pub(super) fn frame_json(&self, ti: usize, fi: usize) -> Json {
        let t = &self.targets[ti];
        let Some(f) = t.frames.get(fi) else {
            return json!({});
        };
        let mut j = json!({
            "id": f.id,
            "loaderId": f.loader,
            "url": f.url,
            "domainAndRegistry": "",
            "securityOrigin": crate::frames::origin_of(&f.url),
            "mimeType": "text/html",
            "adFrameStatus": {"adFrameType": "none"},
            "secureContextType": if f.url.starts_with("https:") { "Secure" } else { "InsecureScheme" },
            "crossOriginIsolatedContextType": "NotIsolated",
            "gatedAPIFeatures": [],
        });
        if let Some(p) = f.parent {
            j["parentId"] = json!(t.frames[p].id);
            j["name"] = json!(f.name);
        }
        j
    }

    /// `Page.getFrameTree`'s tree from frame `fi` down.
    pub(super) fn frame_tree(&self, ti: usize, fi: usize) -> Json {
        let children: Vec<Json> = (0..self.targets[ti].frames.len())
            .filter(|&c| self.targets[ti].frames[c].parent == Some(fi))
            .map(|c| self.frame_tree(ti, c))
            .collect();
        let mut j = json!({"frame": self.frame_json(ti, fi)});
        if !children.is_empty() {
            j["childFrames"] = json!(children);
        }
        j
    }

    /// The frame index of a CDP frame id.
    pub(super) fn frame_index(&self, ti: usize, id: &str) -> Option<usize> {
        self.targets[ti].frames.iter().position(|f| f.id == id)
    }

    /// The frame index of an execution context.
    pub(super) fn ctx_frame(&self, ti: usize, ctx: i64) -> Option<usize> {
        self.targets[ti]
            .contexts
            .iter()
            .find(|c| c.id == ctx)
            .map(|c| c.frame)
    }

    /// `Page.frameDetached` for every child frame of the current list, deepest first.
    pub(super) fn detach_frames(&mut self, ti: usize) {
        if !self.targets[ti].page {
            return;
        }
        let ids: Vec<String> = self.targets[ti]
            .frames
            .iter()
            .skip(1)
            .map(|f| f.id.clone())
            .collect();
        for id in ids.into_iter().rev() {
            self.target_event(
                ti,
                "Page.frameDetached",
                json!({"frameId": id, "reason": "remove"}),
            );
        }
    }

    /// `Page.frameAttached` and `Page.frameNavigated` for every child frame, top-down.
    pub(super) fn attach_frames(&mut self, ti: usize) {
        if !self.targets[ti].page {
            return;
        }
        for fi in 1..self.targets[ti].frames.len() {
            let (id, parent) = {
                let f = &self.targets[ti].frames[fi];
                (
                    f.id.clone(),
                    f.parent.map(|p| self.targets[ti].frames[p].id.clone()),
                )
            };
            self.target_event(
                ti,
                "Page.frameAttached",
                json!({"frameId": id, "parentFrameId": parent}),
            );
            let frame = self.frame_json(ti, fi);
            self.target_event(
                ti,
                "Page.frameNavigated",
                json!({"frame": frame, "type": "Navigation"}),
            );
        }
    }
}

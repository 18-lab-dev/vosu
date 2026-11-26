//! The two presentations core derives from the registry: the 2.0 `ToolView`
//! JSON of `GET /api/tools[/<id>]` and the `ToolHostView` pushed to the
//! bundled tool host (A7.1). Neither carries a path, session, code or secret.
use super::{Entry, ToolRegistry};
use bytes::Bytes;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::Arc;
use vosu_surface::tool_view::{ToolHostView, ToolHostViewSender, ToolPresentation};

pub use vosu_surface::tool_view::ToolHostViews;

/// Everything a presentation depends on; an unchanged key is not republished.
/// The entry is retained so its identity cannot be reused while compared.
struct Key {
    entry: Arc<Entry>,
    servable: bool,
    settings: Option<Bytes>,
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.entry, &other.entry)
            && self.servable == other.servable
            && self.settings == other.settings
    }
}

/// The dashboard origin and per-tool keys of the last published view.
type Presented = (Option<String>, Vec<Key>);

pub(super) struct Publisher {
    sender: ToolHostViewSender,
    /// The last generation and what it presented.
    last: Mutex<(u64, Option<Presented>)>,
}
impl Publisher {
    pub(super) fn new(dashboard: Option<String>) -> Self {
        Self {
            sender: vosu_surface::tool_view::channel(dashboard).0,
            last: Mutex::new((0, None)),
        }
    }
}

impl ToolRegistry {
    /// The latest-only view channel `vosu.toolhost` follows.
    pub fn tool_host_views(&self) -> ToolHostViews {
        self.view.sender.subscribe()
    }

    /// Republishes the `ToolHostView` when any presented fact changed.
    pub(super) fn publish(&self) {
        let mut last = self.view.last.lock();
        let entries: Vec<Arc<Entry>> = self
            .entries
            .lock()
            .values()
            .filter(|entry| entry.manifest.is_some() && entry.error.is_none())
            .cloned()
            .collect();
        let presented: Vec<(Arc<Entry>, Key)> = entries
            .into_iter()
            .map(|entry| {
                let servable = self.servable(&entry);
                let settings = self.settings(&entry).ok().map(|values| {
                    let values = match &entry.settings {
                        Some(schema) => schema.public(values),
                        None => values,
                    };
                    Bytes::from(Value::Object(values).to_string())
                });
                let key = Key {
                    entry: Arc::clone(&entry),
                    servable,
                    settings,
                };
                (entry, key)
            })
            .collect();
        let dashboard = self.dashboard_url();
        let keys: Vec<&Key> = presented.iter().map(|(_, key)| key).collect();
        if let Some((origin, previous)) = &last.1
            && *origin == dashboard
            && previous.iter().eq(keys.iter().copied())
        {
            return;
        }
        last.0 += 1;
        let tools = presented
            .iter()
            .map(|(entry, key)| {
                let tool = &entry.manifest.as_ref().expect("filtered above").tool;
                Arc::new(ToolPresentation {
                    id: entry.id.clone(),
                    name: tool.shared.name.clone(),
                    version: tool.shared.version.clone(),
                    servable: key.servable,
                    ui_root: tool.ui.root.clone(),
                    ui_entry: tool.ui.entry.clone(),
                    icon: tool.shared.icon.clone(),
                    banner: tool.shared.banner.clone(),
                    settings: key.settings.clone(),
                    root: entry.package.lease(),
                })
            })
            .collect();
        self.view.sender.send_replace(Arc::new(ToolHostView {
            generation: last.0,
            dashboard_origin: dashboard.clone(),
            tools,
        }));
        last.1 = Some((
            dashboard,
            presented.into_iter().map(|(_, key)| key).collect(),
        ));
    }

    /// The 2.0 `ToolView` (A7 redacted DTO).
    pub(super) fn tool_view(&self, entry: &Entry) -> Value {
        let (status, detail) = self.status(entry);
        let tool = entry.manifest.as_ref().map(|m| &m.tool.shared);
        let overlay = self.overlay_url();
        let art = |declared: Option<&String>, name: &str| {
            declared.map(|_| format!("{overlay}/tools/{}/_{name}", entry.id))
        };
        let (requires, optional) = self
            .capability_view(entry)
            .unwrap_or_else(|| (json!([]), json!([])));
        json!({
            "id": entry.id,
            "name": tool.map_or(entry.id.as_str(), |t| &t.name),
            "version": tool.map_or("", |t| &t.version),
            "description": tool.and_then(|t| t.description.as_deref()).unwrap_or_default(),
            "author": tool.and_then(|t| t.author.as_deref()).unwrap_or_default(),
            "homepage": tool.and_then(|t| t.homepage.as_ref()),
            "icon_url": art(tool.and_then(|t| t.icon.as_ref()), "icon"),
            "banner_url": art(tool.and_then(|t| t.banner.as_ref()), "banner"),
            "source": entry.source,
            "update_available": false,
            "trusted": self.trusted(entry),
            "enabled": self.enabled(entry),
            "status": status,
            "status_detail": detail,
            "ui_url": entry.manifest.as_ref().map(|_| format!("{overlay}/tools/{}/", entry.id)),
            "requires": requires,
            "optional": optional,
            "has_settings": entry.settings.is_some(),
        })
    }
}

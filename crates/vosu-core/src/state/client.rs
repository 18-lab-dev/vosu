//! Core-owned client facts from attach and SESSION_IDENTIFY.
use super::{StateObject, StateValue};
use std::sync::Arc;

pub struct ClientFacts {
    pub game: String,
    pub variant: String,
    pub name: String,
    pub version: String,
    pub pid: u32,
    pub session: String,
    pub plugin_id: String,
    pub plugin_version: String,
}

impl ClientFacts {
    pub(super) fn into_value(self, focused: bool) -> StateValue {
        let string = |s: String| StateValue::String(Arc::from(s));
        StateValue::Object(Arc::new(StateObject::new(vec![
            (Arc::from("game"), string(self.game)),
            (Arc::from("variant"), string(self.variant)),
            (Arc::from("name"), string(self.name)),
            (Arc::from("version"), string(self.version)),
            (Arc::from("pid"), StateValue::U64(u64::from(self.pid))),
            (Arc::from("session"), string(self.session)),
            (Arc::from("focused"), StateValue::Bool(focused)),
            (
                Arc::from("plugin"),
                StateValue::Object(Arc::new(StateObject::new(vec![
                    (Arc::from("id"), string(self.plugin_id)),
                    (Arc::from("version"), string(self.plugin_version)),
                ]))),
            ),
        ])))
    }
}

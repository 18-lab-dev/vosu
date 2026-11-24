/// Instance-scoped identity supplied to every plugin host message handler,
/// so one handler implementation can serve every loaded instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginInstanceId(pub(crate) u64);

impl PluginInstanceId {
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }
}

use super::*;

impl<H: HostServices + Send + Sync + Clone + 'static, G: OffsetGenerator + 'static>
    OsuPlugin<H, G>
{
    pub(super) fn dispatch_resource(
        &self,
        message: Message,
        correlation_id: u64,
    ) -> Result<(), PluginError> {
        // PluginState -> resource mailbox. Never a host callback under either.
        let resources = {
            let state = self.state.lock().expect("plugin state mutex poisoned");
            let handle = match &message {
                Message::ResourceResolve(r) => Some(r.handle),
                Message::TreeList(r) => Some(r.handle),
                Message::TreeLookup(r) => Some(r.handle),
                _ => None,
            };
            state
                .sessions
                .values()
                .filter(|session| handle.is_none_or(|h| session.handle == h))
                .filter_map(|session| session.publication.get()?.bus.resources.get())
                .find(|resources| match &message {
                    Message::StreamRead(r) => resources.owns_stream(r.stream),
                    Message::StreamClose(r) => resources.owns_stream(r.stream),
                    _ => true,
                })
                .cloned()
        };
        let Some(resources) = resources else {
            return Err(vosu_status::VOSU_ERR_TARGET_GONE.into());
        };
        resources
            .request(message, correlation_id)
            .map_err(Into::into)
    }
}
pub(super) fn configure<H: HostServices + Send + Sync + 'static>(
    session: &OsuSession,
    host: &Arc<H>,
    ids: Arc<std::sync::atomic::AtomicU64>,
) {
    let publication = session.publication.get().expect("publication configured");
    let resource_host: Arc<dyn HostServices + Send + Sync> = host.clone();
    let publisher: Arc<dyn crate::resources::publication::RefreshPublication> =
        publication.bus.clone();
    let resources = crate::resources::publication::Publication::start(
        Arc::downgrade(&publisher),
        resource_host,
        session.handle,
        ids,
    );
    assert!(
        publication.bus.resources.set(resources).is_ok(),
        "resources installed once before registration"
    );
}

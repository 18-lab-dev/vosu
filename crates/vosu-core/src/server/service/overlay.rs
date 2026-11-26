/// The bundled overlay host's in-process composition (M7 C0): the counter
/// service, `/websocket/commands` processing and relays, the static
/// provider and the overlay root generations, owned by the executable's
/// [`crate::runtime::BundledHosts`]. Core keeps admission, CSRF,
/// folder opening, the removed-route notices and the legacy socket bridge;
/// it reaches the host only through this object.
pub trait OverlayComposition: Send + Sync {
    /// The effective overlay root the host serves now.
    fn current_root(&self) -> PathBuf;
    /// Switches to a newly resolved overlay root and delivers it to the host.
    /// Runs in `ConfigStore`'s before-publication barrier.
    fn reload_root(&self, root: PathBuf);
    /// Registers the host's counter API and `/websocket/commands` feed,
    /// whose connections `commands` frames.
    fn mount(&self, commands: WebSocketHub) -> std::io::Result<()>;
    /// One admitted overlay static request (`CounterStatic`).
    fn serve_static(
        &self,
        request: vosu_surface::routes::SurfaceRequest,
    ) -> vosu_surface::routes::ResponseFuture<'_, vosu_surface::routes::SurfaceResponse>;
    /// Whether `overlay` is an installed overlay with readable settings.
    fn has_settings(&self, overlay: &str) -> bool;
    /// Folder → display name of every overlay installed now; a failed scan
    /// identifies nothing.
    fn installed_names(&self) -> HashMap<String, String>;
    /// The installed overlays the diagnostics report lists.
    fn report_counters(&self) -> Result<Vec<crate::report::ReportCounter>, String>;
    /// Where the host keeps `id`'s user values (catalog removal).
    fn values_path(&self, id: &str) -> PathBuf;
    /// Whether `name` is one overlay folder component the host accepts.
    fn valid_folder(&self, name: &str) -> bool;
    /// The overlay folder a socket's decoded `l=` query names, if any.
    fn query_overlay(&self, query: &str) -> Option<String>;
    /// One legacy command connection: `client_id` is already namespaced by
    /// endpoint, `overlay` the decoded `l=` query, and `control` receives
    /// relayed replies and filters until the session drops. `None` refuses
    /// the connection.
    fn command_session(
        &self,
        namespace: u64,
        client_id: u64,
        overlay: Option<&str>,
        control: Arc<dyn RelayControl>,
    ) -> Option<Box<dyn CommandSession>>;
    /// Stops command processing and relays, retires the root lease and
    /// routes, then joins the counter service. Idempotent.
    fn shutdown(&self) -> ProviderFuture<'_, ()>;
}

/// One registered legacy command connection; dropping it unregisters it.
pub trait CommandSession: Send + Sync {
    fn process(&self, text: &str) -> CommandReply;
}

/// The transport's part of one processed command.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandReply {
    Reply(Bytes),
    /// The parsed, bounded legacy filters of an `applyFilters` command. An
    /// unparseable source is [`CommandReply::NoAck`]: the prior plan stays.
    ApplyFilters(Vec<vosu_surface::filter::Filter>),
    NoAck,
}

/// A legacy connection as the overlay host's relay worker drives it.
pub trait RelayControl: Send + Sync {
    fn try_send_text(&self, bytes: Bytes) -> bool;
    fn close(&self);
    fn apply_filters(&self, filters: Vec<vosu_surface::filter::Filter>);
}
impl RelayControl for websocket::ConnectionControl {
    fn try_send_text(&self, bytes: Bytes) -> bool {
        self.try_send_text(bytes)
    }
    fn close(&self) {
        self.cleanup();
    }
    fn apply_filters(&self, filters: Vec<vosu_surface::filter::Filter>) {
        self.set_filters(filters);
    }
}
impl RelayControl for Arc<dyn vosu_surface::routes::FeedControl> {
    fn try_send_text(&self, bytes: Bytes) -> bool {
        self.as_ref().try_send_text(bytes)
    }
    fn close(&self) {
        self.as_ref().close();
    }
    fn apply_filters(&self, filters: Vec<vosu_surface::filter::Filter>) {
        if let Ok(bytes) = serde_json::to_vec(&filters) {
            let _ = self.replace_filters(Bytes::from(bytes));
        }
    }
}

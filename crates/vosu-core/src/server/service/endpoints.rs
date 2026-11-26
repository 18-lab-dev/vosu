/// The legacy client-id namespace of `/websocket/commands` connections;
/// the other feeds are 1–4 (`WebSocketEndpoints::hub`). Route-originated
/// relays target this namespace.
pub const COMMANDS_NAMESPACE: u64 = 5;

#[derive(Clone)]
pub struct WebSocketEndpoints {
    pub v1: WebSocketHub,
    pub sc: WebSocketHub,
    pub v2: WebSocketHub,
    pub precise: WebSocketHub,
    pub commands: WebSocketHub,
}

impl WebSocketEndpoints {
    fn hub(&self, endpoint: Endpoint) -> (&WebSocketHub, bool, u64) {
        match endpoint {
            Endpoint::WsV1 => (&self.v1, false, 1),
            Endpoint::WsTokens => (&self.sc, false, 2),
            Endpoint::WsV2 => (&self.v2, false, 3),
            Endpoint::WsV2Precise => (&self.precise, false, 4),
            Endpoint::WsCommands => (&self.commands, true, COMMANDS_NAMESPACE),
            _ => unreachable!("hub requested for non-WebSocket endpoint"),
        }
    }
}

#[derive(Clone)]
pub struct ProductionServices {
    pub overlay: Arc<dyn OverlayComposition>,
    pub config: Arc<dyn ConfigUpdateService>,
    pub report_input: Arc<dyn ReportInputProvider>,
    pub report_probe: Arc<dyn ReportProbe>,
    pub updater: Arc<dyn UpdaterHandoff>,
    pub websocket: WebSocketEndpoints,
}

impl ProductionServices {
    pub fn with_native_report_probe(
        overlay: Arc<dyn OverlayComposition>,
        config: Arc<dyn ConfigUpdateService>,
        report_input: Arc<dyn ReportInputProvider>,
        updater: Arc<dyn UpdaterHandoff>,
        websocket: WebSocketEndpoints,
    ) -> Self {
        Self {
            overlay,
            config,
            report_input,
            report_probe: Arc::new(NativeReportProbe),
            updater,
            websocket,
        }
    }
}

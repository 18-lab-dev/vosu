fn subscription_pointer_cases(document: &serde_json::Value) -> Vec<Vec<String>> {
    fn visit(value: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        out.push(path.clone());
        if path.len() >= 5 { return; }
        match value {
            serde_json::Value::Object(object) => for (key, value) in object {
                path.push(key.clone()); visit(value, path, out); path.pop();
            },
            serde_json::Value::Array(array) => for (index, value) in array.iter().take(8).enumerate() {
                path.push(index.to_string()); visit(value, path, out); path.pop();
            },
            _ => {}
        }
    }
    let mut cases = Vec::new();
    visit(document, &mut Vec::new(), &mut cases);
    cases.extend([
        vec!["missing".into()], vec!["/escaped~key".into()],
        vec!["keys".into(), "all".into(), "01".into()],
        vec!["keys".into(), "all".into(), "length".into()],
        vec!["keys".into(), "all".into(), "999999".into()],
        vec!["client".into(), "0".into()],
    ]);
    cases
}

fn subscription_pointer_text(pointer: &[String]) -> String {
    pointer.iter().map(|token| format!("/{}", token.replace('~', "~0").replace('/', "~1"))).collect()
}

fn random_subscription_set(cases: &[Vec<String>], seed: &mut u64) -> Vec<Vec<String>> {
    let mut selected = Vec::new();
    for _ in 0..12 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let pointer = &cases[(*seed >> 32) as usize % cases.len()];
        if !selected.contains(pointer) { selected.push(pointer.clone()); }
    }
    selected
}

#[test]
fn subscription_projection_matches_full_document_for_random_pointer_unions() {
    let snapshot = all_schema_snapshot("狐 \"kitsune\"\n\\尾😀");
    let full = crate::api::v2::document(&snapshot).unwrap();
    let cases = subscription_pointer_cases(&full);
    let mut seed = 0x51ec710_u64;
    for iteration in 0..512 {
        let selected = if iteration == 0 { vec![Vec::new()] }
            else { random_subscription_set(&cases, &mut seed) };
        let pointers: Vec<_> = selected.iter().map(Vec::as_slice).collect();
        let projected = ProductionSnapshotEncoder::subscription_document(&snapshot, &pointers).unwrap();
        for pointer in &selected {
            let text = subscription_pointer_text(pointer);
            let expected = full.pointer(&text).map(|value| serde_json::to_vec(value).unwrap());
            let actual = projected.pointer(&text).map(|value| serde_json::to_vec(value).unwrap());
            assert_eq!(actual, expected, "union {selected:?}, pointer {text}");
        }
    }
}

#[tokio::test]
async fn subscription_projection_keeps_initial_delta_not_ready_and_error_frames_identical() {
    use crate::server::v2_subscription::{
        V2SubscriptionHub, V2SubscriptionOutbound, V2SubscriptionProvider, V2SubscriptionSnapshot,
    };
    struct Provider {
        state: Mutex<(u64, Arc<CombinedSnapshot>)>,
        event: watch::Sender<u64>,
        legacy: bool,
    }
    struct Snapshot { revision: u64, state: Arc<CombinedSnapshot>, legacy: bool }
    impl V2SubscriptionSnapshot for Snapshot {
        fn revision(&self) -> u64 { self.revision }
        fn document(&self, pointers: &[&[String]]) -> Result<Option<serde_json::Value>, ApiError> {
            if self.revision == 4 { return Err(ApiError::serialization("injected failure")); }
            let result = if self.legacy { crate::api::v2::document(&self.state) }
                else { ProductionSnapshotEncoder::subscription_document(&self.state, pointers) };
            match result {
                Ok(value) => Ok(Some(value)),
                Err(ApiError::NotReady) => Ok(None),
                Err(error) => Err(error),
            }
        }
    }
    impl V2SubscriptionProvider for Provider {
        fn subscribe(&self) -> watch::Receiver<u64> { self.event.subscribe() }
        fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
            let (revision, state) = &*self.state.lock().unwrap();
            Arc::new(Snapshot { revision: *revision, state: Arc::clone(state), legacy: self.legacy })
        }
    }
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, protocol::{CloseFrame, frame::coding::CloseCode}};
    let initial = Arc::new(all_schema_snapshot("initial"));
    let changed = Arc::new(all_schema_snapshot("changed"));
    let missing = Arc::new(CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        3, None, false, ApiRuntimeSettings::default(), [],
    ))));
    let full = crate::api::v2::document(&initial).unwrap();
    let cases = subscription_pointer_cases(&full);
    let mut seed = 0xc01ec7_u64;
    for iteration in 0..32 {
        let overlay_pointers: &[&str] = match iteration {
            1 => &["/keys/all", "/beatmap/mode", "/beatmap/time/live", "/state", "/game/paused", "/beatmap/checksum"],
            2 => &["/state/number", "/game/paused", "/beatmap/time/live", "/keys/all", "/play/mode/number", "/play/mods", "/directPath/beatmapFile", "/settings/mania/scrollSpeed", "/settings/mania/scrollDirection/number", "/settings/audio/offset/universal", "/settings/keybinds", "/client"],
            _ => &[],
        };
        let mut selected: Vec<Vec<String>> = if iteration == 0 { vec![Vec::new()] }
            else { vec![vec!["beatmap".into(), "title".into()]] };
        selected.extend(overlay_pointers.iter().map(|pointer| pointer.split('/').skip(1).map(str::to_owned).collect()));
        for pointer in random_subscription_set(&cases, &mut seed) {
            if selected.iter().all(|existing| {
                !existing.starts_with(&pointer) && !pointer.starts_with(existing)
            }) { selected.push(pointer); }
        }
        let pointers: Vec<_> = selected.iter().map(|pointer| subscription_pointer_text(pointer)).collect();
        let providers: Vec<_> = [true, false].into_iter().map(|legacy| Arc::new(Provider {
            state: Mutex::new((1, Arc::clone(&initial))), event: watch::channel(1).0, legacy,
        })).collect();
        let hubs: Vec<_> = providers.iter().map(|provider| V2SubscriptionHub::new(provider.clone(), Duration::ZERO)).collect();
        let mut clients = Vec::new();
        let mut runners = Vec::new();
        let mut bridges = Vec::new();
        for hub in &hubs {
            let listener = bounded!(tokio::net::TcpListener::bind(("127.0.0.1", 0))).unwrap();
            let address = listener.local_addr().unwrap();
            let hub_for_bridge = hub.clone();
            bridges.push(tokio::spawn(async move {
                let (stream, _) = bounded!(listener.accept()).unwrap();
                stream.set_nodelay(true).unwrap();
                let mut socket = bounded!(tokio_tungstenite::accept_async(stream)).unwrap();
                let request = bounded!(socket.next()).unwrap().unwrap().into_data();
                let pointers = crate::server::v2_subscription::parse_subscription_request(&request).unwrap();
                let (connection, mut receiver) = hub_for_bridge.connect();
                connection.subscribe(pointers).unwrap();
                while let Some(frame) = bounded!(receiver.recv()) {
                    let acknowledge = matches!(frame, V2SubscriptionOutbound::Text(_));
                    let message = match frame {
                        V2SubscriptionOutbound::Text(bytes) | V2SubscriptionOutbound::Error(bytes) => Message::Text(bytes.try_into().unwrap()),
                        V2SubscriptionOutbound::Pong(bytes) => Message::Pong(bytes),
                        V2SubscriptionOutbound::Close { code } => Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: "".into() })),
                    };
                    let closing = message.is_close();
                    bounded!(socket.send(message)).unwrap();
                    if acknowledge { connection.write_completed(); }
                    if closing { break; }
                }
            }));
            let (mut socket, _) = bounded!(tokio_tungstenite::connect_async(format!("ws://{address}/websocket/v2/subscribe"))).unwrap();
            bounded!(socket.send(Message::Text(serde_json::to_string(&pointers).unwrap().into()))).unwrap();
            clients.push(socket);
            runners.push(tokio::spawn({ let hub = hub.clone(); async move { bounded!(hub.run()) } }));
        }
        for (revision, snapshot) in [(1, &initial), (2, &changed), (3, &missing), (4, &initial)] {
            if revision != 1 {
                for provider in &providers {
                    *provider.state.lock().unwrap() = (revision, Arc::clone(snapshot));
                    provider.event.send_replace(revision);
                }
            }
            let mut frames = Vec::new();
            for socket in &mut clients {
                let frame = tokio::time::timeout(Duration::from_secs(2), socket.next()).await.unwrap().unwrap().unwrap();
                frames.push(frame);
            }
            assert_eq!(frames[0], frames[1], "revision {revision}, selection {selected:?}");
        }
        let mut closes = Vec::new();
        for socket in &mut clients {
            closes.push(tokio::time::timeout(Duration::from_secs(2), socket.next()).await.unwrap().unwrap().unwrap());
        }
        assert_eq!(closes[0], closes[1]);
        for hub in hubs { hub.shutdown(); }
        for runner in runners { bounded!(runner).unwrap(); }
        for bridge in bridges { bounded!(bridge).unwrap(); }
    }
}

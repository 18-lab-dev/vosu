// Share the frozen benchmark fixture/filter list so tests guard the actual search boundary.
#[allow(dead_code)]
#[path = "../benches/autoresearch_filtered_projection.rs"]
mod benchmark;
use benchmark::*;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use vosu::runtime::{ProductionSnapshotEncoder, SnapshotEncoder};
use vosu::server::websocket::{Schema, StateProvider, StateSnapshot, WebSocketHub};

#[test]
fn frozen_reference_bytes_all_schemas_and_filter_variants() {
    for (schema, api_schema, _) in SCHEMAS {
        for offset in [0, 100, 200] {
            let snapshot = fixture_offset(offset);
            let identical = filters(schema, 0);
            assert_eq!(
                ProductionSnapshotEncoder
                    .encode(&snapshot, api_schema, &identical)
                    .unwrap(),
                reference_offset(schema, 0, offset)
            );
            let mut canonical = std::collections::HashSet::new();
            for index in 0..100 {
                let unique = filters(schema, index);
                assert!(canonical.insert(serde_json::to_vec(&unique).unwrap()));
                assert_eq!(
                    ProductionSnapshotEncoder
                        .encode(&snapshot, api_schema, &unique)
                        .unwrap(),
                    reference_offset(schema, index, offset)
                );
            }
        }
    }
}

// A long cadence isolates generation/writability events from periodic repeats.
struct EventProvider(Arc<Provider>);
impl StateProvider for EventProvider {
    fn poll_rate(&self, _: Schema) -> Duration {
        Duration::from_secs(60)
    }
    fn subscribe(&self, schema: Schema) -> watch::Receiver<u64> {
        self.0.subscribe(schema)
    }
    fn capture(&self, schema: Schema) -> Option<Arc<dyn StateSnapshot>> {
        self.0.capture(schema)
    }
}

#[tokio::test]
async fn unchanged_changed_and_blocked_latest_only_then_cleanup() {
    for (schema, _, _) in SCHEMAS {
        for index in [0, 99] {
            let provider = Arc::new(Provider::new());
            let hub = WebSocketHub::new(schema, Arc::new(EventProvider(provider.clone())));
            let (connection, mut receiver) = hub.connect();
            connection.set_filters(filters(schema, index));
            let runner = tokio::spawn({
                let hub = hub.clone();
                async move { hub.run().await }
            });
            assert_eq!(receive(&mut receiver).await, reference(schema, index));
            connection.write_completed();
            provider.notify_unchanged();
            assert!(
                tokio::time::timeout(Duration::from_millis(20), receiver.recv())
                    .await
                    .is_err()
            );
            provider.publish();
            // Wake is the production mechanism for forced immediate delivery between cadence points.
            hub.wake();
            assert_eq!(
                receive(&mut receiver).await,
                reference_offset(schema, index, 100)
            );
            // Leave the client writing while multiple generations arrive: no queued backlog.
            provider.publish();
            provider.publish();
            assert!(
                tokio::time::timeout(Duration::from_millis(20), receiver.recv())
                    .await
                    .is_err()
            );
            connection.write_completed();
            assert_eq!(
                receive(&mut receiver).await,
                reference_offset(schema, index, 0)
            );
            connection.write_completed();
            assert!(
                tokio::time::timeout(Duration::from_millis(20), receiver.recv())
                    .await
                    .is_err()
            );
            hub.shutdown();
            tokio::time::timeout(Duration::from_secs(1), runner)
                .await
                .unwrap()
                .unwrap();
            connection.cleanup();
            assert_eq!(hub.client_count(), 0);
            assert!(receiver.recv().await.is_none());
        }
    }
}

#[test]
fn escaped_unicode_metadata_and_populated_tournament_reference() {
    use vosu::api::ApiSchema;
    use vosu::api::{
        ApiRuntimeSettings, CombinedSnapshot, ManagerInstanceSnapshot, ManagerSnapshot,
    };
    use vosu::instance::InstanceSettings;
    use vosu::server::websocket::Filter;
    let base = fixture();
    let mut state = (**base.focused().unwrap()).clone();
    Arc::make_mut(&mut state.regular).menu.title = Some("狐 \"kitsune\"\n\\尾".into());
    let state = Arc::new(state);
    let entry = |pid, ipc: Option<f64>| {
        ManagerInstanceSnapshot::new(
            pid,
            InstanceSettings {
                tournament_spectator: ipc.is_some(),
                tournament_ipc_id: ipc,
                devserver_endpoint: None,
            },
            ipc.is_none(),
            state.clone(),
        )
    };
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(7),
        true,
        ApiRuntimeSettings::default(),
        [entry(7, None), entry(8, Some(9.0)), entry(9, Some(2.5))],
    )));
    let field = || Filter::Field("title".into());
    let cases = [
        (
            ApiSchema::V1,
            vec![nested(
                "menu",
                vec![nested("bm", vec![nested("metadata", vec![field()])])],
            )],
            r#"{"menu":{"bm":{"metadata":{"title":"狐 \"kitsune\"\n\\尾"}}}}"#,
        ),
        (
            ApiSchema::V2,
            vec![nested("beatmap", vec![field()])],
            r#"{"beatmap":{"title":"狐 \"kitsune\"\n\\尾"}}"#,
        ),
        (
            ApiSchema::StreamCompanion,
            vec![Filter::Field("titleRoman".into())],
            r#"{"titleRoman":"狐 \"kitsune\"\n\\尾"}"#,
        ),
        (
            ApiSchema::V2Precise,
            vec![nested(
                "tourney",
                vec![
                    nested(
                        "0",
                        vec![
                            Filter::Field("ipcId".into()),
                            nested("hitErrors", vec![Filter::Field("99".into())]),
                        ],
                    ),
                    nested("1", vec![Filter::Field("ipcId".into())]),
                ],
            )],
            r#"{"tourney":{"0":{"ipcId":2.5,"hitErrors":{"99":99}},"1":{"ipcId":9}}}"#,
        ),
    ];
    for (schema, filters, expected) in cases {
        assert_eq!(
            ProductionSnapshotEncoder
                .encode(&snapshot, schema, &filters)
                .unwrap(),
            expected.as_bytes()
        );
    }
}

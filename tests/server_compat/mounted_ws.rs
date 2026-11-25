//! Real adapter feeds and API v1 on one mounted listener: actual filters,
//! cadence, a non-acknowledging consumer, concurrent demand and cleanup.
use super::mounted::*;
use super::*;

fn object_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// Reads frames until one satisfies `done`, bounded by the caller's deadline.
async fn until(socket: &mut Ws, done: impl Fn(&Value) -> bool) -> Value {
    loop {
        let frame: Value = serde_json::from_str(&text(socket).await).unwrap();
        if done(&frame) {
            return frame;
        }
    }
}

fn publish_score(m: &Mounted, score: i64) -> tokio::task::JoinHandle<()> {
    let mut input = m.input.clone();
    input.services.gameplay["score"] = json!(score);
    let publisher = m.publisher.clone().unwrap();
    tokio::task::spawn_blocking(move || {
        publisher
            .publish(oracle::state(&input), vosu_osu_state::Delivery::All)
            .unwrap();
    })
}

async fn settle(what: &str, done: impl Fn() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} did not settle"));
}

#[test]
fn mounted_feeds_filter_pace_isolate_slow_consumers_and_release_demand_beside_api_v1() {
    let m = Mounted::new(Some(seeded_play()));
    let v2 = m.expected("v2");
    let full: Value = serde_json::from_slice(&v2).unwrap();
    let baseline = m.demand();
    let hubs = m.hubs.as_ref().unwrap();
    m.block_on(async {
        // API v1 subscribes first and stays open across every adapter client.
        let mut api = m.ws("/api/v1/sessions/active/subscribe", None).await.unwrap();
        api.send(WsMessage::Text(json!(["/vosu.osu/play/score"]).to_string().into()))
            .await
            .unwrap();
        assert!(text(&mut api).await.contains("1234567"), "API v1 initial frame");
        settle("API v1 demand", || m.demand() != baseline).await;
        let api_only = m.demand();

        let mut plain = m.ws("/websocket/v2", None).await.unwrap();
        let mut filtered = m.ws("/websocket/v2", None).await.unwrap();
        let mut tokens = m.ws("/tokens", None).await.unwrap();
        assert_eq!(text(&mut plain).await.as_bytes(), v2);
        assert_eq!(text(&mut filtered).await.as_bytes(), v2);
        let sc: Value = serde_json::from_str(&text(&mut tokens).await).unwrap();
        settle("adapter demand", || {
            let demand = m.demand();
            demand.len() > api_only.len() && api_only.iter().all(|s| demand.contains(s))
        })
        .await;

        // Real applyFilters command: removed members cannot be resurrected,
        // and nothing outside the requested members is emitted.
        filtered
            .send(WsMessage::Text(
                r#"applyFilters:[{"field":"play","keys":["score","pressHitErrors","source"]},"keys","resultsScreen"]"#.into(),
            ))
            .await
            .unwrap();
        let frame = until(&mut filtered, |frame| {
            frame.as_object().unwrap().len() < full.as_object().unwrap().len()
        })
        .await;
        assert_eq!(object_keys(&frame), ["play", "resultsScreen"], "{frame}");
        assert_eq!(object_keys(&frame["play"]), ["score"]);
        assert_eq!(frame["play"]["score"], full["play"]["score"]);
        assert_eq!(frame["resultsScreen"], full["resultsScreen"]);
        // streamCompanion's bare filter array is normalized on its own feed only.
        tokens
            .send(WsMessage::Text(r#"["score","combo","pressHitErrors"]"#.into()))
            .await
            .unwrap();
        let frame = until(&mut tokens, |frame| frame.as_object().unwrap().len() < 3).await;
        assert_eq!(object_keys(&frame), ["combo", "score"], "{frame}");
        assert_eq!(frame["score"], sc["score"]);

        // 1.x cadence: the unfiltered feed keeps ticking with no publication.
        for _ in 0..3 {
            assert_eq!(text(&mut plain).await.as_bytes(), v2);
        }

        // A consumer that never acknowledges its write holds one frame only;
        // it neither blocks other clients nor accumulates a backlog.
        let (slow, mut slow_out) = hubs.v2.connect();
        assert!(matches!(slow_out.recv().await, Some(Outbound::Text(_))));
        publish_score(&m, 7_654_321).await.unwrap();
        until(&mut plain, |f| f["play"]["score"] == 7_654_321).await;
        until(&mut filtered, |f| f["play"]["score"] == 7_654_321).await;
        until(&mut tokens, |f| f["score"] == 7_654_321).await;
        loop {
            if text(&mut api).await.contains("7654321") {
                break;
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(1), slow_out.recv())
                .await
                .is_err(),
            "an unacknowledged client must not queue further frames"
        );
        // Once acknowledged it receives the latest value, not a stale backlog.
        slow.write_completed();
        let Some(Outbound::Text(latest)) = slow_out.recv().await else {
            panic!("latest frame after acknowledgement");
        };
        let latest: Value = serde_json::from_slice(&latest).unwrap();
        assert_eq!(latest["play"]["score"], 7_654_321);

        // Cleanup releases adapter demand but never API v1's.
        drop(slow);
        drop(slow_out);
        for socket in [&mut plain, &mut filtered, &mut tokens] {
            socket.close(None).await.unwrap();
        }
        drop((plain, filtered, tokens));
        settle("adapter clients", || {
            hubs.v2.client_count() == 0 && hubs.stream_companion.client_count() == 0
        })
        .await;
        settle("adapter demand release", || m.demand() == api_only).await;
        publish_score(&m, 8_888_888).await.unwrap();
        loop {
            if text(&mut api).await.contains("8888888") {
                break;
            }
        }
        api.close(None).await.unwrap();
        drop(api);
        settle("API v1 demand release", || m.demand() == baseline).await;
    });
    m.shutdown();
}

#[test]
fn mounted_precise_and_gosumemory_feeds_follow_their_own_cadence_and_bytes() {
    let m = Mounted::new(Some(seeded_play()));
    let expected = [m.expected("v1"), m.expected("v2-precise")];
    m.block_on(async {
        let mut sockets = [
            m.ws("/ws", None).await.unwrap(),
            m.ws("/websocket/v2/precise", None).await.unwrap(),
        ];
        for (socket, expected) in sockets.iter_mut().zip(&expected) {
            for _ in 0..3 {
                assert_eq!(text(socket).await.as_bytes(), expected);
            }
        }
        // A precise publication reaches the precise feed on its own cadence.
        let mut input = m.input.clone();
        input.services.gameplay["hitErrors"] = json!([9.5, -9.75, 10.25]);
        let publisher = m.publisher.clone().unwrap();
        tokio::task::spawn_blocking(move || {
            publisher
                .publish(oracle::state(&input), vosu_osu_state::Delivery::All)
                .unwrap();
        })
        .await
        .unwrap();
        let precise = until(&mut sockets[1], |frame| {
            frame["hitErrors"] == json!([9.5, -9.75, 10.25])
        })
        .await;
        assert_eq!(object_keys(&precise), ["hitErrors", "keys", "tourney"]);
        for socket in &mut sockets {
            socket.close(None).await.unwrap();
        }
    });
    m.shutdown();
}

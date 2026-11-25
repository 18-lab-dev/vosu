use super::*;
#[tokio::test]
async fn surface_feeds_keep_filters_acknowledgements_and_latest_only_state_per_client() {
    let provider = EchoProvider::new();
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let mut first = hub.surface_feed(false);
    let mut second = hub.surface_feed(false);
    let first_plan = vec![Filter::Field("a".into())];
    let second_plan = vec![Filter::Field("b".into())];
    first
        .state
        .replace_filters(Bytes::from(serde_json::to_vec(&first_plan).unwrap()))
        .unwrap();
    second
        .state
        .replace_filters(Bytes::from(serde_json::to_vec(&second_plan).unwrap()))
        .unwrap();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    async fn text(feed: &mut vosu_surface::routes::SurfaceFeed) -> Bytes {
        match bounded!(feed.receiver.recv()) {
            Some(Outbound::Text(bytes)) => bytes,
            other => panic!("expected state frame: {other:?}"),
        }
    }
    assert_eq!(text(&mut first).await, echo(1, &first_plan));
    assert_eq!(text(&mut second).await, echo(1, &second_plan));
    provider.publish();
    hub.wake();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), first.receiver.recv())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), second.receiver.recv())
            .await
            .is_err()
    );
    first.state.write_completed();
    assert_eq!(text(&mut first).await, echo(2, &first_plan));
    assert!(
        first
            .state
            .replace_filters(Bytes::from_static(b"["))
            .is_err()
    );
    provider.publish();
    hub.wake();
    first.state.write_completed();
    assert_eq!(text(&mut first).await, echo(3, &first_plan));
    second.state.write_completed();
    assert_eq!(text(&mut second).await, echo(3, &second_plan));
    first.state.close();
    assert!(first.state.is_closed());
    assert_eq!(hub.client_count(), 1);
    drop(second);
    assert_eq!(hub.client_count(), 0);
    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test]
async fn each_acknowledgement_of_the_same_generation_gets_its_forced_delivery() {
    let provider = EchoProvider::new();
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let plan = || vec![Filter::Field("a".into())];
    let mut clients: Vec<_> = (0..3)
        .map(|_| {
            let (connection, receiver) = hub.connect();
            connection.set_filters(plan());
            (connection, receiver)
        })
        .collect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    for (_, receiver) in &mut clients {
        assert_eq!(bounded!(echo_text(receiver)), echo(1, &plan()));
    }

    // Generation 2 arrives while every client is still writing frame 1.
    provider.publish();
    hub.wake();
    tokio::task::yield_now().await;
    for (_, receiver) in &mut clients {
        assert!(receiver.try_recv().is_none());
    }

    // Each acknowledgement carries the same ceiling after the previous wake
    // was consumed; none may be suppressed until the 60 s cadence.
    for index in 0..clients.len() {
        clients[index].0.write_completed();
        assert_eq!(bounded!(echo_text(&mut clients[index].1)), echo(2, &plan()));
        for (_, receiver) in &mut clients[index + 1..] {
            assert!(receiver.try_recv().is_none());
        }
    }

    // Acknowledgements batched before the hub runs coalesce yet all deliver.
    provider.publish();
    hub.wake();
    tokio::task::yield_now().await;
    for (connection, _) in &clients {
        connection.write_completed();
    }
    for (_, receiver) in &mut clients {
        assert_eq!(bounded!(echo_text(receiver)), echo(3, &plan()));
    }

    hub.shutdown();
    bounded!(runner).unwrap();
}

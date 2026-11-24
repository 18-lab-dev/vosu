#[tokio::test]
async fn four_hub_tasks_join_without_lingering() {
    struct NoState;
    impl WebSocketStateProvider for NoState {
        fn poll_rate(&self, _: WebSocketSchema) -> Duration { Duration::from_millis(10) }
        fn subscribe(&self, _: WebSocketSchema) -> watch::Receiver<u64> { watch::channel(0).1 }
        fn capture(&self, _: WebSocketSchema) -> Option<Arc<dyn WebSocketStateSnapshot>> { None }
    }
    let provider = Arc::new(NoState);
    let hubs = WebSocketHubs::start(provider);
    bounded!(hubs.shutdown());
}



// `/websocket/commands` `applyFilters` reaches the connection's feed as raw
// legacy filter JSON; the transport parses and bounds it here, so a rejected
// source keeps the prior plan (the overlay host never parses filters).
mod command_filters {
    use super::*;
    use vosu_adapter_support::filtered::{Filter, parse_filters};
    use vosu_overlay_host::commands::{CommandClient, CommandProcessor, SettingsProvider};

    struct NoSettings;
    impl SettingsProvider for NoSettings {
        fn get(&self, _: &str) -> Result<Value, String> {
            Ok(json!({}))
        }
        fn save(&self, _: &str, _: Value) -> Result<(), String> {
            Ok(())
        }
    }
    struct NoRelays;
    impl vosu_overlay_host::commands::RelaySink for NoRelays {
        fn relay(&self, _: vosu_overlay_host::commands::Relay) {}
    }

    fn setup(overlay: &str) -> (CommandProcessor, CommandClient) {
        let processor = CommandProcessor::new(Arc::new(NoSettings), Arc::new(NoRelays));
        let client = CommandClient {
            id: 7,
            overlay: overlay.into(),
        };
        (processor, client)
    }

    /// The filters the transport applies for one processed frame; `None`
    /// keeps the connection's prior plan.
    fn applied(processor: &CommandProcessor, client: &CommandClient, frame: &str) -> Option<Vec<Filter>> {
        match processor.process(client, frame) {
            CommandAction::ApplyFilters(source) => parse_filters(std::str::from_utf8(&source).ok()?),
            _ => None,
        }
    }

    #[test]
    fn filter_envelopes_parse_at_the_transport() {
        let (processor, client) = setup("my overlay");
        assert_eq!(
            applied(
                &processor,
                &client,
                "applyFilters:any:[\"play\",42,{\"unknown\":true},{\"field\":\"menu\",\"keys\":[\"state\",false,{\"field\":\"beatmap\",\"keys\":[\"title\",null]}]}]"
            ),
            Some(vec![
                Filter::Field("play".into()),
                Filter::Nested {
                    field: "menu".into(),
                    keys: vec![
                        Filter::Field("state".into()),
                        Filter::Nested {
                            field: "beatmap".into(),
                            keys: vec![Filter::Field("title".into())],
                        },
                    ],
                },
            ])
        );
        assert_eq!(
            applied(&processor, &client, "applyFilters:[\"legacy\"]"),
            Some(vec![Filter::Field("legacy".into())])
        );
        assert_eq!(applied(&processor, &client, "applyFilters:any:{}"), None);
    }

    #[test]
    fn excessive_filters_are_bounded() {
        let excessive = format!("[{}]", vec!["\"field\""; 257].join(","));
        assert_eq!(parse_filters(&excessive), None);
        let too_deep = format!(
            "{}{}{}",
            "[{\"field\":\"a\",\"keys\":".repeat(9),
            "[\"leaf\"]",
            "}]".repeat(9)
        );
        assert_eq!(parse_filters(&too_deep), None);
    }

    #[test]
    fn nested_filter_fields_use_javascript_truthiness_and_property_keys() {
        let keys = |field: &str| Filter::Nested {
            field: field.into(),
            keys: vec![Filter::Field("value".into())],
        };
        assert_eq!(
            parse_filters(
                r#"[{"field":1.0,"keys":["value"]},{"field":true,"keys":["value"]},{"field":[],"keys":["value"]},{"field":[1,null,"x"],"keys":["value"]},{"field":{},"keys":["value"]},{"field":0,"keys":["value"]},{"field":false,"keys":["value"]},{"field":"","keys":["value"]},{"field":null,"keys":["value"]}]"#
            ),
            Some(vec![
                keys("1"),
                keys("true"),
                keys(""),
                keys("1,,x"),
                keys("[object Object]"),
            ])
        );
    }

    #[tokio::test]
    async fn mixed_filter_command_reaches_projection_with_valid_entries_only() {
        use websocket::{Schema, StateProvider, StateSnapshot};

        struct ProjectionSnapshot;
        impl StateSnapshot for ProjectionSnapshot {
            fn generation(&self) -> u64 {
                1
            }

            fn encode_unfiltered(&self) -> Result<Bytes, String> {
                Ok(Bytes::from_static(b"unfiltered"))
            }

            fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String> {
                serde_json::to_vec(filters)
                    .map(Bytes::from)
                    .map_err(|error| error.to_string())
            }
        }

        struct ProjectionProvider {
            events: tokio::sync::watch::Sender<u64>,
        }
        impl StateProvider for ProjectionProvider {
            fn poll_rate(&self, _: Schema) -> std::time::Duration {
                std::time::Duration::from_millis(10)
            }

            fn subscribe(&self, _: Schema) -> tokio::sync::watch::Receiver<u64> {
                self.events.subscribe()
            }

            fn capture(&self, _: Schema) -> Option<Arc<dyn StateSnapshot>> {
                Some(Arc::new(ProjectionSnapshot))
            }
        }

        let (processor, client) = setup("overlay");
        let Some(filters) = applied(
            &processor,
            &client,
            "applyFilters:overlay:[\"play\",42,{\"field\":\"menu\",\"keys\":[\"state\",null]},{\"other\":true}]",
        ) else {
            panic!("mixed array must apply its supported filters");
        };
        let expected = vec![
            Filter::Field("play".into()),
            Filter::Nested {
                field: "menu".into(),
                keys: vec![Filter::Field("state".into())],
            },
        ];
        assert_eq!(filters, expected);

        let (events, _) = tokio::sync::watch::channel(0);
        let hub = WebSocketHub::new(Schema::V2, Arc::new(ProjectionProvider { events }));
        let (connection, mut frames) = hub.connect();
        connection.set_filters(filters);
        let runner = tokio::spawn({
            let hub = hub.clone();
            async move { vosu_proptest::bounded!(hub.run()) }
        });
        assert_eq!(
            vosu_proptest::bounded!(frames.recv()),
            Some(Outbound::Text(Bytes::from(
                serde_json::to_vec(&expected).unwrap()
            )))
        );
        connection.cleanup();
        hub.shutdown();
        vosu_proptest::bounded!(runner).unwrap();
    }
}

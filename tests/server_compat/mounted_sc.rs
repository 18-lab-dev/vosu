//! HTTP-only streamCompanion reads on the mounted production stack. 1.x always
//! served chart facts; a one-shot reader must neither starve its own chart job
//! nor make chart work start while nobody reads.
use super::mounted::*;
use super::*;

fn chart_facts(case: &str, sc: &Value) -> Value {
    let strains = sc["mapStrains"].as_object().unwrap();
    assert!(!strains.is_empty(), "{case}: chart-less strains: {sc}");
    assert!(sc["bpm"].as_f64().unwrap() > 0.0, "{case}: chart-less bpm");
    assert!(
        sc["maxCombo"].as_i64().unwrap() > 0,
        "{case}: chart-less combo"
    );
    sc["mapStrains"].clone()
}

async fn sc(m: &Mounted) -> Value {
    let response = m
        .client
        .get(format!("{}/json/sc", m.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

#[test]
fn http_only_sc_reads_keep_chart_facts_and_idle_starts_no_chart_work() {
    let m = Mounted::new(Some(seeded_play()));
    let settled = m.fixture.chart.counts();
    assert_eq!(settled.parses, 1, "settling parsed the chart once");
    m.block_on(async {
        // No WebSocket client at any point: every one-shot read has chart facts
        // and reuses the one completed chart for its key.
        let mut previous = None;
        for read in 0..20 {
            let strains = chart_facts(&format!("read {read}"), &sc(&m).await);
            assert!(previous.as_ref().is_none_or(|p| *p == strains));
            previous = Some(strains);
        }
        assert_eq!(
            m.fixture.chart.counts().starts,
            settled.starts,
            "no re-parse"
        );

        // A key change with zero consumers commits on the bus but starts no
        // chart work: idle hubs neither decode nor select.
        let before = m.revision();
        let mut input = m.input.clone();
        input.services.gameplay["mods"] = json!({
            "checksum": "dt", "number": 64, "name": "DT",
            "array": [{"acronym": "DT"}], "rate": 1.5
        });
        let publisher = m.publisher.clone().unwrap();
        tokio::task::spawn_blocking(move || {
            publisher
                .publish(oracle::state(&input), vosu_osu_state::Delivery::All)
                .unwrap();
        })
        .await
        .unwrap();
        assert!(m.revision() > before, "the key change was committed");
        for hub in [
            &m.hubs.as_ref().unwrap().stream_companion,
            &m.hubs.as_ref().unwrap().v2,
        ] {
            assert_eq!(hub.client_count(), 0);
        }
        assert_eq!(m.fixture.chart.counts().starts, settled.starts);

        // The first read after the change waits (bounded) for the new key's
        // job and serves its facts; later reads reuse that one result. The
        // bound is measured on release builds; only an oversubscribed debug
        // host may exceed it, and then the read must have waited the whole
        // bound while the job still completes for the next reader.
        let started = std::time::Instant::now();
        let first = sc(&m).await;
        let first = if first["mapStrains"].as_object().unwrap().is_empty() {
            assert!(
                started.elapsed() >= vosu::server::bus_provider::DERIVED_HTTP_WAIT,
                "a chart-less read returned before the bounded wait elapsed: {:?} {:?} {first}",
                started.elapsed(),
                m.fixture.chart.counts()
            );
            loop {
                let next = sc(&m).await;
                if !next["mapStrains"].as_object().unwrap().is_empty() {
                    break next;
                }
            }
        } else {
            first
        };
        let changed = chart_facts("first read after key change", &first);
        assert_ne!(Some(&changed), previous.as_ref(), "DT changes strains");
        let after = m.fixture.chart.counts();
        assert_eq!(after.starts, settled.starts + 1);
        for read in 0..5 {
            assert_eq!(
                chart_facts(&format!("reuse {read}"), &sc(&m).await),
                changed
            );
        }
        assert_eq!(m.fixture.chart.counts().starts, after.starts);
    });
    m.shutdown();
}

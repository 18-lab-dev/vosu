use super::*;
use crate::{tests::fixture, view::View};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use vosu_plugin_api::{CborValue, StateEntry};
use vosu_surface::{
    files::{FileError, FileMetadata, FileReader, OpenedFile, Operation},
    resources::{PublishedResource, ResourceRequester},
    state::IdentifiedSession,
};
const WAIT: Duration = Duration::from_secs(5);
const MAP: &str = "osu file format v14\n\n[General]\nMode:0\nPreviewTime:1234\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[Events]\n2,2500,3500\n\n[TimingPoints]\n0,500,4,2,1,80,1,0\n1000,500,4,2,1,80,1,1\n2000,400,4,2,1,80,1,0\n\n[HitObjects]\n64,64,1000,1,0,0:0:0:0:\n448,64,1200,1,0,0:0:0:0:\n64,320,1400,1,0,0:0:0:0:\n448,320,1600,1,0,0:0:0:0:\n64,64,4000,1,0,0:0:0:0:\n448,64,4200,1,0,0:0:0:0:\n";
fn view(token: Option<&str>) -> View {
    let mut capture = fixture();
    if let Some(token) = token {
        let mut values = capture.values.to_vec();
        values.push(StateEntry {
            ns: "vosu.chart".into(),
            pointer: "/resources/file".into(),
            error: None,
            value: Some(
                CborValue::serialized(&json!({"url":format!("/api/v1/resources/{token}")}))
                    .unwrap(),
            ),
        });
        capture.values = values.into();
    }
    let mut view = View::decode(capture).unwrap();
    view.play.mode.number = Some(0);
    view.play.mods.rate = 1.0;
    view.metadata.time.mp3_length = Some(5000.0);
    view
}
#[derive(Default)]
struct Fixture {
    opens: AtomicU64,
    reads: Arc<AtomicU64>,
    closes: Arc<AtomicU64>,
    started: Notify,
    gate: Notify,
    block_a: bool,
    fail: AtomicU64,
}
struct Reader {
    bytes: Vec<u8>,
    reads: Arc<AtomicU64>,
    closes: Arc<AtomicU64>,
    fixture: Arc<Fixture>,
    blocked: bool,
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.closes.fetch_add(1, Ordering::Relaxed);
    }
}
impl FileReader for Reader {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.blocked {
                self.fixture.started.notify_one();
                self.fixture.gate.notified().await;
            }
            let start = (offset as usize).min(self.bytes.len());
            Ok(self.bytes[start..(start + len as usize).min(self.bytes.len())].to_vec())
        })
    }
}
struct Requester(Arc<Fixture>);
impl ResourceRequester for Requester {
    fn open_published_resource(
        &self,
        session: &IdentifiedSession,
        token: &str,
    ) -> Operation<'_, PublishedResource> {
        let token = token.to_owned();
        let generation = session.generation;
        Box::pin(async move {
            self.0.opens.fetch_add(1, Ordering::Relaxed);
            if token == "forbidden" || generation == 99 {
                return Err(FileError::PermissionDenied);
            }
            if self.0.fail.swap(0, Ordering::Relaxed) != 0 {
                return Err(FileError::NotReady);
            }
            let content = if token == "b" {
                MAP.replace("500,4", "600,4")
            } else {
                MAP.into()
            };
            let bytes = content.into_bytes();
            Ok(PublishedResource {
                identity: token.clone(),
                media_type: "text/plain",
                file: OpenedFile::new(
                    FileMetadata {
                        size: bytes.len() as u64,
                        modified: None,
                        identity: Arc::from(token.as_bytes()),
                    },
                    Reader {
                        bytes,
                        reads: self.0.reads.clone(),
                        closes: self.0.closes.clone(),
                        fixture: self.0.clone(),
                        blocked: self.0.block_a && token == "a",
                    },
                ),
            })
        })
    }
}
fn worker(fixture: &Arc<Fixture>) -> ChartWorker {
    ChartWorker::new(Arc::new(Requester(fixture.clone())))
}
async fn ready(worker: &ChartWorker, notices: &mut watch::Receiver<u64>) -> Arc<DerivedChart> {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(chart) = worker.current() {
                return chart;
            }
            if let Some(error) = worker.error() {
                panic!("chart failed: {error}");
            }
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("bounded chart completion")
}
async fn failed(worker: &ChartWorker, notices: &mut watch::Receiver<u64>) {
    tokio::time::timeout(WAIT, async {
        while worker.error().is_none() {
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("bounded chart error");
}
#[tokio::test]
async fn parsed_resource_facts_are_nonzero_and_wire_kiai_stays_empty() {
    for variant in ["stable", "lazer"] {
        let fixture = Arc::new(Fixture::default());
        let mut worker = worker(&fixture);
        let mut view = view(Some("a"));
        view.capture.session.as_mut().unwrap().variant = variant.into();
        let mut notices = worker.subscribe();
        worker.select(&view).unwrap();
        let chart = ready(&worker, &mut notices).await;
        assert!(chart.facts.strains.values().any(|v| *v > 0.0));
        assert_eq!(chart.facts.breaks.len(), 1);
        assert_eq!(chart.facts.breaks[0].start_time, 2500.0);
        assert_eq!(chart.kiai.len(), 1);
        assert_eq!(chart.kiai[0].start, 1000.0);
        assert_eq!(chart.kiai[0].end, 2000.0);
        assert_eq!(chart.facts.timing_points.len(), 3);
        assert_eq!(chart.facts.timing_points[2].beat_length, 400.0);
        assert_eq!(chart.facts.objects.circles, 6);
        assert!(chart.facts.difficulty.stars > 0.0);
        let wire: serde_json::Value =
            serde_json::from_slice(&crate::encode(&view, Some(&chart.facts)).unwrap()).unwrap();
        assert_eq!(wire["mapKiaiPoints"], json!([]));
        assert_eq!(wire["mapBreaks"][0]["startTime"], 2500.0);
        assert_eq!(worker.counts().parses, 1);
        assert_eq!(fixture.closes.load(Ordering::Relaxed), 1);
        tokio::time::timeout(WAIT, worker.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}
#[tokio::test]
async fn precise_clients_reuse_chart_and_complete_dependency_key_invalidates() {
    let fixture = Arc::new(Fixture::default());
    let mut worker = worker(&fixture);
    let mut view = view(Some("a"));
    let mut notices = worker.subscribe();
    worker.select(&view).unwrap();
    let first = ready(&worker, &mut notices).await;
    for i in 0..100 {
        view.play_time = f64::from(i);
        view.capture.revision += 1;
        worker.select(&view).unwrap();
        assert!(Arc::ptr_eq(&first, &worker.current().unwrap()));
    }
    assert_eq!(worker.counts().starts, 1);
    assert_eq!(worker.counts().reads, 1);
    assert_eq!(worker.counts().parses, 1);
    view.play.mods.rate = 1.5;
    worker.select(&view).unwrap();
    let rate = ready(&worker, &mut notices).await;
    assert_eq!(worker.counts().parses, 2);
    assert_ne!(rate.facts.bpm.common, first.facts.bpm.common);
    view.metadata.time.mp3_length = Some(20000.0);
    worker.select(&view).unwrap();
    let audio = ready(&worker, &mut notices).await;
    assert_eq!(worker.counts().parses, 2); // projection-only dependency
    assert_ne!(audio.facts.strains.len(), rate.facts.strains.len());
    view.play.mode.number = Some(1);
    worker.select(&view).unwrap();
    ready(&worker, &mut notices).await;
    assert_eq!(worker.counts().parses, 3);
    view.play.mods.array.push(vosu_osu_model::states::Mod {
        acronym: "HR".into(),
        settings: None,
    });
    worker.select(&view).unwrap();
    ready(&worker, &mut notices).await;
    assert_eq!(worker.counts().parses, 4);
    tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn canceled_a_cannot_publish_over_latest_b_and_pending_is_bounded() {
    let fixture = Arc::new(Fixture {
        block_a: true,
        ..Fixture::default()
    });
    let mut worker = worker(&fixture);
    let a = view(Some("a"));
    let mut notices = worker.subscribe();
    worker.select(&a).unwrap();
    tokio::time::timeout(WAIT, fixture.started.notified())
        .await
        .unwrap();
    let b = view(Some("b"));
    for _ in 0..1000 {
        worker.select(&b).unwrap();
    }
    let chart = ready(&worker, &mut notices).await;
    assert_eq!(chart.facts.timing_points[0].beat_length, 600.0);
    fixture.gate.notify_one();
    assert_eq!(worker.counts().starts, 2);
    assert_eq!(worker.counts().cancels, 1);
    assert_eq!(worker.counts().parses, 1);
    assert_eq!(fixture.closes.load(Ordering::Relaxed), 2);
    tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn no_consumer_missing_resource_and_denied_generation_never_read_private_path() {
    let fixture = Arc::new(Fixture::default());
    let mut worker = worker(&fixture);
    let a = view(Some("a"));
    worker.invalidate();
    worker.select(&view(None)).unwrap();
    assert_eq!(worker.counts(), Counts::default());
    let mut notices = worker.subscribe();
    worker.select(&view(Some("forbidden"))).unwrap();
    failed(&worker, &mut notices).await;
    assert!(worker.current().is_none());
    assert_eq!(worker.counts().reads, 0);
    assert_eq!(worker.counts().parses, 0);
    let mut stale = a.clone();
    stale.capture.session.as_mut().unwrap().generation = 99;
    worker.select(&stale).unwrap();
    failed(&worker, &mut notices).await;
    assert_eq!(worker.counts().reads, 0);
    worker.select(&a).unwrap();
    ready(&worker, &mut notices).await;
    // Releasing the last consumer is no worker call: the completed chart for
    // the selected key stays; only explicit invalidation clears it.
    assert_eq!(worker.key(), ChartKey::from_view(&a).unwrap());
    assert!(worker.current().is_some());
    worker.invalidate();
    assert!(worker.current().is_none() && worker.key().is_none());
    tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn released_job_completes_and_is_reused_but_a_new_key_cancels_in_flight_work() {
    let fixture = Arc::new(Fixture {
        block_a: true,
        ..Fixture::default()
    });
    let mut worker = worker(&fixture);
    let mut notices = worker.subscribe();
    let a = view(Some("a"));
    let key = worker.select(&a).unwrap().unwrap();
    tokio::time::timeout(WAIT, fixture.started.notified())
        .await
        .unwrap();
    assert!(worker.pending_for(&key));
    // The one-shot consumer releases now; releasing is no worker call, so the
    // in-flight job is not cancelled and its result is kept for the key.
    fixture.gate.notify_one();
    let chart = ready(&worker, &mut notices).await;
    assert!(!worker.pending_for(&key));
    assert_eq!(worker.counts().cancels, 0);
    let before = worker.counts();
    assert_eq!(worker.select(&a).unwrap(), Some(key.clone()));
    assert!(Arc::ptr_eq(&worker.current_for(&key).unwrap(), &chart));
    assert_eq!(
        worker.counts(),
        before,
        "the same key reuses, never re-parses"
    );
    // A different key cancels in-flight work for the previous one.
    worker.invalidate();
    let key = worker.select(&a).unwrap().unwrap();
    tokio::time::timeout(WAIT, fixture.started.notified())
        .await
        .unwrap();
    let b = worker.select(&view(Some("b"))).unwrap().unwrap();
    ready(&worker, &mut notices).await;
    fixture.gate.notify_one();
    assert!(worker.counts().cancels >= 1);
    assert!(worker.current_for(&key).is_none());
    assert!(worker.current_for(&b).is_some());
    tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn failed_open_is_retried_and_shutdown_cancels_reader_and_joins() {
    let fixture = Arc::new(Fixture {
        block_a: true,
        ..Fixture::default()
    });
    fixture.fail.store(1, Ordering::Relaxed);
    let mut worker = worker(&fixture);
    let a = view(Some("a"));
    let mut notices = worker.subscribe();
    worker.select(&a).unwrap();
    failed(&worker, &mut notices).await;
    worker.select(&a).unwrap();
    tokio::time::timeout(WAIT, fixture.started.notified())
        .await
        .unwrap();
    assert_eq!(worker.counts().opens, 2);
    assert_eq!(worker.counts().reads, 1);
    let counts = tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counts.joins, 1);
    assert_eq!(counts.cancels, 1);
    assert_eq!(fixture.closes.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn completed_a_is_discarded_after_b_is_selected_and_only_latest_pending_runs() {
    let fixture = Arc::new(Fixture::default());
    let mut worker = worker(&fixture);
    let completed = Arc::new(Notify::new());
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let released = std::sync::Mutex::new(released);
    let signal = completed.clone();
    worker.before_completion(Arc::new(move |key| {
        if key.token == "a" {
            signal.notify_one();
            released.lock().unwrap().recv_timeout(WAIT).unwrap();
        }
    }));
    let mut notices = worker.subscribe();
    worker.select(&view(Some("a"))).unwrap();
    tokio::time::timeout(WAIT, completed.notified())
        .await
        .unwrap();
    // A has actually parsed, but cannot publish after the desired chart changes.
    assert_eq!(worker.counts().parses, 1);
    let mut next = view(Some("b"));
    for generation in 10..1010 {
        next.capture.session.as_mut().unwrap().generation = generation;
        worker.select(&next).unwrap();
    }
    assert!(worker.current().is_none());
    release.send(()).unwrap();
    let chart = ready(&worker, &mut notices).await;
    assert_eq!(chart.facts.timing_points[0].beat_length, 600.0);
    assert_eq!(worker.counts().starts, 2);
    assert_eq!(worker.counts().parses, 2);
    assert_eq!(worker.counts().cancels, 1);
    assert_eq!(fixture.closes.load(Ordering::Relaxed), 2);
    let counts = tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counts.joins, 1);
}

#[tokio::test]
async fn resource_and_session_identity_and_scoring_changes_never_reuse_stale_output() {
    let fixture = Arc::new(Fixture::default());
    let mut worker = worker(&fixture);
    let mut notices = worker.subscribe();
    worker.select(&view(Some("a"))).unwrap();
    let a = ready(&worker, &mut notices).await;
    let mut b = view(Some("b")); // same echoed filename, distinct content identity/bytes
    worker.select(&b).unwrap();
    let changed = ready(&worker, &mut notices).await;
    assert_ne!(
        a.facts.timing_points[0].beat_length,
        changed.facts.timing_points[0].beat_length
    );
    assert_eq!(worker.counts().parses, 2);
    for field in 0..4 {
        let session = b.capture.session.as_mut().unwrap();
        match field {
            0 => session.session = "another-session".into(),
            1 => session.generation += 1,
            2 => session.owner_instance += 1,
            _ => session.variant = "lazer".into(),
        }
        worker.select(&b).unwrap();
        let result = ready(&worker, &mut notices).await;
        assert!(!Arc::ptr_eq(&changed, &result));
    }
    assert_eq!(worker.counts().opens, 6);
    assert_eq!(worker.counts().reads, 6);
    assert_eq!(worker.counts().parses, 3); // identical content/calculation reuses parser, scoring does not
    let counts = tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counts.joins, 1);
}

#[tokio::test]
async fn calculation_failure_clears_output_and_cannot_poison_the_success_cache() {
    let fixture = Arc::new(Fixture::default());
    let mut worker = worker(&fixture);
    let mut notices = worker.subscribe();
    let a_key = worker.select(&view(Some("a"))).unwrap().unwrap();
    ready(&worker, &mut notices).await;
    assert!(worker.current_for(&a_key).is_some());
    let mut invalid = view(Some("a"));
    invalid.play.mods.array = vec![vosu_osu_model::states::Mod {
        acronym: "DT".into(),
        settings: Some(indexmap::IndexMap::from([(
            "speed_change".into(),
            json!("not-a-number"),
        )])),
    }];
    worker.select(&invalid).unwrap();
    failed(&worker, &mut notices).await;
    assert!(worker.current_for(&a_key).is_none());
    assert!(worker.current().is_none());
    let defaults = crate::build(&invalid, None).unwrap();
    assert!(defaults.map_strains.is_empty());
    assert!(defaults.map_breaks.is_empty());
    assert!(defaults.map_timing_points.is_empty());
    worker.select(&view(Some("b"))).unwrap();
    ready(&worker, &mut notices).await;
    assert_eq!(worker.counts().opens, 3);
    assert_eq!(worker.counts().reads, 3);
    assert_eq!(fixture.closes.load(Ordering::Relaxed), 3);
    let counts = tokio::time::timeout(WAIT, worker.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counts.joins, 1);
}

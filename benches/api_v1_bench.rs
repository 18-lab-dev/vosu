#[path = "support/api_e2e.rs"]
mod api_e2e;
use criterion::{Criterion, criterion_group, criterion_main};
use std::{hint::black_box, time::Duration};
use vosu::server::{
    api_v1::{router::ApiV1Router, subscriptions::ApiV1SubscriptionHub},
    subscription_wire::Outbound,
};
use vosu_core::api::SessionTarget;
use vosu_plugin_api::PublishValueRef;
#[path = "support/api_v1.rs"]
mod support;
use support::{
    References, Telemetry,
    harness::{bounded, publish},
};

fn bench(c: &mut Criterion) {
    let id = support::ID;
    let mut f = Telemetry::new(id, 991001);
    let source = f.h.host.api_state_source();
    let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
    let router = ApiV1Router::new(source.clone(), hub.clone());
    let mut group = c.benchmark_group("api_v1");
    let mut n = 913i64;
    group.bench_function("idle_no_clients", |b| {
        b.iter(|| {
            n += 1;
            f.publish(id, black_box(n));
        })
    });
    group.bench_function("http_current_read", |b| {
        b.iter(|| {
            f.h.runtime.block_on(bounded(async {
                let request = axum::http::Request::builder()
                    .uri(format!("/api/v1/sessions/active/state?pointer=/{id}/score"))
                    .body(axum::body::Body::empty())
                    .unwrap();
                let response = router.handle(request).await;
                assert_eq!(response.status(), axum::http::StatusCode::OK);
                black_box(
                    axum::body::to_bytes(response.into_body(), 1024 * 1024)
                        .await
                        .unwrap(),
                );
            }))
        })
    });
    for (name, ns, path) in [
        ("subscribe_regular", id, "/score"),
        ("subscribe_precise", "vosu.osu", "/beatmap/time/live"),
        ("subscribe_slow_consumer", id, "/score"),
    ] {
        let handle = if ns == id {
            f.handle
        } else {
            f.h.publisher(ns, 991002).0
        };
        f.h.foreground
            .set(Some(if ns == id { 991001 } else { 991002 }));
        f.h.host.foreground_changed();
        publish(ns, handle, &[(path, PublishValueRef::I64(19))]);
        let (connection, mut receiver) = {
            let _enter = f.h.runtime.enter();
            hub.connect(SessionTarget::Active).unwrap()
        };
        connection.text(
            serde_json::to_string(&serde_json::json!([format!("/{ns}{path}")]))
                .unwrap()
                .as_bytes(),
        );
        f.h.runtime.block_on(bounded(receiver.recv())).unwrap();
        let slow = name == "subscribe_slow_consumer";
        if !slow {
            connection.write_completed();
        }
        group.bench_function(name, |b| {
            b.iter(|| {
                f.h.runtime.block_on(bounded(async {
                    n += 1;
                    if slow {
                        for offset in 0..16 {
                            publish(ns, handle, &[(path, PublishValueRef::I64(n * 16 + offset))]);
                        }
                        connection.write_completed();
                    } else {
                        publish(ns, handle, &[(path, PublishValueRef::I64(n))]);
                    }
                    let frame = receiver.recv().await.unwrap();
                    assert!(matches!(frame, Outbound::Text(_)));
                    black_box(frame);
                    if !slow {
                        connection.write_completed();
                    }
                }))
            })
        });
    }
    let (_second, _) = f.h.publisher("test.x1.second", 991003);
    f.h.foreground.set(Some(991001));
    f.h.host.foreground_changed();
    let (rebind, mut rebound) = {
        let _enter = f.h.runtime.enter();
        hub.connect(SessionTarget::Active).unwrap()
    };
    rebind.text(br#"["/vosu.client/game"]"#);
    f.h.runtime.block_on(bounded(rebound.recv())).unwrap();
    rebind.write_completed();
    let mut second = false;
    group.bench_function("active_rebind", |b| {
        b.iter(|| {
            f.h.runtime.block_on(bounded(async {
                second = !second;
                f.h.foreground
                    .set(Some(if second { 991003 } else { 991001 }));
                f.h.host.foreground_changed();
                assert!(matches!(rebound.recv().await.unwrap(), Outbound::Text(_)));
                rebind.write_completed();
            }))
        })
    });
    drop(rebind);
    let references = References::new();
    references.commit(false);
    group.bench_function("reference_publish_unchanged", |b| {
        b.iter(|| references.commit(false))
    });
    group.bench_function("reference_publish_changed", |b| {
        b.iter(|| {
            n += 1;
            references.commit(n % 2 == 0);
        })
    });
    let resource = support::resource_host("test.x1.resources", 991004);
    let urls = support::resource_urls(&resource, "test.x1.resources");
    let resources = std::sync::Arc::new(vosu::server::api_v1::resources::ResourceHttp::new(
        &resource.host,
        Duration::from_secs(5),
    ));
    let resource_hub = ApiV1SubscriptionHub::new(resource.host.api_state_source(), Duration::ZERO);
    let resource_router = ApiV1Router::new(resource.host.api_state_source(), resource_hub.clone())
        .with_resources(resources);
    for (name, kind) in [
        ("resource_cached_range", "file"),
        ("stream_range", "stream"),
    ] {
        let url = urls["resources"][kind]["url"].as_str().unwrap();
        let request = || {
            axum::http::Request::builder()
                .uri(url)
                .header("range", "bytes=2-7")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        resource
            .runtime
            .block_on(bounded(resource_router.handle(request())));
        group.bench_function(name, |b| {
            b.iter(|| {
                resource.runtime.block_on(bounded(async {
                    let response = resource_router.handle(request()).await;
                    assert_eq!(response.status(), axum::http::StatusCode::PARTIAL_CONTENT);
                    assert_eq!(
                        axum::body::to_bytes(response.into_body(), 1024)
                            .await
                            .unwrap()
                            .len(),
                        6
                    );
                }))
            })
        });
    }
    use vosu_core::actions::jobs::{JobProgress, JobRegistry, Principal};
    let jobs = JobRegistry::new();
    let job = jobs
        .create(Principal::Broker, "test.x1.work".into(), None)
        .unwrap();
    group.bench_function("job_progress_poll", |b| {
        b.iter(|| {
            f.h.runtime.block_on(bounded(async {
                let previous = jobs.get(&Principal::Broker, &job.id).unwrap();
                let waiter = jobs
                    .poll(
                        &job.id,
                        &Principal::Broker,
                        previous.revision,
                        Duration::from_secs(5),
                    )
                    .unwrap();
                assert!(matches!(
                    waiter,
                    vosu_core::actions::waiters::JobWait::Waiting(_)
                ));
                n += 1;
                jobs.progress(
                    &job.id,
                    JobProgress {
                        fraction: Some((n % 100) as f64 / 100.0),
                        message: Some("working-17".into()),
                    },
                )
                .unwrap();
                let current = waiter.wait().await;
                assert!(current.revision > previous.revision);
                black_box(serde_json::to_vec(&*current).unwrap());
            }))
        })
    });
    group.finish();
    hub.shutdown();
    resource_hub.shutdown();
    let worker = {
        let _enter = resource.runtime.enter();
        vosu_osu::resources::ResourceWorker::start()
    };
    let selection = vosu_osu::resources::Selection {
        roots: vosu_osu::resources::RootPaths {
            songs: Some(resource.root.path().to_owned()),
            skin: None,
        },
        chart: Some("fixture.osu".into()),
        ..Default::default()
    };
    c.benchmark_group("resource_identity_cold_io")
        .bench_function("stable_chart", |b| {
            b.iter(|| {
                resource.runtime.block_on(bounded(async {
                    let prepared = worker.prepare(selection.clone()).unwrap().await.unwrap();
                    assert_eq!(prepared.references().len(), 1);
                    black_box(prepared);
                }))
            })
        });
    resource.runtime.block_on(bounded(worker.shutdown()));
    // Release the precise-lane publisher before the production osu! fixture
    // registers its own distinct live instance under the same plugin id.
    drop(f);
    api_e2e::bench(c, "api_v1", "end_to_end");
}
criterion_group!(benches, bench);
criterion_main!(benches);

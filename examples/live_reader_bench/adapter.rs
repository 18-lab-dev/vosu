//! `--adapter`: production adapter output from a live client. Per lane
//! (regular → `/json/v2`, precise → `/json/v2/precise`) two passes run:
//!
//! - `adapter_stages`: tick-thread CPU attributed by stage — `read` (capture
//!   plan execution), `reduce` (state reducer), `projection` (osu! projection,
//!   diff and ABI batch encoding), `commit` (host bus commit, measured inside
//!   the synchronous publish call), `surface_view` (the bundled upstream
//!   adapter's coherent surface capture plus typed decode) and `encode` (actual
//!   adapter bytes). A tick whose captured selection equals the previous one
//!   is a cache hit: no decode or encode, as in the production provider.
//! - `adapter_frame`: read → reduce → publish on the tick thread, then the
//!   production `BusStateProvider` + `WebSocketHub` deliver the real frame;
//!   `read_to_frame_ns` is wall time from read start to the received frame.
//!   Delivery is acknowledgement-driven: it excludes the 1.x cadence wait
//!   (up to one poll interval) that an idle, already-acknowledged client sees.
use super::{
    bus::{Readers, thread_cpu_ns},
    *,
};
use bytes::Bytes;
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};
use vosu::server::{
    bus_provider::BusStateProvider,
    websocket::{Outbound, Schema, StateProvider, WebSocketHub},
};
use vosu_adapter_support::filtered::Filter;
use vosu_osu::{project::SessionFacts, publish::BusPublisher};
use vosu_osu_model::launch::InstanceSettings;
use vosu_osu_state::{Delivery, SnapshotPublisher, StateBundleSnapshot};
use vosu_plugin_api::{HostServices, Message, VtableHostServices, vosu_opcode, vosu_status};
use vosu_surface::{projection::Projection, state::SessionTarget};

/// Forwards every call; the host commits a publish batch synchronously inside
/// `send_flat`, so its thread CPU is the commit stage.
struct Timed {
    inner: VtableHostServices,
    commit_ns: Arc<AtomicU64>,
}
impl HostServices for Timed {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status {
        self.inner.send(message, correlation_id)
    }
    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation_id: u64,
        bytes: &[u8],
    ) -> vosu_status {
        let start = thread_cpu_ns().unwrap_or_default();
        let status = self.inner.send_flat(opcode, flags, correlation_id, bytes);
        let spent = thread_cpu_ns().unwrap_or(start).saturating_sub(start);
        self.commit_ns.fetch_add(spent as u64, Ordering::Relaxed);
        status
    }
}

const STAGES: [&str; 6] = [
    "read",
    "reduce",
    "projection",
    "commit",
    "surface_view",
    "encode",
];

struct Tick<'a, 'r, R> {
    args: &'a Args,
    readers: &'a mut Readers<'r, R>,
    bundle: &'a mut StateBundle,
    request: &'a mut RegularCaptureRequest,
    rt: &'a tokio::runtime::Runtime,
    sequence: u64,
    errors: usize,
}
impl<R: SynchronizedMemoryReader + 'static> Tick<'_, '_, R> {
    /// One production read and reduction; returns the publication and the
    /// read/reduce thread CPU.
    fn read(&mut self, precise: bool) -> Result<(Arc<StateBundleSnapshot>, Delivery, u128, u128)> {
        let start = thread_cpu_ns()?;
        if precise {
            let sync = *self
                .readers
                .synchronization
                .lock()
                .map_err(|_| fail("synchronization poisoned"))?;
            self.sequence += 1;
            let raw =
                self.readers
                    .precise
                    .capture_precise(&self.readers.context, self.sequence, sync)?;
            self.errors += usize::from(precise_failed(&raw));
            let read = thread_cpu_ns()?;
            let transition = self.rt.block_on(self.bundle.apply_precise(raw))?;
            if let Some(context) = transition.context {
                self.readers.context = context;
            }
            let reduced = thread_cpu_ns()?;
            Ok((
                transition.publication,
                Delivery::Precise,
                read - start,
                reduced - read,
            ))
        } else {
            let raw = self.readers.regular.capture_regular(self.request)?;
            self.errors += usize::from(regular_failed(&raw));
            let read = thread_cpu_ns()?;
            let transition = self.rt.block_on(self.bundle.apply_regular(raw))?;
            self.request.plan = transition.next_capture;
            self.sequence += 1;
            self.readers.context = transition.context;
            self.request.context = RegularCaptureContext::new(
                self.args.client,
                1,
                self.readers.context.reset_epoch,
                self.sequence,
            );
            let reduced = thread_cpu_ns()?;
            Ok((
                transition.publication,
                Delivery::All,
                read - start,
                reduced - read,
            ))
        }
    }
}

fn summary(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::json!({
        "median": values[values.len() / 2],
        "min": values[0],
        "max": values[values.len() - 1],
        "observations": values.len(),
    })
}

pub(super) fn measure_ticks<R: SynchronizedMemoryReader + 'static>(
    args: &Args,
    mut readers: Readers<'_, R>,
    bundle: &mut StateBundle,
    request: &mut RegularCaptureRequest,
    rt: &tokio::runtime::Runtime,
    attachment: &plan::Attachment,
) -> Result<serde_json::Value> {
    let (inner, handle) = attachment
        .publication
        .ok_or_else(|| fail("bus attachment missing"))?;
    let requester = attachment
        .adapter
        .clone()
        .ok_or_else(|| fail("adapter requester missing"))?;
    let commit_ns = Arc::new(AtomicU64::new(0));
    let publisher = BusPublisher::new(
        Timed {
            inner,
            commit_ns: Arc::clone(&commit_ns),
        },
        handle,
        Arc::new(AtomicBool::new(true)),
        SessionFacts {
            client: args.client,
            settings: InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            foreground: None,
        },
    );
    let mut tick = Tick {
        args,
        readers: &mut readers,
        bundle,
        request,
        rt,
        sequence: 4,
        errors: 0,
    };
    let mut modes = Vec::new();
    for precise in [false, true] {
        let lane = if precise { "precise" } else { "regular" };
        let (schema, route, adapter): (Schema, &str, Arc<dyn Projection<Filter, Bytes>>) =
            if precise {
                (
                    Schema::V2Precise,
                    "/json/v2/precise",
                    Arc::new(vosu_adapter_upstream::provider::Precise),
                )
            } else {
                (
                    Schema::V2,
                    "/json/v2",
                    Arc::new(vosu_adapter_upstream::provider::Regular),
                )
            };
        let selectors = adapter.selectors();

        // Pass 1: stage attribution on the tick thread.
        let mut cpu = Vec::with_capacity(9);
        let mut stages: [Vec<f64>; 6] = Default::default();
        let (mut changed, mut unchanged, mut bytes_total) = (0_usize, 0_usize, 0_usize);
        let mut last: Option<Arc<[vosu_plugin_api::StateEntry]>> = None;
        tick.errors = 0;
        for sample in 0..10 {
            let count = if sample == 0 {
                args.warmup
            } else {
                args.cycles
            };
            let mut sums = [0_u128; 6];
            for _ in 0..count {
                let (snapshot, delivery, read, reduce) = tick.read(precise)?;
                commit_ns.store(0, Ordering::Relaxed);
                let published = thread_cpu_ns()?;
                publisher.publish(snapshot, delivery)?;
                let committed = thread_cpu_ns()?;
                let commit = u128::from(commit_ns.load(Ordering::Relaxed));
                let capture = requester
                    .capture(SessionTarget::Focused, &selectors)
                    .map_err(|e| fail(e.to_string()))?;
                let mut viewed = thread_cpu_ns()?;
                let mut encoded = viewed;
                if last
                    .as_ref()
                    .is_some_and(|values| *values == capture.values)
                {
                    unchanged += 1;
                } else {
                    last = Some(Arc::clone(&capture.values));
                    let view = adapter.decode(capture).map_err(|e| fail(e.to_string()))?;
                    viewed = thread_cpu_ns()?;
                    let body = view.encode(&[]).map_err(fail)?;
                    encoded = thread_cpu_ns()?;
                    bytes_total += body.len();
                    black_box(body);
                    changed += 1;
                }
                let spent = [
                    read,
                    reduce,
                    (committed - published).saturating_sub(commit),
                    commit,
                    viewed - committed,
                    encoded - viewed,
                ];
                for (sum, value) in sums.iter_mut().zip(spent) {
                    *sum += value;
                }
            }
            if sample != 0 {
                let per_tick = |sum: u128| sum as f64 / args.cycles as f64;
                cpu.push(per_tick(sums.iter().sum()));
                for (stage, sum) in stages.iter_mut().zip(sums) {
                    stage.push(per_tick(sum));
                }
            }
        }
        let stages: serde_json::Map<_, _> = STAGES
            .iter()
            .zip(stages)
            .map(|(name, values)| ((*name).to_owned(), summary(values)))
            .collect();
        modes.push(serde_json::json!({
            "lane": lane, "mode": "adapter_stages", "route": route,
            "thread_cpu_ns_per_tick": summary(cpu),
            "stage_thread_cpu_ns_per_tick": stages,
            "changed_views": changed, "unchanged_views": unchanged,
            "mean_bytes": bytes_total.checked_div(changed).unwrap_or(0),
            "errors": tick.errors,
        }));

        // Pass 2: read → real WebSocket frame through the production provider/hub.
        let provider = Arc::new(BusStateProvider::new(
            schema,
            Arc::clone(&requester),
            Arc::clone(&adapter),
            Duration::from_secs(3600),
        ));
        let hub = WebSocketHub::new(schema, provider.clone());
        let run = attachment.runtime().spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        let (connection, mut receiver) = {
            let _enter = attachment.runtime().enter();
            hub.connect()
        };
        let frame = |receiver: &mut vosu::server::websocket::OutboundReceiver| match attachment
            .runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), receiver.recv()).await })
        {
            Ok(Some(Outbound::Text(bytes))) => Ok(bytes),
            other => Err(fail(format!("{route} frame not delivered: {other:?}"))),
        };
        frame(&mut receiver)?;
        let mut generation = provider.capture(schema).map(|s| s.generation());
        let mut cpu = Vec::with_capacity(9);
        let mut latencies = Vec::with_capacity(9 * args.cycles);
        let (mut frames, mut unchanged) = (0_usize, 0_usize);
        tick.errors = 0;
        for sample in 0..10 {
            let count = if sample == 0 {
                args.warmup
            } else {
                args.cycles
            };
            let mut sum = 0;
            for _ in 0..count {
                let read_start = Instant::now();
                let start = thread_cpu_ns()?;
                let (snapshot, delivery, _, _) = tick.read(precise)?;
                publisher.publish(snapshot, delivery)?;
                // The transport acknowledgement releases the in-flight slot; a
                // client already acknowledged after an unchanged tick is
                // delivered by the hub's wake instead of the next cadence.
                connection.write_completed();
                hub.wake();
                sum += thread_cpu_ns()? - start;
                let current = provider.capture(schema).map(|s| s.generation());
                if current == generation {
                    unchanged += 1;
                    continue;
                }
                generation = current;
                black_box(frame(&mut receiver)?);
                if sample != 0 {
                    latencies.push(read_start.elapsed().as_nanos() as f64);
                    frames += 1;
                }
            }
            if sample != 0 {
                cpu.push(sum as f64 / args.cycles as f64);
            }
        }
        hub.shutdown();
        drop((connection, receiver));
        attachment
            .runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), run).await })??;
        modes.push(serde_json::json!({
            "lane": lane, "mode": "adapter_frame", "route": route,
            "thread_cpu_ns_per_tick": summary(cpu),
            "read_to_frame_ns": summary(latencies),
            "frames": frames, "unchanged_ticks": unchanged, "errors": tick.errors,
        }));
    }
    Ok(serde_json::json!({
        "modes": modes, "samples": 9, "cycles_per_sample": args.cycles,
        "clock": "tick-thread CPU; read-to-frame wall time",
        "scope": "read → reduce → osu! projection → host bus commit → upstream adapter surface view → encoded bytes (stages) and production BusStateProvider/WebSocketHub frame (acknowledgement-driven, no cadence wait); excludes other daemon threads, timers and network",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The adapter mode's host composition delivers real `/json/v2` bytes and
    /// a real hub frame for a committed nonzero score, and attributes commit CPU.
    #[test]
    fn adapter_mode_emits_real_bytes_and_frame() {
        let attachment = plan::Attachment::with_adapter(
            plan::native_record(std::process::id()).unwrap(),
            "stable",
        )
        .unwrap();
        let (inner, handle) = attachment.publication.unwrap();
        let requester = attachment.adapter.clone().unwrap();
        let commit_ns = Arc::new(AtomicU64::new(0));
        let publisher = BusPublisher::new(
            Timed {
                inner,
                commit_ns: Arc::clone(&commit_ns),
            },
            handle,
            Arc::new(AtomicBool::new(true)),
            SessionFacts {
                client: ClientType::Stable,
                settings: InstanceSettings {
                    tournament_spectator: false,
                    tournament_ipc_id: None,
                    devserver_endpoint: None,
                },
                foreground: None,
            },
        );
        let mut snapshot = (*StateBundle::new(
            ClientType::Stable,
            1,
            Arc::new(LiveStateCalculationBackend::new()),
        )
        .snapshot())
        .clone();
        let regular = Arc::make_mut(&mut snapshot.regular);
        regular.global.status = 2;
        regular.gameplay.score = 24_680.0;
        publisher
            .publish(Arc::new(snapshot), Delivery::All)
            .unwrap();
        assert!(
            commit_ns.load(Ordering::Relaxed) > 0,
            "commit CPU attributed"
        );
        let adapter: Arc<dyn Projection<Filter, Bytes>> =
            Arc::new(vosu_adapter_upstream::provider::Regular);
        let capture = requester
            .capture(SessionTarget::Focused, &adapter.selectors())
            .unwrap();
        let body = adapter.decode(capture).unwrap().encode(&[]).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["play"]["score"], 24_680);
        let provider = Arc::new(BusStateProvider::new(
            Schema::V2,
            requester,
            adapter,
            Duration::from_secs(3600),
        ));
        let hub = WebSocketHub::new(Schema::V2, provider);
        let run = attachment.runtime().spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        let (connection, mut receiver) = {
            let _enter = attachment.runtime().enter();
            hub.connect()
        };
        let frame = attachment
            .runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), receiver.recv()).await })
            .unwrap();
        let Some(Outbound::Text(frame)) = frame else {
            panic!("expected a /json/v2 frame");
        };
        assert_eq!(frame, body, "the hub frame is the adapter's /json/v2 bytes");
        hub.shutdown();
        drop((connection, receiver));
        attachment
            .runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), run).await })
            .unwrap()
            .unwrap();
    }
}

//! Tick-thread CPU excludes scheduling delays; subscriber observation uses wall time.
use super::*;
use std::{sync::atomic::AtomicBool, time::Duration};
use vosu_osu::bridge_b1::{B1Event, B1Inputs, B1Publisher, B1SessionKey};
use vosu_osu::{project::SessionFacts, publish::BusPublisher};
use vosu_osu_model::launch::InstanceSettings;
use vosu_osu_state::{Delivery, SnapshotPublisher};

struct Latest {
    event: Mutex<Option<B1Event>>,
    inputs: tokio::sync::watch::Sender<B1Inputs>,
}
impl vosu_surface::LegacySink<B1Event, B1Inputs> for Latest {
    fn send(&self, event: B1Event) {
        *self.event.lock().unwrap() = Some(event);
    }
    fn inputs(&self) -> tokio::sync::watch::Receiver<B1Inputs> {
        self.inputs.subscribe()
    }
}

#[cfg(target_os = "linux")]
fn thread_cpu_ns() -> Result<u128> {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: time points to writable storage and the clock id is a valid thread clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, time.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: clock_gettime initialized the timespec on success.
    let time = unsafe { time.assume_init() };
    Ok(time.tv_sec as u128 * 1_000_000_000 + time.tv_nsec as u128)
}
#[cfg(windows)]
fn thread_cpu_ns() -> Result<u128> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentThread, GetThreadTimes},
    };
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    // SAFETY: the pseudo handle names this live thread and all four outputs are valid.
    let success = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Ok(u128::from(ticks(kernel) + ticks(user)) * 100)
}

pub(super) struct Readers<'a, R> {
    pub regular: &'a mut SourceRegularReader<R>,
    pub precise: &'a mut SourcePreciseReader<R>,
    pub synchronization: &'a Arc<Mutex<ReaderSynchronization>>,
    pub context: ReadContext,
}

pub(super) fn measure_ticks<R: SynchronizedMemoryReader + 'static>(
    args: &Args,
    mut readers: Readers<'_, R>,
    bundle: &mut StateBundle,
    request: &mut RegularCaptureRequest,
    rt: &tokio::runtime::Runtime,
    attachment: &plan::Attachment,
) -> Result<serde_json::Value> {
    let (services, handle) = attachment
        .publication
        .ok_or_else(|| fail("bus attachment missing"))?;
    let publisher = BusPublisher::new(
        services,
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
    let mut subscriber = attachment.subscribe()?;
    subscriber.updates.borrow_and_update();
    let mut sequence = 4;
    let mut modes = Vec::new();
    // Both modes include the same capture/reducer/legacy publication work.
    for precise_lane in [false, true] {
        for bus in [false, true] {
            let legacy = B1Publisher::new(
                B1SessionKey {
                    pid: args.pid,
                    generation: 1,
                },
                Arc::new(Latest {
                    event: Mutex::new(None),
                    inputs: tokio::sync::watch::channel(B1Inputs::default()).0,
                }),
            );
            let mut cpu = Vec::with_capacity(9);
            let mut latencies = Vec::with_capacity(9 * args.cycles);
            let mut unchanged = 0;
            let mut errors = 0;
            for sample in 0..10 {
                let count = if sample == 0 {
                    args.warmup
                } else {
                    args.cycles
                };
                let mut sum = 0;
                for _ in 0..count {
                    let start_cpu = thread_cpu_ns()?;
                    let read_start = Instant::now();
                    let snapshot = if precise_lane {
                        let sync = *readers
                            .synchronization
                            .lock()
                            .map_err(|_| fail("synchronization poisoned"))?;
                        sequence += 1;
                        let raw =
                            readers
                                .precise
                                .capture_precise(&readers.context, sequence, sync)?;
                        errors += usize::from(precise_failed(&raw));
                        let transition = rt.block_on(bundle.apply_precise(raw))?;
                        if let Some(context) = transition.context {
                            readers.context = context;
                        }
                        transition.publication
                    } else {
                        let raw = readers.regular.capture_regular(request)?;
                        errors += usize::from(regular_failed(&raw));
                        let transition = rt.block_on(bundle.apply_regular(raw))?;
                        request.plan = transition.next_capture;
                        sequence += 1;
                        readers.context = transition.context;
                        request.context = RegularCaptureContext::new(
                            args.client,
                            1,
                            readers.context.reset_epoch,
                            sequence,
                        );
                        transition.publication
                    };
                    let delivery = if precise_lane {
                        Delivery::Precise
                    } else {
                        Delivery::All
                    };
                    legacy.publish(snapshot.clone(), delivery)?;
                    if bus {
                        publisher.publish(snapshot, delivery)?;
                    }
                    let elapsed = thread_cpu_ns()? - start_cpu;
                    sum += elapsed;
                    if bus {
                        if subscriber.updates.has_changed()? {
                            rt.block_on(async {
                                tokio::time::timeout(
                                    Duration::from_secs(2),
                                    subscriber.updates.changed(),
                                )
                                .await
                            })??;
                            let observed = Instant::now();
                            black_box(subscriber.updates.borrow_and_update().revision);
                            if sample != 0 {
                                latencies
                                    .push(observed.duration_since(read_start).as_nanos() as f64);
                            }
                        } else {
                            unchanged += 1;
                        }
                    }
                }
                if sample != 0 {
                    cpu.push(sum as f64 / args.cycles as f64);
                }
            }
            cpu.sort_by(f64::total_cmp);
            latencies.sort_by(f64::total_cmp);
            modes.push(serde_json::json!({"lane":if precise_lane {"precise"} else {"regular"},"mode":if bus {"b1_plus_bus"} else {"b1_only"}, "thread_cpu_ns_per_tick": {"median":cpu[4], "min":cpu[0], "max":cpu[8]}, "read_to_surface_ns": if latencies.is_empty() { serde_json::Value::Null } else {serde_json::json!({"median":latencies[latencies.len()/2], "min":latencies[0], "max":latencies[latencies.len()-1], "observations":latencies.len()})}, "unchanged_ticks":unchanged,"errors":errors}));
        }
    }
    Ok(
        serde_json::json!({"modes":modes,"samples":9,"cycles_per_sample":args.cycles,"clock":"tick thread CPU; read-to-surface wall time","scope":"capture + reducer + production B1Publisher/latest-only sink + bus; excludes B1 manager background work, other daemon threads, timers and network"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_clock_is_monotonic() {
        let before = thread_cpu_ns().unwrap();
        for n in 0..10_000 {
            black_box(n);
        }
        assert!(thread_cpu_ns().unwrap() >= before);
    }

    #[test]
    fn subscriber_observes_real_host_commit() {
        let attachment =
            plan::Attachment::with_bus(plan::native_record(std::process::id()).unwrap()).unwrap();
        let mut subscriber = attachment.subscribe().unwrap();
        let before = subscriber.updates.borrow_and_update().revision;
        let (services, handle) = attachment.publication.unwrap();
        let publisher = BusPublisher::new(
            services,
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
        let bundle = StateBundle::new(
            ClientType::Stable,
            1,
            Arc::new(LiveStateCalculationBackend::new()),
        );
        let start = Instant::now();
        publisher.publish(bundle.snapshot(), Delivery::All).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), subscriber.updates.changed()).await
        })
        .unwrap()
        .unwrap();
        let observed = Instant::now();
        let update = subscriber.updates.borrow_and_update();
        assert!(update.revision > before);
        assert!(update.values[0].value.is_some());
        assert!(observed >= start);
    }
}

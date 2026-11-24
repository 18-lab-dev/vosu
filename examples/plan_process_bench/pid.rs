use super::{Result, open, plan};
use std::{hint::black_box, time::Instant};
use vosu_plugin_api::{
    PlanBase, PlanMemory, PlanOp, StageBase, StageRef, SyncReadBuffers,
    read_plan_profile::{self, Phase},
};
use vosu_sys::process::{ProcessMemory, ReadRequest};

fn native_capture(memory: &dyn ProcessMemory, address: usize, widths: &[usize]) -> Result<()> {
    for &width in widths {
        let mut destinations = [[0; 4]; 64];
        let mut requests = destinations
            .each_mut()
            .map(|dst| ReadRequest::new(address, dst));
        memory.read_many(&mut requests[..width])?;
        black_box(&destinations);
    }
    Ok(())
}

pub(super) fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let (mut pid, mut address, mut cycles) = (None, None, 1_000usize);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing argument value")?;
        match flag.as_str() {
            "--pid" => {
                pid = Some(if value == "self" {
                    std::process::id()
                } else {
                    value.parse::<u32>()?
                });
            }
            "--address" => {
                address = Some(usize::from_str_radix(value.trim_start_matches("0x"), 16)?)
            }
            "--cycles" => cycles = value.parse()?,
            _ => return Err(format!("unknown argument: {flag}").into()),
        }
    }
    if !(1..=100_000).contains(&cycles) {
        return Err("cycles must be 1..=100000".into());
    }
    let pid = pid.ok_or("--pid is required")?;
    let native = open(pid)?;
    let address = match address {
        Some(address) => address,
        None => {
            native
                .readable_regions()?
                .into_iter()
                .find(|region| region.size >= 4)
                .ok_or("target has no readable four-byte region")?
                .address
        }
    };
    native.read_exact(address, &mut [0; 4])?;
    let attachment = plan::Attachment::new(plan::native_record(pid)?, "vosu.bench.pid-phases")?;
    // Prime the same adapter used by live_reader_bench before profiling flat plans.
    black_box(attachment.process.read_u32(address)?);
    let (host, handle) = attachment.as_ref();
    let profiling = std::env::var_os("VOSU_READ_PLAN_PROFILE").is_some_and(|value| value == "1");
    let mut floor = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let start = Instant::now();
        floor.push(start.elapsed().as_nanos());
    }
    floor.sort_unstable();
    println!(
        "pid={pid} address={address:#x} profile={profiling} clock_pair_delta_ns={}",
        floor[500]
    );
    println!(
        "Synthetic widths use repeated readable addresses, not live osu decoding. Profiling clocks are reported separately and inflate timings."
    );
    let shapes: [(&str, &[usize]); 4] = [
        ("one_op", &[1]),
        ("stable_precise_2ops", &[2]),
        ("stable_wide_18ops", &[18]),
        // Brokered-read guide's modeled precise width/depth, without chasing
        // arbitrary pointers from a foreign process supplied only by PID.
        ("lazer_8stages_43ops", &[1, 1, 1, 1, 1, 18, 18, 2]),
    ];
    let memory = PlanMemory::new();
    let mut buffers = SyncReadBuffers::default();
    let mut sequence = 1u64;
    for (shape, widths) in shapes {
        let ops: Vec<Vec<PlanOp>> = widths
            .iter()
            .map(|&width| {
                vec![
                    PlanOp {
                        offset: 0,
                        length: 4
                    };
                    width
                ]
            })
            .collect();
        let stages: Vec<_> = ops
            .iter()
            .map(|ops| StageRef {
                base: StageBase::Root,
                base_offset: 0,
                ops,
            })
            .collect();
        for _ in 0..100 {
            native_capture(native.as_ref(), address, widths)?;
            memory
                .read_flat(
                    host,
                    sequence,
                    *handle,
                    PlanBase::Absolute(address as u64),
                    &stages,
                    &mut buffers,
                )
                .map_err(|error| format!("plan warmup: {error:?}"))?;
            sequence += 1;
        }
        read_plan_profile::take_thread_snapshot();
        let mut native_times = Vec::with_capacity(cycles);
        let mut plan_times = Vec::with_capacity(cycles);
        for iteration in 0..cycles {
            for is_plan in [iteration % 2 == 0, iteration % 2 != 0] {
                let start = Instant::now();
                if is_plan {
                    let result = memory
                        .read_flat(
                            host,
                            sequence,
                            *handle,
                            PlanBase::Absolute(address as u64),
                            &stages,
                            &mut buffers,
                        )
                        .map_err(|error| format!("plan capture: {error:?}"))?;
                    if result.len() != widths.iter().sum::<usize>() {
                        return Err("missing plan result".into());
                    }
                    for index in 0..result.len() {
                        let (kind, message) = result.get(index);
                        if kind != vosu_plugin_api::vosu_read_result_kind::VOSU_READ_OK {
                            return Err(format!("plan read failed: {kind:?}: {message:?}").into());
                        }
                    }
                    black_box(result);
                    plan_times.push(start.elapsed().as_nanos());
                    sequence += 1;
                } else {
                    native_capture(native.as_ref(), address, widths)?;
                    native_times.push(start.elapsed().as_nanos());
                }
            }
        }
        let snapshot = read_plan_profile::take_thread_snapshot();
        native_times.sort_unstable();
        plan_times.sort_unstable();
        println!(
            "{shape}: native_median_ns={} plan_median_ns={} plans={cycles}",
            native_times[cycles / 2],
            plan_times[cycles / 2]
        );
        if profiling {
            println!(
                "  plan_clock_reads_per_capture: precise={} coarse={} profiler={}",
                snapshot.precise_clock_reads / cycles as u64,
                snapshot.coarse_clock_reads / cycles as u64,
                snapshot.profiler_clock_reads / cycles as u64
            );
            for phase in Phase::ALL {
                let timing = snapshot.phases[phase as usize];
                println!(
                    "  {phase:?}: mean_ns_per_capture={} calls_per_capture={} backend_ops_per_capture={}",
                    timing.nanos / cycles as u128,
                    timing.calls / cycles as u64,
                    timing.ops / cycles as u64
                );
            }
        }
    }
    Ok(())
}

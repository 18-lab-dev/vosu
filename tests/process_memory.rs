use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use vosu::assets::DaemonAssets;
use vosu::memory::lazer_loader::{
    DEFAULT_ENDPOINTS, FetchError, FetchFuture, FetchResponse, LazerOffsetLoader, OffsetFetcher,
    REQUEST_TIMEOUT,
};
use vosu::memory::lazer_offsets::LazerOffsets;
use vosu::memory::{LazerMemory, LazerPattern, MemoryReader, StableMemory, StablePatterns};
use vosu::process::{
    MemoryError, MemoryRegion, Pattern, ProcessBitness, ProcessHandle, ProcessMemory, ReadRequest,
    ReadStatus,
};

#[derive(Default)]
struct SparseMemory {
    bytes: BTreeMap<usize, u8>,
    regions: Vec<MemoryRegion>,
}

impl SparseMemory {
    fn dense(base: usize, len: usize) -> Self {
        let bytes = (base..base + len).map(|address| (address, 0)).collect();
        Self {
            bytes,
            regions: vec![MemoryRegion {
                address: base,
                size: len,
            }],
        }
    }

    fn put(&mut self, address: usize, bytes: impl AsRef<[u8]>) {
        let bytes = bytes.as_ref();
        for (offset, byte) in bytes.iter().copied().enumerate() {
            self.bytes.insert(address + offset, byte);
        }
        if !bytes.is_empty() && self.regions.is_empty() {
            self.regions.push(MemoryRegion {
                address,
                size: bytes.len(),
            });
        }
    }

    fn u8(&mut self, address: usize, value: u8) {
        self.put(address, [value]);
    }
    fn i32(&mut self, address: usize, value: i32) {
        self.put(address, value.to_le_bytes());
    }
    fn u32(&mut self, address: usize, value: u32) {
        self.put(address, value.to_le_bytes());
    }
    fn i64(&mut self, address: usize, value: i64) {
        self.put(address, value.to_le_bytes());
    }
    fn u64(&mut self, address: usize, value: u64) {
        self.put(address, value.to_le_bytes());
    }
    fn ptr64(&mut self, address: usize, value: usize) {
        self.put(address, (value as u64).to_le_bytes());
    }
}

impl ProcessMemory for SparseMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let mut staged = Vec::with_capacity(dst.len());
        for offset in 0..dst.len() {
            staged.push(
                *self
                    .bytes
                    .get(&(address + offset))
                    .ok_or_else(|| MemoryError::Read {
                        address,
                        len: dst.len(),
                        message: "sparse fixture hole".into(),
                    })?,
            );
        }
        dst.copy_from_slice(&staged);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        let mut staged = Vec::with_capacity(reads.len());
        for read in reads.iter() {
            let mut bytes = vec![0; read.dst.len()];
            if let Err(error) = self.read_exact(read.address, &mut bytes) {
                for request in reads.iter_mut() {
                    request.dst.fill(0);
                }
                return Err(error);
            }
            staged.push(bytes);
        }
        for (request, bytes) in reads.iter_mut().zip(staged) {
            request.dst.copy_from_slice(&bytes);
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(self.regions.clone())
    }
}

fn handle(memory: SparseMemory, bitness: ProcessBitness) -> ProcessHandle {
    ProcessHandle::new(Arc::new(memory), bitness)
}

#[test]
fn lazer_scan_rejects_unmarked_size_pair_and_resolves_marked_container() {
    let offsets = Arc::new(LazerOffsets::embedded().unwrap());
    let mut memory = SparseMemory::dense(0x1000, 0x9000);
    // An unrelated size pair precedes the actual scaling container.
    memory.put(0x1800, [0, 0, 0x80, 0x44, 0, 0, 0x40, 0x44]);
    memory.put(0x4000, [1, 1, 0, 0, 0, 0, 0x80, 0x44, 0, 0, 0x40, 0x44]);
    memory.ptr64(0x4000 - 0x2c, 0x5000);
    memory.ptr64(
        0x5000
            + offsets
                .osu_game_online_chat_external_link_opener
                .api_k_backing_field,
        0x6000,
    );
    memory.ptr64(0x6000 + offsets.osu_game_online_api_apiaccess.game, 0x7000);
    memory.ptr64(0x7000, 0x8000);
    memory.i64(0x8000, offsets.effective_game_base_vtable() as i64);
    let process = handle(memory, ProcessBitness::X64);
    assert_eq!(
        process
            .scan_first(&LazerPattern::source_default().scaling_container_target_draw_size)
            .unwrap(),
        Some(0x4000),
    );
    let mut reader = LazerMemory::new(process, offsets, LazerPattern::source_default());
    assert!(reader.resolve_game_base().unwrap());
    assert_eq!(reader.game_base_address(), Some(0x7000));
}

fn stable_fixture_patterns() -> StablePatterns {
    StablePatterns {
        base_addr: 0x200,
        play_time_addr: 0x300,
        chat_checker_ptr: 0x400,
        skin_data_addr: 0x500,
        settings_class_addr: 0x600,
        configuration_addr: 0x700,
        bindings_addr: 0x800,
        rulesets_addr: 0x900,
        can_run_slowly_addr: 0xa00,
        status_ptr: 0xb00,
        menu_mods_ptr: 0xc00,
        get_audio_length_ptr: 0xd00,
        user_profile_ptr: 0xe00,
        raw_login_status_ptr: 0xf00,
        spectating_user_ptr: 0x1000,
        game_time_ptr: 0x1100,
    }
}

#[test]
fn sparse_builder_proves_atomic_failures_and_all_typed_layouts() {
    let mut x86 = SparseMemory::dense(0x100, 0x5000);
    x86.u32(0x100, 0xfedc_ba98);
    x86.u32(0x104, 0x1234);
    let h86 = handle(x86, ProcessBitness::X86);
    assert_eq!(h86.read_pointer(0x100).unwrap(), 0xfedc_ba98);
    assert_eq!(h86.read_pointer_pointer(0x104).unwrap(), 0);

    let mut memory = SparseMemory::dense(0x1000, 0xa000);
    memory.u64(0x1000, 0x1234_5678_9abc_def0);
    memory.u8(0x1008, 1);
    memory.ptr64(0x1100, 0x1200);
    memory.i32(0x1208, 2);
    memory.put(0x120c, (b'O' as u16).to_le_bytes());
    memory.put(0x120e, (b'K' as u16).to_le_bytes());
    memory.i32(0x1308, 3);
    for (i, value) in [11_i32, 22, 33].into_iter().enumerate() {
        memory.i32(0x1310 + 4 * i, value);
    }
    memory.ptr64(0x1408, 0x1500);
    memory.i32(0x1410, 2);
    memory.i32(0x1508, 3);
    for (i, value) in [44_i32, 55, 66].into_iter().enumerate() {
        memory.i32(0x1510 + 4 * i, value);
    }
    memory.ptr64(0x1610, 0x1700);
    memory.i32(0x1708, 2);
    memory.u64(0x1710, 0x1800);
    memory.i32(0x1718, 1);
    memory.i32(0x1720, 7);
    memory.u64(0x1728, 0x1900);
    memory.i32(0x1730, 1);
    memory.i32(0x1738, 9);
    memory.u8(0x1a00, 1);
    memory.i32(0x1a04, -77);
    memory.u64(0x1b00, 621_355_968_000_000_000);
    memory.put(
        0x1c00,
        [
            0x00, 0x00, 0x02, 0x80, 0, 0, 0, 0, 0x39, 0x30, 0, 0, 0, 0, 0, 0,
        ],
    );
    let h = handle(memory, ProcessBitness::X64);
    assert_eq!(h.read_u64(0x1000).unwrap(), 0x1234_5678_9abc_def0);
    assert!(h.read_bool(0x1008).unwrap());
    assert_eq!(
        h.read_managed_string_pointer(0x1100).unwrap(),
        ReadStatus::Ready("OK".into())
    );
    assert_eq!(
        h.read_managed_array(0x1300, 4, ProcessHandle::read_i32)
            .unwrap(),
        [11, 22, 33]
    );
    assert_eq!(
        h.read_managed_list(0x1400, 4, ProcessHandle::read_i32)
            .unwrap(),
        [44, 55]
    );
    let dictionary = h.read_managed_dictionary_i32_to_ref(0x1600).unwrap();
    assert_eq!(
        dictionary
            .iter()
            .map(|entry| (entry.key, entry.value))
            .collect::<Vec<_>>(),
        [(7, 0x1800), (9, 0x1900)]
    );
    assert_eq!(h.read_nullable_i32(0x1a00).unwrap(), Some(-77));
    assert_eq!(
        h.read_dotnet_datetime(0x1b00).unwrap().timestamp_millis(),
        0
    );
    assert_eq!(h.read_dotnet_decimal(0x1c00).unwrap(), -123.45);

    let mut hole = SparseMemory::default();
    hole.put(0x2000, [1, 2, 3]);
    let hole = handle(hole, ProcessBitness::X64);
    let mut first = [9; 2];
    let mut second = [9; 2];
    assert!(
        hole.read_many(&mut [
            ReadRequest::new(0x2000, &mut first),
            ReadRequest::new(0x3000, &mut second)
        ])
        .is_err()
    );
    assert_eq!((first, second), ([0; 2], [0; 2]));
}

#[test]
fn managed_string_boundaries_distinguish_ready_not_ready_and_corruption() {
    let mut memory = SparseMemory::dense(0x1000, 0x9000);
    memory.i32(0x1008, 0);
    memory.i32(0x1108, -1);
    memory.i32(0x1208, 4096);
    memory.i32(0x1308, 4095);
    for index in 0..4095 {
        memory.put(0x130c + index * 2, (b'x' as u16).to_le_bytes());
    }
    let handle = handle(memory, ProcessBitness::X64);
    assert_eq!(handle.read_managed_string(0).unwrap(), ReadStatus::NotReady);
    assert_eq!(
        handle.read_managed_string(0x1000).unwrap(),
        ReadStatus::Ready(String::new())
    );
    assert!(matches!(
        handle.read_managed_string(0x1100),
        Err(MemoryError::Corrupt { .. })
    ));
    assert!(matches!(
        handle.read_managed_string(0x1200),
        Err(MemoryError::Corrupt { .. })
    ));
    assert!(
        matches!(handle.read_managed_string(0x1300).unwrap(), ReadStatus::Ready(value) if value.len() == 4095)
    );
}

#[test]
fn committed_fixture_resolves_every_stable_scan_pattern_and_public_sentinels() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/raw_memory/stable_patterns.json")).unwrap();
    let base = fixture["base_address"].as_u64().unwrap() as usize;
    let gap = fixture["gap_bytes"].as_u64().unwrap() as usize;
    let wildcard = fixture["wildcard_byte"].as_u64().unwrap() as u8;
    let mut image = Vec::new();
    let mut expected = Vec::new();
    for item in fixture["patterns"].as_array().unwrap() {
        let pattern = Pattern::parse(item["signature"].as_str().unwrap(), false).unwrap();
        let start = base + image.len();
        let offset = item["offset"].as_i64().unwrap();
        expected.push(if offset >= 0 {
            start + offset as usize
        } else {
            start - offset.unsigned_abs() as usize
        });
        image.extend(
            pattern
                .bytes()
                .iter()
                .zip(pattern.mask())
                .map(|(byte, mask)| if *mask == 0 { wildcard } else { *byte }),
        );
        image.extend(std::iter::repeat_n(0, gap));
    }
    let mut memory = SparseMemory::default();
    memory.put(base, image.as_slice());
    memory.regions = vec![MemoryRegion {
        address: base,
        size: image.len(),
    }];
    let resolved = match StablePatterns::resolve(&handle(memory, ProcessBitness::X86)).unwrap() {
        ReadStatus::Ready(value) => value,
        other => panic!("stable patterns unresolved: {other:?}"),
    };
    assert_eq!(
        [
            resolved.base_addr,
            resolved.play_time_addr,
            resolved.chat_checker_ptr,
            resolved.skin_data_addr,
            resolved.settings_class_addr,
            resolved.configuration_addr,
            resolved.bindings_addr,
            resolved.rulesets_addr,
            resolved.can_run_slowly_addr,
            resolved.status_ptr,
            resolved.menu_mods_ptr,
            resolved.get_audio_length_ptr,
            resolved.user_profile_ptr,
            resolved.raw_login_status_ptr,
            resolved.spectating_user_ptr,
            resolved.game_time_ptr,
        ],
        expected.as_slice()
    );

    let mut reader = StableMemory::new(
        handle(SparseMemory::dense(0, 0x9000), ProcessBitness::X86),
        stable_fixture_patterns(),
    );
    assert!(matches!(
        reader.audio_velocity_base().unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(reader.user().unwrap(), ReadStatus::NotReady));
    assert!(matches!(
        reader.result_screen().unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.gameplay(0.0, None).unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.key_overlay(0).unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.hit_errors(0).unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(reader.global().unwrap(), ReadStatus::Ready(_)));
    assert!(matches!(
        reader.global_precise().unwrap(),
        ReadStatus::Ready(_)
    ));
    assert!(matches!(reader.menu("").unwrap(), ReadStatus::NotReady));
    assert!(matches!(reader.tourney().unwrap(), ReadStatus::NotReady));
    assert!(matches!(
        reader.tourney_chat(&[]).unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.tourney_user().unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.mp3_length().unwrap(),
        ReadStatus::Ready(0.0)
    ));
    assert!(
        matches!(reader.leaderboard(0).unwrap(), ReadStatus::Ready((false, None, value)) if value.is_empty())
    );
    assert!(matches!(
        reader.ranked_play().unwrap(),
        ReadStatus::Unsupported("Stable: ranked play not implemented")
    ));
    assert!(matches!(
        reader.spectating_data(false).unwrap(),
        ReadStatus::Unsupported("Stable: lazer spectating not implemented")
    ));
    assert!(matches!(reader.settings().unwrap(), ReadStatus::NotReady));
}

fn lazer_reader(memory: SparseMemory) -> (LazerMemory, Arc<LazerOffsets>) {
    let offsets = Arc::new(LazerOffsets::embedded().unwrap());
    let reader = LazerMemory::new(
        handle(memory, ProcessBitness::X64),
        offsets.clone(),
        LazerPattern {
            scaling_container_target_draw_size: Pattern::parse("AA", false).unwrap(),
            offset: 0,
        },
    );
    (reader, offsets)
}

fn valid_lazer_base(memory: &mut SparseMemory, base: usize, offsets: &LazerOffsets) {
    let vtable = base + 0x800;
    memory.ptr64(base, vtable);
    memory.i64(vtable, offsets.effective_game_base_vtable() as i64);
}

#[test]
fn lazer_public_screens_spectating_ranked_bounds_and_unsupported_tourney_are_observable() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x10000;
    let player = 0x20000;
    let logo = 0x26000;
    let visualizer = 0x27000;
    let amplitudes = 0x28000;
    let mut memory = SparseMemory::dense(0x10000, 0x30000);
    valid_lazer_base(&mut memory, base, &offsets);
    memory.ptr64(base + offsets.osu_game_osu_game.osu_logo, logo);
    memory.ptr64(
        logo + offsets.osu_game_screens_menu_osu_logo.visualizer,
        visualizer,
    );
    memory.ptr64(
        visualizer
            + offsets
                .osu_game_screens_menu_logo_visualisation
                .frequency_amplitudes,
        amplitudes,
    );
    for index in 0..40 {
        memory.put(amplitudes + 0x10 + index * 4, (index as f32).to_le_bytes());
    }
    memory.ptr64(
        base + offsets.osu_game_osu_game_base.api_k_backing_field,
        0x29000,
    );
    memory.ptr64(
        base + offsets
            .osu_game_osu_game_base
            .spectator_client_k_backing_field,
        0x29100,
    );
    memory.ptr64(
        player
            + offsets
                .osu_game_screens_play_submitting_player
                .api_k_backing_field,
        0x29000,
    );
    memory.ptr64(
        player
            + offsets
                .osu_game_screens_play_submitting_player
                .spectator_client_k_backing_field,
        0x29100,
    );
    let (mut reader, _) = lazer_reader(memory);
    reader.set_resolved_state(base, player);
    assert!(
        matches!(reader.audio_velocity_base().unwrap(), ReadStatus::Ready(values) if values.len() == 40 && values[39] == 39.0)
    );
    assert!(reader.gameplay(0.0, None).is_err());
    assert!(matches!(
        reader.tourney().unwrap(),
        ReadStatus::Unsupported("Lazer: tourney not implemented")
    ));
    assert!(matches!(
        reader.tourney_chat(&[]).unwrap(),
        ReadStatus::Unsupported(_)
    ));
    assert!(matches!(
        reader.tourney_user().unwrap(),
        ReadStatus::Unsupported(_)
    ));

    let client = 0x12000;
    let connected = 0x12100;
    let room = 0x13000;
    let state = 0x14000;
    let mut memory = SparseMemory::dense(0x10000, 0x10000);
    valid_lazer_base(&mut memory, base, &offsets);
    memory.ptr64(
        base + offsets
            .osu_game_osu_game_base
            .multiplayer_client_k_backing_field,
        client,
    );
    memory.ptr64(
        client
            + offsets
                .osu_game_online_multiplayer_online_multiplayer_client
                .is_connected_k_backing_field,
        connected,
    );
    memory.u8(connected + 0x40, 1);
    memory.ptr64(
        client + offsets.osu_game_online_multiplayer_multiplayer_client.room,
        room,
    );
    memory.ptr64(
        room + offsets
            .osu_game_online_multiplayer_multiplayer_room
            .match_state_k_backing_field,
        state,
    );
    let ranked =
        &offsets.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state;
    memory.i32(state + ranked.stage_k_backing_field, 7);
    memory.i32(state + ranked.current_round_k_backing_field, 0);
    let (mut reader, _) = lazer_reader(memory);
    reader.set_resolved_state(base, 0);
    assert_eq!(reader.ranked_play().unwrap(), ReadStatus::NotReady);
}

#[derive(Clone, Default)]
struct FakeFetcher {
    responses: Arc<Mutex<VecDeque<Result<FetchResponse, FetchError>>>>,
    calls: Arc<Mutex<Vec<(String, String, Duration)>>>,
}

impl OffsetFetcher for FakeFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        user_agent: &'a str,
        timeout: Duration,
    ) -> FetchFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .await
                .push((url.into(), user_agent.into(), timeout));
            self.responses.lock().await.pop_front().unwrap_or_else(|| {
                Err(FetchError {
                    message: "offline fixture".into(),
                })
            })
        })
    }
}

fn embedded_offsets() -> Bytes {
    Bytes::copy_from_slice(&DaemonAssets::get("offsets.json").unwrap().data)
}

fn offsets_for(version: &str) -> Bytes {
    let mut value: Value = serde_json::from_slice(&embedded_offsets()).unwrap();
    value["OsuVersion"] = Value::String(version.into());
    Bytes::from(serde_json::to_vec(&value).unwrap())
}

#[tokio::test]
async fn lazer_offsets_cover_embedded_current_cache_and_remote_fallback_without_network() {
    let source_fixture: Value = serde_json::from_str(include_str!(
        "fixtures/raw_memory/lazer_offset_sources.json"
    ))
    .unwrap();
    let embedded = embedded_offsets();
    let embedded_version = LazerOffsets::from_json(&embedded).unwrap().osu_version;
    let cache = tempfile::tempdir().unwrap();
    let fetcher = FakeFetcher::default();
    let loader = LazerOffsetLoader::new(fetcher.clone(), cache.path().into(), embedded.clone());
    assert_eq!(
        loader
            .resolve(&embedded_version, &CancellationToken::new())
            .await
            .unwrap()
            .osu_version,
        embedded_version
    );
    assert!(fetcher.calls.lock().await.is_empty());

    let cached_version = source_fixture["cache_version"].as_str().unwrap();
    std::fs::write(
        cache.path().join(format!("{cached_version}.json")),
        offsets_for(cached_version),
    )
    .unwrap();
    assert_eq!(
        loader
            .resolve(cached_version, &CancellationToken::new())
            .await
            .unwrap()
            .osu_version,
        cached_version
    );
    assert!(fetcher.calls.lock().await.is_empty());

    let remote_version = source_fixture["remote_version"].as_str().unwrap();
    let responses = source_fixture["remote_responses"].as_array().unwrap();
    fetcher.responses.lock().await.extend([
        Ok(FetchResponse {
            status: responses[0]["status"].as_u64().unwrap() as u16,
            body: Bytes::new(),
        }),
        Ok(FetchResponse {
            status: responses[1]["status"].as_u64().unwrap() as u16,
            body: offsets_for(remote_version),
        }),
    ]);
    let remote = loader
        .resolve(remote_version, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(remote.osu_version, remote_version);
    let calls = fetcher.calls.lock().await;
    assert_eq!(calls.len(), 2);
    // The fixture retains the upstream endpoints as parity evidence; live requests use vosu.
    assert!(calls[0].0.starts_with(DEFAULT_ENDPOINTS[0]));
    assert!(calls[1].0.starts_with(DEFAULT_ENDPOINTS[1]));
    assert_eq!(calls[0].2, REQUEST_TIMEOUT);
    drop(calls);
    assert_eq!(
        std::fs::read(loader.cache_path(remote_version).unwrap()).unwrap(),
        offsets_for(remote_version)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_native_backend_opens_and_reads_this_process() {
    use vosu::process::{open_process, process_bitness};
    let marker = Box::new(0x9a7b_6c5d_4e3f_2011_u64);
    let address = (&*marker as *const u64) as usize;
    let pid = std::process::id();
    let backend = open_process(pid).unwrap();
    let handle = ProcessHandle::new(Arc::new(backend), process_bitness(pid).unwrap());
    assert_eq!(handle.process_id(), Some(pid));
    assert!(handle.is_alive());
    assert_eq!(handle.read_u64(address).unwrap(), *marker);
}

#[cfg(windows)]
#[test]
fn windows_native_backend_opens_and_reads_this_process() {
    use vosu::process::open_process;
    let marker = Box::new(0x9a7b_6c5d_4e3f_2011_u64);
    let address = (&*marker as *const u64) as usize;
    let pid = std::process::id();
    let handle = open_process(pid).unwrap();
    assert_eq!(handle.process_id(), Some(pid));
    assert!(handle.is_alive());
    assert_eq!(handle.read_u64(address).unwrap(), *marker);
}

//! Shared successful x86 capture: identical bytes for native and brokered benches.
use vosu_osu_model::memory::{Gameplay, Global, Menu};
use vosu_osu_reader::memory::{MemoryReader, StableMemory, StablePatterns};
use vosu_plugin_api::memory::ReadStatus;

pub const PLAY_TIME: f64 = 1234.0;
pub const FIRST_OBJECT_TIME: Option<i64> = Some(0);

pub fn patterns() -> StablePatterns {
    StablePatterns {
        base_addr: 0x200,
        play_time_addr: 0x500,
        chat_checker_ptr: 0x380,
        skin_data_addr: 0x600,
        settings_class_addr: 0x580,
        configuration_addr: 0x780,
        bindings_addr: 0x800,
        rulesets_addr: 0x700,
        can_run_slowly_addr: 0x400,
        status_ptr: 0x300,
        menu_mods_ptr: 0x340,
        get_audio_length_ptr: 0x880,
        user_profile_ptr: 0x900,
        raw_login_status_ptr: 0x980,
        spectating_user_ptr: 0xa00,
        game_time_ptr: 0x480,
    }
}

struct Graph {
    bytes: Vec<u8>,
    next: usize,
}
impl Graph {
    fn node(&mut self) -> usize {
        let address = self.next;
        self.next += 0x200;
        address
    }
    fn put(&mut self, address: usize, value: &[u8]) {
        self.bytes[address..address + value.len()].copy_from_slice(value);
    }
    fn word(&mut self, address: usize, value: usize) {
        self.put(address, &u32::try_from(value).unwrap().to_le_bytes());
    }
    fn child(&mut self, address: usize) -> usize {
        let node = self.node();
        self.word(address, node);
        node
    }
    fn value(&mut self, address: usize, value: usize) {
        let node = self.child(address);
        self.word(node, value);
    }
    fn string(&mut self, address: usize, text: &str) {
        let node = self.child(address);
        let units: Vec<_> = text.encode_utf16().collect();
        self.word(node + 4, units.len());
        for (index, unit) in units.iter().enumerate() {
            self.put(node + 8 + index * 2, &unit.to_le_bytes());
        }
    }
    fn f64(&mut self, address: usize, value: f64) {
        self.put(address, &value.to_le_bytes());
    }
}

pub fn bytes() -> Vec<u8> {
    let p = patterns();
    let mut g = Graph {
        bytes: vec![0; 0x10000],
        next: 0x2000,
    };
    g.value(p.menu_mods_ptr, 72);
    g.value(p.chat_checker_ptr, 2);
    g.value(p.can_run_slowly_addr + 0x46, 1);
    g.value(p.game_time_ptr, 999);
    g.value(p.status_ptr, 3);
    g.value(p.play_time_addr + 5, 1234);
    let settings = g.child(p.settings_class_addr + 8);
    let songs = g.child(settings + 0xb8);
    g.string(songs + 4, "C:\\osu!\\Songs");
    let show = g.child(settings + 4);
    g.word(show + 0xc, 1);
    let skin = g.child(p.skin_data_addr + 7);
    let skin_object = g.child(skin);
    g.string(skin_object + 0x44, "Benchmark skin");
    let container = g.child(p.rulesets_addr - 0xb);
    let ruleset = g.child(container + 4);
    g.word(ruleset + 0x1d8, 1);
    g.word(ruleset + 0xf8, 7654321);
    let beat = g.node();
    g.value(p.base_addr - 0xc, beat);
    for (offset, text) in [
        (0x6c, "0123456789abcdef0123456789abcdef"),
        (0x90, "Artist - Title (Mapper) [Expert].osu"),
        (0x64, "audio.mp3"),
        (0x68, "background.jpg"),
        (0x18, "Artist"),
        (0x1c, "Artist original"),
        (0x24, "Title"),
        (0x28, "Title original"),
        (0x78, "Benchmark folder"),
        (0x7c, "Mapper"),
        (0xac, "Expert"),
    ] {
        g.string(beat + offset, text);
    }
    for (offset, value) in [(0x2c, 9.5f32), (0x30, 4.0), (0x34, 5.0), (0x38, 8.25)] {
        g.put(beat + offset, &value.to_le_bytes());
    }
    for (offset, value) in [(0xc8, 123), (0xcc, 456), (0xf8, 500), (0x12c, 4)] {
        g.word(beat + offset, value);
    }
    let mode = g.child(p.base_addr - 0x33);
    g.word(mode, 0);
    g.word(mode + 8, 2);
    g.word(mode + 0xc, 77);
    let game = g.child(ruleset + 0x64);
    let score = g.child(game + 0x38);
    let mods = g.child(score + 0x1c);
    g.word(mods + 8, 0x5a5a);
    g.word(mods + 0xc, 0x5a5a ^ 72);
    g.string(score + 0x28, "Benchmark player");
    g.word(score + 0x64, 0);
    g.word(score + 0x78, 123456);
    g.put(score + 0x68, &321i16.to_le_bytes());
    for index in 0..7 {
        g.put(
            score + 0x88 + index * 2,
            &(100i16 + index as i16).to_le_bytes(),
        );
    }
    let hp = g.child(game + 0x40);
    g.f64(hp + 0x14, 95.0);
    g.f64(hp + 0x1c, 90.0);
    let accuracy = g.child(game + 0x48);
    g.f64(accuracy + 0xc, 98.75);
    g.bytes
}

pub fn capture(reader: &mut StableMemory) -> (Global, Menu, Gameplay) {
    let ReadStatus::Ready(global) = reader.global().unwrap() else {
        panic!("global must be ready")
    };
    let ReadStatus::Ready(menu) = reader.menu("").unwrap() else {
        panic!("menu must be ready")
    };
    let ReadStatus::Ready(gameplay) = reader.gameplay(PLAY_TIME, FIRST_OBJECT_TIME).unwrap() else {
        panic!("gameplay must be ready")
    };
    (global, menu, gameplay)
}

pub fn assert_capture(reader: &mut StableMemory) {
    let (global, menu, gameplay) = capture(reader);
    assert_eq!(global.status, 3);
    assert_eq!(global.game_time, 999);
    assert!(global.is_watching_replay && global.show_interface && global.is_replay_ui_hidden);
    assert_eq!(global.skin_folder, "Benchmark skin");
    assert_eq!(global.memory_songs_folder, "C:\\osu!\\Songs");
    let Menu::Full(menu) = menu else {
        panic!("menu must decode its full populated graph")
    };
    assert_eq!(menu.map_id, 123);
    assert_eq!(menu.artist, "Artist");
    assert_eq!(menu.plays, 77);
    assert_eq!(gameplay.score, 123456.0);
    assert_eq!(gameplay.player_name, "Benchmark player");
    assert_eq!(gameplay.player_hp, 90.0);
    assert_eq!(gameplay.accuracy, 98.75);
    assert_eq!(gameplay.max_combo, 321);
}

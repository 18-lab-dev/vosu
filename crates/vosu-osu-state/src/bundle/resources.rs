use super::*;
use vosu_osu_model::memory::{BeatmapResources, CurrentSkin};

/// An immutable, internal input to cold resource work. Construct on selection/root/skin
/// changes, not on capture ticks. Paths are facts, not filesystem capabilities.
#[derive(Debug, Clone, PartialEq)]
pub struct ColdResourceFacts {
    pub source: ClientType,
    pub instance_generation: u64,
    pub reset_epoch: u64,
    pub selection_generation: u64,
    pub game_folder: String,
    pub songs_folder: String,
    pub memory_songs_folder: String,
    pub skin_folder: String,
    pub skin: Option<Arc<CurrentSkin>>,
    pub checksum: Option<String>,
    pub folder: Option<String>,
    pub filename: Option<String>,
    pub audio_filename: Option<String>,
    pub background_filename: Option<String>,
    pub set: Option<Arc<BeatmapResources>>,
}

impl StateBundleSnapshot {
    /// Takes a cold copy of scalar facts and retains the shared set/skin indexes.
    /// No memory or filesystem reads occur here.
    pub fn cold_resource_facts(&self) -> ColdResourceFacts {
        let global = &self.regular.global;
        let menu = &self.regular.menu;
        ColdResourceFacts {
            source: self.source,
            instance_generation: self.generation,
            reset_epoch: self.reset_epoch,
            selection_generation: menu.resource_generation,
            game_folder: global.game_folder.clone(),
            songs_folder: global.songs_folder.clone(),
            memory_songs_folder: global.memory_songs_folder.clone(),
            skin_folder: global.skin_folder.clone(),
            skin: global.current_skin.clone(),
            checksum: menu.checksum.clone(),
            folder: menu.folder.clone(),
            filename: menu.filename.clone(),
            audio_filename: menu.audio_filename.clone(),
            background_filename: menu.background_filename.clone(),
            set: menu.resources.clone(),
        }
    }
}

impl ColdResourceFacts {
    /// Check immediately before accepting asynchronous work. An A→B→A switch, reset,
    /// instance replacement, or root/skin change invalidates the earlier result.
    /// This check borrows scalars and compares shared skin identity without allocation.
    pub fn is_current(&self, snapshot: &StateBundleSnapshot) -> bool {
        let global = &snapshot.regular.global;
        let menu = &snapshot.regular.menu;
        self.source == snapshot.source
            && self.instance_generation == snapshot.generation
            && self.reset_epoch == snapshot.reset_epoch
            && self.selection_generation == snapshot.regular.menu.resource_generation
            && self.game_folder == global.game_folder
            && self.checksum == menu.checksum
            && self.folder == menu.folder
            && self.filename == menu.filename
            && self.audio_filename == menu.audio_filename
            && self.background_filename == menu.background_filename
            && self.songs_folder == global.songs_folder
            && self.memory_songs_folder == global.memory_songs_folder
            && self.skin_folder == global.skin_folder
            && match (&self.skin, &global.current_skin) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
    }
}

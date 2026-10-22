use super::*;
use crate::model::memory::CurrentSkin;

// Skin's first resource store is RealmBackedResourceStore<SkinInfo>. Its lazy
// dictionary is populated by Skin's constructor when it requests skin.ini.
// Source: ppy/osu aaa75b7c0a9f21d558ce84cd0ffac6c739bad8e7,
// Skin.cs and Skinning/RealmBackedResourceStore.cs.
impl LazerMemory {
    pub(super) fn read_current_skin(&mut self) -> Result<Arc<CurrentSkin>, MemoryError> {
        let manager = self.root(GameRoot::SkinManager)?;
        let current = self.ptr(add(
            manager,
            self.offsets.osu_game_skinning_skin_manager.current_skin,
        )?)?;
        let skin = self.ptr(add(current, 0x20)?)?;
        if skin == 0 {
            return Err(corrupt("current skin", skin, "null skin"));
        }
        let offsets = &self.offsets.osu_game_skinning_skin;
        let store_offset = self
            .offsets
            .skin_resource_store
            .as_ref()
            .map(|o| o.stores)
            .unwrap_or(0);
        let mapping_offset = self
            .offsets
            .skin_realm_store
            .as_ref()
            .map(|o| o.mapping)
            .unwrap_or(0);
        let id_offset = self.offsets.skin_live.as_ref().map(|o| o.id).unwrap_or(0);
        if [
            offsets.skin_info,
            offsets.store,
            store_offset,
            mapping_offset,
            id_offset,
        ]
        .contains(&0)
        {
            return Err(corrupt("current skin offsets", 0, "unmeasured skin fields"));
        }
        let live = self.ptr(add(skin, offsets.skin_info)?)?;
        let mut id = [0u8; 16];
        self.process.read_exact(add(live, id_offset)?, &mut id)?;
        let builtin = builtin_identity(id);
        let lazy = if builtin.is_none() {
            let store = self.ptr(add(skin, offsets.store)?)?;
            let stores = self.ptr(add(store, store_offset)?)?;
            let (count, items) = self.list_info(stores)?;
            if count == 0 || count > 16 {
                return Err(corrupt(
                    "skin resource stores",
                    stores,
                    "invalid store count",
                ));
            }
            let realm = self.ptr(items)?;
            self.ptr(add(realm, mapping_offset)?)?
        } else {
            0
        };
        if let Some((cached_skin, cached_lazy, cached)) = &self.skin_cache
            && *cached_skin == skin
            && *cached_lazy == lazy
        {
            return Ok(Arc::clone(cached));
        }
        let name = self.string_at(add(skin, offsets.name_k_backing_field)?)?;
        let mut files = BTreeMap::new();
        if builtin.is_none() {
            // Lazy<T>._value is at +0x18 on the bundled runtime, as used for the
            // existing Lazy<Playfield> reader. A null value is not cached.
            let dict = self.ptr(add(lazy, 0x18)?)?;
            if dict == 0 {
                return Err(corrupt("skin mapping", lazy, "mapping not initialized"));
            }
            let entries = self.ptr(add(dict, 0x10)?)?;
            let count = self.process.read_i32(add(dict, 0x38)?)?;
            if !(0..=16384).contains(&count) {
                return Err(corrupt("skin mapping", dict, "invalid file count"));
            }
            for i in 0..count as usize {
                let slot = add(entries, 0x10 + i * 0x18)?;
                // CLR groups the two GC references before Entry's scalar fields.
                if self.process.read_i32(add(slot, 20)?)? < -1 {
                    continue;
                }
                let key = self.string_at(slot)?;
                let path = self.string_at(add(slot, 8)?)?;
                // Validate the hash and reconstruct the path, rather than trusting
                // arbitrary filenames/paths read from another process.
                let hash = path.rsplit(['/', '\\']).next().unwrap_or_default();
                let relative = lazer_path(hash)?;
                if relative.is_empty() {
                    continue;
                }
                files.insert(key.replace('\\', "/").to_lowercase(), relative);
            }
            if self.ptr(add(
                realm_mapping(self, skin, offsets.store, store_offset)?,
                mapping_offset,
            )?)? != lazy
            {
                return Err(corrupt("skin mapping", lazy, "skin changed during capture"));
            }
        }
        if self.ptr(add(current, 0x20)?)? != skin {
            return Err(corrupt("current skin", skin, "skin changed during capture"));
        }
        let result = Arc::new(CurrentSkin {
            name,
            builtin: builtin.map(str::to_owned),
            files,
        });
        self.skin_cache = Some((skin, lazy, Arc::clone(&result)));
        Ok(result)
    }
}

fn realm_mapping(
    reader: &LazerMemory,
    skin: usize,
    skin_store: usize,
    stores_offset: usize,
) -> Result<usize, MemoryError> {
    let store = reader.ptr(add(skin, skin_store)?)?;
    let stores = reader.ptr(add(store, stores_offset)?)?;
    let (_, items) = reader.list_info(stores)?;
    reader.ptr(items)
}

fn builtin_identity(id: [u8; 16]) -> Option<&'static str> {
    // Guid's in-memory encoding uses little endian for its first three fields.
    let a = u32::from_le_bytes(id[..4].try_into().unwrap());
    let b = u16::from_le_bytes(id[4..6].try_into().unwrap());
    let c = u16::from_le_bytes(id[6..8].try_into().unwrap());
    let d = u64::from_be_bytes(id[8..].try_into().unwrap());
    match (a, b, c, d) {
        (0x2991cfd8, 0x2140, 0x469a, 0xbcb92ec23fbce4ad) => Some("triangles"),
        (0xcffa69de, 0xb3e3, 0x4dee, 0x85633c4f425c05d0) => Some("argon"),
        (0x9fc9cf5d, 0x0f16, 0x4c71, 0x825698868321ac43) => Some("argon-pro"),
        (0x81f02cd3, 0xeec6, 0x4865, 0xac23fae26a386187) => Some("legacy"),
        (0x0555c76a, 0xcc6b, 0x4bb4, 0x9548df76ba72ef25) => Some("retro"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    fn fixture(builtin: bool) -> LazerMemory {
        let o = LazerOffsets::embedded().unwrap();
        let mut memory = SparseMemory::default();
        let (base, manager, binding, skin, live) = (0x1000, 0x3000, 0x4000, 0x5000, 0x6000);
        memory.valid_base(base, &o);
        memory.ptr(
            base + o.osu_game_osu_game_base.skin_manager_k_backing_field,
            manager,
        );
        memory.ptr(
            manager + o.osu_game_skinning_skin_manager.current_skin,
            binding,
        );
        memory.ptr(binding + 0x20, skin);
        memory.ptr(skin + o.osu_game_skinning_skin.skin_info, live);
        memory.string_ptr(
            skin + o.osu_game_skinning_skin.name_k_backing_field,
            0x7000,
            "Argon",
        );
        let mut id = [0u8; 16];
        if builtin {
            id[..4].copy_from_slice(&0xcffa69deu32.to_le_bytes());
            id[4..6].copy_from_slice(&0xb3e3u16.to_le_bytes());
            id[6..8].copy_from_slice(&0x4deeu16.to_le_bytes());
            id[8..].copy_from_slice(&0x85633c4f425c05d0u64.to_be_bytes());
        }
        memory.put(live + o.skin_live.as_ref().unwrap().id, id);
        if !builtin {
            memory.ptr(skin + o.osu_game_skinning_skin.store, 0x8000);
            memory.ptr(
                0x8000 + o.skin_resource_store.as_ref().unwrap().stores,
                0x9000,
            );
            memory.ptr(0x9008, 0xa000);
            memory.i32(0x9010, 1);
            memory.ptr(0xa010, 0xb000);
            memory.ptr(
                0xb000 + o.skin_realm_store.as_ref().unwrap().mapping,
                0xc000,
            );
            memory.ptr(0xc018, 0xd000);
            memory.ptr(0xd010, 0xe000);
            memory.i32(0xd038, 1);
            memory.i32(0xe024, -1);
            memory.string_ptr(0xe010, 0xf000, "SKIN.INI");
            memory.string_ptr(0xe018, 0x10000, &format!("a/aa/{}", "a".repeat(64)));
        }
        let mut decoder = reader(Arc::new(memory));
        decoder.set_resolved_state(base, 0);
        decoder
    }

    #[test]
    fn offline_current_skin_memory_chain_and_cache() {
        let mut decoder = fixture(false);
        let skin = decoder.read_current_skin().unwrap();
        assert_eq!(skin.name, "Argon");
        assert_eq!(
            skin.builtin, None,
            "an imported skin named Argon is not builtin"
        );
        assert_eq!(skin.files["skin.ini"], format!("a/aa/{}", "a".repeat(64)));
        assert!(Arc::ptr_eq(&skin, &decoder.read_current_skin().unwrap()));
    }

    #[test]
    fn offline_builtin_skin_has_identity_and_no_files() {
        let skin = fixture(true).read_current_skin().unwrap();
        assert_eq!(skin.builtin.as_deref(), Some("argon"));
        assert!(skin.files.is_empty());
        assert_eq!(builtin_identity([0; 16]), None);
    }
}

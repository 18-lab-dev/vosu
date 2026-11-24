use super::*;

impl LazerMemory {
    pub(super) fn visual_values(
        &self,
        store: &IndexMap<i32, usize>,
        fields: &[(i32, &str)],
    ) -> Option<[Option<bool>; 3]> {
        if fields.len() > 3 {
            return None;
        }
        let mut bytes = [[0u8; 1]; 3];
        let mut stage = Stage::<3>::new();
        for ((key, _), byte) in fields.iter().zip(bytes.iter_mut()) {
            if let Some(address) = store.get(key) {
                stage.push(*address, byte);
            }
        }
        stage.read(&self.process)?;
        Some(std::array::from_fn(|index| {
            fields
                .get(index)
                .filter(|(key, _)| store.contains_key(key))
                .map(|_| bytes[index][0] == 1)
        }))
    }

    /// Same bounded ruleset cache as the scalar path. Any speculative failure
    /// leaves publication and first-error ordering to that original path.
    pub(super) fn visual_entries(
        &self,
        entries: usize,
        count: usize,
    ) -> Option<[(String, usize); 64]> {
        if count > 64 || self.process.pointer_size() != 8 {
            return None;
        }
        let mut records = [[0u8; 16]; 64];
        let mut stage = Stage::<64>::new();
        for (index, record) in records.iter_mut().take(count).enumerate() {
            stage.push(add(entries, 0x10 + index * 0x18).ok()?, record);
        }
        stage.read(&self.process)?;
        let mut out: [(String, usize); 64] = std::array::from_fn(|_| (String::new(), 0));
        for start in (0..count).step_by(8) {
            let len = (count - start).min(8);
            let mut heads = [[0u8; STRING_HEAD]; 8];
            let mut spans = [(0usize, 0usize); 8];
            let mut objects = [0usize; 8];
            let mut stage = Stage::<8>::new();
            for (index, ((head, span), object)) in heads
                .iter_mut()
                .zip(spans.iter_mut())
                .zip(objects.iter_mut())
                .take(len)
                .enumerate()
            {
                let record = &records[start + index];
                *object = le_pointer(record)?;
                out[start + index].1 = le_pointer(&record[8..])?;
                if *object != 0 {
                    *span = self.string_head_span(*object)?;
                    stage.push(span.0, &mut head[..span.1]);
                }
            }
            stage.read(&self.process)?;
            for index in 0..len {
                if objects[index] != 0 {
                    out[start + index].0 =
                        self.string_from_head(objects[index], &heads[index][..spans[index].1])?;
                }
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::lazer::test_support::{SparseMemory, reader};

    #[test]
    fn visual_prefetch_decodes_names_and_retains_failure_fallback() {
        let mut memory = SparseMemory::default();
        for (index, (name, object, config)) in [("osu", 0x6000, 0x8000), ("mania", 0x7000, 0x9000)]
            .into_iter()
            .enumerate()
        {
            let entry = 0x3010 + index * 0x18;
            memory.put(object, [0; 512]);
            memory.string_ptr(entry, object, name);
            memory.ptr(entry + 8, config);
        }
        let good = reader(Arc::new(memory));
        let result = good.visual_entries(0x3000, 2).unwrap();
        assert_eq!(result[0], ("osu".into(), 0x8000));
        assert_eq!(result[1], ("mania".into(), 0x9000));

        let mut memory = SparseMemory::default();
        memory.ptr(0x3010, 0x6000);
        memory.ptr(0x3018, 0x8000);
        let faulty = reader(Arc::new(memory));
        assert!(faulty.visual_entries(0x3000, 1).is_none());
    }
}

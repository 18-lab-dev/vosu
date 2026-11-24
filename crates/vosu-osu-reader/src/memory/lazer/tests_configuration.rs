use super::test_support::{SparseMemory, reader};
use super::*;

fn fixture() -> (SparseMemory, Vec<usize>) {
    let mut memory = SparseMemory::default();
    memory.ptr(0x1020, 0x2000);
    memory.ptr(0x2010, 0x3000);
    memory.i32(0x2038, 65);
    let mut fields = vec![0x1020, 0x2010, 0x2038];
    for index in 0..65usize {
        let entry = 0x3010 + index * 0x18;
        memory.ptr(
            entry,
            if index % 5 == 0 {
                0
            } else {
                0x8000 + index * 0x100
            },
        );
        memory.i32(entry + 0x10, (index % 7) as i32);
        fields.extend([entry, entry + 0x10]);
    }
    (memory, fields)
}

fn scalar(reader: &LazerMemory) -> Result<IndexMap<i32, usize>, MemoryError> {
    let store = reader.ptr(0x1020)?;
    let entries = reader.ptr(add(store, 0x10)?)?;
    let count = reader.process.read_i32(add(store, 0x38)?)?;
    let mut result = IndexMap::new();
    for index in 0..count as usize {
        let entry = add(entries, 0x10 + index * 0x18)?;
        let key = reader.process.read_i32(add(entry, 0x10)?)?;
        let value = reader.ptr(entry)?;
        if value != 0 {
            result.insert(key, add(value, 0x40)?);
        }
    }
    Ok(result)
}

#[test]
fn dictionary_batches_preserve_nulls_duplicates_and_chunk_boundaries() {
    let (memory, _) = fixture();
    let memory = Arc::new(memory);
    let reader = reader(memory.clone());
    let expected = scalar(&reader).unwrap();
    assert_eq!(reader.config_store(0x1000).unwrap(), expected);
    assert_eq!(memory.batches.load(Ordering::Relaxed), 3);
}

#[test]
fn dictionary_batch_failure_preserves_first_scalar_error() {
    let (_, fields) = fixture();
    for address in fields {
        let (mut memory, _) = fixture();
        memory.bytes.remove(&address);
        let reader = reader(Arc::new(memory));
        let expected = scalar(&reader).unwrap_err();
        assert_eq!(
            reader.config_store(0x1000).unwrap_err(),
            expected,
            "fault at {address:#x}"
        );
    }
    let (mut memory, _) = fixture();
    memory.ptr(0x3010, usize::MAX);
    let reader = reader(Arc::new(memory));
    assert_eq!(
        reader.config_store(0x1000).unwrap_err(),
        scalar(&reader).unwrap_err()
    );
}

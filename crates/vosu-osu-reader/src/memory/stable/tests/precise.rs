use super::*;
#[test]
fn precise_global_reuses_validated_pointer_chains_without_changing_values() {
    let mut bytes = vec![0; 0x1400];
    put_u32(&mut bytes, 0xb00, 0x1200);
    put_u32(&mut bytes, 0x1200, 2);
    put_u32(&mut bytes, 0x305, 0x1210);
    put_i32(&mut bytes, 0x1210, -123);
    let memory = Arc::new(SparseMemory {
        base: 0,
        bytes,
        reads: AtomicUsize::new(0),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    let mut reader = StableMemory::new(process, fixture_patterns());

    let ReadStatus::Ready(first) = reader.global_precise().unwrap() else {
        panic!()
    };
    let ReadStatus::Ready(second) = reader.global_precise().unwrap() else {
        panic!()
    };
    assert_eq!(
        first,
        GlobalPrecise {
            status: 2,
            time: -123.0,
        }
    );
    assert_eq!(second, first);
    assert_eq!(memory.reads.load(Ordering::Relaxed), 5);
    assert_eq!(memory.batches.load(Ordering::Relaxed), 1);
    let ReadStatus::Ready(third) = reader.global_precise().unwrap() else {
        panic!()
    };
    assert_eq!(third, first);
    assert_eq!(memory.reads.load(Ordering::Relaxed), 6);
    assert_eq!(memory.batches.load(Ordering::Relaxed), 2);
}

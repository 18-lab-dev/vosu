//! The visualizer exposes forty independent, known float addresses. Keep them
//! as separate ops (and therefore preserve per-address failure attribution), but
//! send one read_many/plan instead of forty synchronous scalar plans.
use super::*;

pub(super) fn read(process: &ProcessHandle, array: usize) -> Result<Vec<f64>, MemoryError> {
    let mut bytes = [[0; 4]; 40];
    let mut requests = bytes.each_mut().map(|dst| ReadRequest::new(0, dst));
    for (index, request) in requests.iter_mut().enumerate() {
        request.address = add(array, 0x10 + 4 * index)?;
    }
    process.read_many(&mut requests)?;
    Ok(bytes
        .into_iter()
        .map(|value| f32::from_le_bytes(value).into())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{MemoryRegion, ProcessBitness, ProcessMemory};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct AudioMemory {
        batches: AtomicUsize,
        fail: bool,
    }
    impl ProcessMemory for AudioMemory {
        fn read_exact(&self, _: usize, _: &mut [u8]) -> Result<(), MemoryError> {
            panic!("audio must batch the forty known scalar addresses");
        }
        fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
            self.batches.fetch_add(1, Ordering::Relaxed);
            assert_eq!(reads.len(), 40);
            for (index, request) in reads.iter_mut().enumerate() {
                assert_eq!(request.address, 0x1010 + 4 * index);
                if self.fail && index == 7 {
                    return Err(MemoryError::Read {
                        address: request.address,
                        len: 4,
                        message: "unreadable amplitude".into(),
                    });
                }
                request
                    .dst
                    .copy_from_slice(&(index as f32 + 0.25).to_le_bytes());
            }
            Ok(())
        }
        fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
            unreachable!("audio does not enumerate memory");
        }
    }

    #[test]
    fn audio_uses_one_batch_and_preserves_float_order() {
        let memory = Arc::new(AudioMemory {
            batches: AtomicUsize::new(0),
            fail: false,
        });
        let process = ProcessHandle::new(memory.clone(), ProcessBitness::X64);
        let values = read(&process, 0x1000).unwrap();
        assert_eq!(
            values,
            (0..40).map(|index| index as f64 + 0.25).collect::<Vec<_>>()
        );
        assert_eq!(memory.batches.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn audio_batch_retains_failing_scalar_address() {
        let memory = Arc::new(AudioMemory {
            batches: AtomicUsize::new(0),
            fail: true,
        });
        let process = ProcessHandle::new(memory, ProcessBitness::X64);
        assert!(matches!(
            read(&process, 0x1000),
            Err(MemoryError::Read {
                address: 0x102c,
                len: 4,
                ..
            })
        ));
    }
}

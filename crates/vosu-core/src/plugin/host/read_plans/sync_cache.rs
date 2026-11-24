//! One bounded successful lookup per calling thread. No lock spans a plugin call.
use super::*;
use std::sync::{Weak, atomic::AtomicU64};

struct Cached {
    host: Weak<PluginHost>,
    instance: PluginInstanceId,
    handle: SessionHandle,
    epoch: Arc<AtomicU64>,
    generation: u64,
    // A reader thread must not keep a detached process handle alive.
    memory: Weak<SessionMemory>,
}

thread_local! {
    static LAST: RefCell<Option<Cached>> = const { RefCell::new(None) };
}

pub(super) fn resolve(
    host: &PluginHost,
    instance: PluginInstanceId,
    handle: SessionHandle,
) -> Result<Arc<SessionMemory>, vosu_status> {
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        if !host.shutting_down.load(Ordering::Acquire)
            && let Some(cached) = last.as_ref()
            && Weak::ptr_eq(&cached.host, &host.self_ref)
            && cached.instance == instance
            && cached.handle == handle
            && cached.epoch.load(Ordering::Acquire) == cached.generation
            && let Some(memory) = cached.memory.upgrade()
        {
            return Ok(memory);
        }
        // Same lock order as before: instances then sessions. Both are released
        // before fallible native opens, execution, and reentrant result delivery.
        let (pid, epoch, generation) = {
            let instances = host
                .instances
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let plugin = instances
                .get(&instance)
                .ok_or(vosu_status::VOSU_ERR_INVALID_ARGUMENT)?
                .plugin_id();
            let sessions = host.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            let (owner, pid) = sessions
                .owner_and_pid(handle)
                .ok_or(vosu_status::VOSU_ERR_TARGET_GONE)?;
            if owner != plugin {
                return Err(vosu_status::VOSU_ERR_PERMISSION_DENIED);
            }
            let epoch = sessions.read_cache_epoch();
            let generation = epoch.load(Ordering::Relaxed);
            (pid, epoch, generation)
        };
        let memory = host
            .session_memory(handle, pid)
            .map_err(vosu_status::from)?;
        // If detach raced the open, this stale generation can never hit the
        // fast path, including when this OS PID is reused by another process.
        *last = Some(Cached {
            host: host.self_ref.clone(),
            instance,
            handle,
            epoch,
            generation,
            memory: Arc::downgrade(&memory),
        });
        Ok(memory)
    })
}

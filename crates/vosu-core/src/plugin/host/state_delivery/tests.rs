use super::*;
use vosu_plugin_api::StateEntry;

impl PluginHost {
    pub(in crate::plugin::host) fn state_delivery_idle_for_tests(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
    ) -> bool {
        self.state_delivery
            .lock()
            .unwrap()
            .slots
            .get(&(who, key))
            .is_none_or(|slot| {
                let slot = slot.lock().unwrap();
                !slot.queued && !slot.dirty()
            })
    }

    pub(in crate::plugin::host) fn set_lifecycle_cap_for_tests(&self, cap: usize) {
        self.state_delivery.lock().unwrap().lifecycle_cap = cap;
    }

    pub(in crate::plugin::host) fn lifecycle_backlog_for_tests(
        &self,
        who: PluginInstanceId,
    ) -> (usize, bool) {
        let delivery = self.state_delivery.lock().unwrap();
        (
            delivery
                .slots
                .iter()
                .filter(|((id, _), slot)| {
                    let slot = slot.lock().unwrap();
                    *id == who
                        && slot.lifecycle
                        && (slot.attached.is_some() || slot.detached.is_some())
                })
                .count(),
            delivery.watchers.contains(&who),
        )
    }
}

fn entry(pointer: &str, value: u64) -> StateEntry {
    StateEntry {
        ns: "vosu.play".into(),
        pointer: pointer.into(),
        value: Some(value.into()),
        error: None,
    }
}

#[test]
fn out_of_order_notifications_keep_newest_per_selected_key() {
    let mut slot = Slot::default();
    slot.merge_at(3, vec![entry("/combo", 3)]);
    slot.merge_at(2, vec![entry("/combo", 2), entry("/accuracy", 2)]);
    let values = slot.pending.take();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], entry("/combo", 3));
    assert_eq!(values[1], entry("/accuracy", 2));
    slot.baseline_revision = Some(4);
    slot.revisions.clear();
    slot.merge_at(3, vec![entry("/combo", 3)]);
    assert!(slot.pending.is_empty());
}

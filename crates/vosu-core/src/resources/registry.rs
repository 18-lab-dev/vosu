use std::{collections::HashMap, sync::Arc};

use super::{
    BindingOwner, ReferenceIdentity, RegistryError, RegistryLimits, RegistryStats, ResourceId,
    ResourceSecret,
};

#[derive(Clone)]
struct Record {
    identity: Arc<ReferenceIdentity>,
    owners: Vec<BindingOwner>,
    sequence: u64,
}

/// No resolution state or secondary owner/tombstone map is retained. Eviction
/// removes all owners atomically; authenticity supplies 410 without tombstones.
#[derive(Clone)]
pub struct ResourceRegistry {
    records: HashMap<ResourceId, Record>,
    limits: RegistryLimits,
    stats: RegistryStats,
    revision: u64,
    sequence: u64,
}

/// An isolated candidate, discarded without side effects on rejected state
/// commits. Keep the state/registry locks across prepare and commit; the revision
/// check additionally rejects stale candidates rather than losing newer owners.
pub struct PreparedReferenceBindings {
    revision: u64,
    candidate: ResourceRegistry,
}

impl ResourceRegistry {
    pub fn new(limits: RegistryLimits) -> Self {
        Self {
            records: HashMap::new(),
            limits,
            stats: RegistryStats::default(),
            revision: 0,
            sequence: 0,
        }
    }

    pub fn stats(&self) -> RegistryStats {
        self.stats
    }

    /// Replace exact state-leaf bindings. Additions also replace their previous
    /// token binding, including when it belongs to a different reference class.
    /// Ancestor/root removal is supplied as all affected leaves by the state
    /// transaction, which owns the authoritative document's pointer traversal.
    pub fn prepare(
        &self,
        removals: &[BindingOwner],
        additions: Vec<(BindingOwner, Arc<ReferenceIdentity>)>,
    ) -> Result<PreparedReferenceBindings, RegistryError> {
        let mut candidate = self.clone();
        for owner in removals {
            candidate.remove_owner(owner);
        }
        for (owner, identity) in additions {
            candidate.remove_owner(&owner);
            let bytes = identity.bytes() + owner.bytes() + size_of::<Record>();
            if candidate.limits.records == 0 || bytes > candidate.limits.metadata_bytes {
                return Err(RegistryError::Capacity);
            }
            let record = candidate
                .records
                .entry(identity.id.clone())
                .or_insert_with(|| {
                    candidate.sequence += 1;
                    Record {
                        identity,
                        owners: Vec::new(),
                        sequence: candidate.sequence,
                    }
                });
            record.owners.push(owner);
            candidate.recount();
            // Oldest-published whole-record eviction. Re-publishing a retained
            // token does not refresh its age or grow a hidden eviction queue.
            while candidate.stats.records > candidate.limits.records
                || candidate.stats.metadata_bytes > candidate.limits.metadata_bytes
            {
                let oldest = candidate
                    .records
                    .iter()
                    .min_by_key(|(_, record)| record.sequence)
                    .map(|(id, _)| id.clone())
                    .ok_or(RegistryError::Capacity)?;
                candidate.records.remove(&oldest);
                candidate.stats.evictions = candidate.stats.evictions.saturating_add(1);
                candidate.recount();
            }
        }
        Ok(PreparedReferenceBindings {
            revision: self.revision,
            candidate,
        })
    }

    /// No allocation or callbacks; call before publishing the prepared document.
    pub fn commit(&mut self, mut prepared: PreparedReferenceBindings) -> Result<(), RegistryError> {
        if prepared.revision != self.revision {
            return Err(RegistryError::StalePreparation);
        }
        prepared.candidate.revision = self
            .revision
            .checked_add(1)
            .expect("registry revision exhausted");
        *self = prepared.candidate;
        Ok(())
    }

    /// Detach/instance loss cleanup does not require consulting plugin code.
    pub fn remove_instance(&mut self, instance: u64) {
        self.records.retain(|_, record| {
            record.owners.retain(|owner| owner.instance != instance);
            !record.owners.is_empty()
        });
        self.revision = self
            .revision
            .checked_add(1)
            .expect("registry revision exhausted");
        self.recount();
    }

    pub fn remove_session(&mut self, instance: u64, session: u64) {
        self.records.retain(|_, record| {
            record
                .owners
                .retain(|owner| owner.instance != instance || owner.session != session);
            !record.owners.is_empty()
        });
        self.revision = self
            .revision
            .checked_add(1)
            .expect("registry revision exhausted");
        self.recount();
    }

    /// The caller supplies its already-loaded plugin identities, not a token's
    /// own assertion. Authentication always precedes plugin/membership lookup.
    pub fn lookup(
        &self,
        secret: &ResourceSecret,
        token: &str,
        known_plugins: &[&str],
    ) -> Result<&ReferenceIdentity, RegistryError> {
        let (id, plugin, _) = secret.verify(token)?;
        if !known_plugins.contains(&plugin.as_str()) {
            return Err(RegistryError::NotFound);
        }
        self.records
            .get(&id)
            .map(|record| record.identity.as_ref())
            .ok_or(RegistryError::Gone)
    }

    /// Authenticated current identity and its bounded set of publication owners.
    pub fn lookup_bindings(
        &self,
        secret: &ResourceSecret,
        token: &str,
        known_plugins: &[&str],
    ) -> Result<(Arc<ReferenceIdentity>, Vec<BindingOwner>), RegistryError> {
        let identity = self.lookup(secret, token, known_plugins)?;
        let record = self.records.get(&identity.id).expect("lookup found record");
        Ok((Arc::clone(&record.identity), record.owners.clone()))
    }

    fn remove_owner(&mut self, owner: &BindingOwner) {
        self.records.retain(|_, record| {
            record.owners.retain(|current| current != owner);
            !record.owners.is_empty()
        });
        self.recount();
    }

    fn recount(&mut self) {
        self.stats.records = self.records.len();
        self.stats.bindings = self
            .records
            .values()
            .map(|record| record.owners.len())
            .sum();
        self.stats.metadata_bytes = self
            .records
            .values()
            .map(|record| {
                record.identity.bytes()
                    + size_of::<Record>()
                    + size_of::<ResourceId>()
                    + (record.owners.capacity() - record.owners.len()) * size_of::<BindingOwner>()
                    + record.owners.iter().map(BindingOwner::bytes).sum::<usize>()
            })
            .sum();
    }
}

impl Default for ResourceRegistry {
    fn default() -> Self {
        Self::new(RegistryLimits::default())
    }
}

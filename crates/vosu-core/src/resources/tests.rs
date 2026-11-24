use super::*;
use ciborium::Value;
use proptest::prelude::*;
use std::sync::Arc;
use vosu_plugin_api::ResourceRef;

fn reference(key: Value) -> ResourceRef {
    ResourceRef {
        kind: "background".into(),
        key,
        name: "sentinel.png".into(),
        content_id: Value::Text("sha256:abc123".into()),
    }
}

fn identity(secret: &ResourceSecret, key: Value) -> Arc<ReferenceIdentity> {
    Arc::new(
        ReferenceIdentity::new(
            secret,
            "test.resource",
            ReferenceClass::Resource,
            reference(key),
        )
        .unwrap(),
    )
}

fn owner(session: u64) -> BindingOwner {
    BindingOwner {
        instance: 71,
        session,
        namespace: "test.resource".into(),
        pointer: "/cover".into(),
    }
}

#[test]
fn resources_lookup_bindings_excludes_retired_owners_and_authenticates_first() {
    let secret = ResourceSecret::fixture([137; 32]);
    let identity = identity(&secret, Value::Integer(193.into()));
    let mut registry = ResourceRegistry::default();
    let prepared = registry
        .prepare(
            &[],
            vec![
                (owner(191), identity.clone()),
                (owner(192), identity.clone()),
            ],
        )
        .unwrap();
    registry.commit(prepared).unwrap();
    registry.remove_session(71, 191);
    let (current, bindings) = registry
        .lookup_bindings(&secret, identity.id.as_str(), &["test.resource"])
        .unwrap();
    assert_eq!(current.id, identity.id);
    assert_eq!(bindings, vec![owner(192)]);
    assert!(matches!(
        registry.lookup_bindings(&secret, identity.id.as_str(), &[]),
        Err(RegistryError::NotFound)
    ));
    registry.remove_session(71, 192);
    assert!(matches!(
        registry.lookup_bindings(&secret, identity.id.as_str(), &["test.resource"]),
        Err(RegistryError::Gone)
    ));
}

#[test]
fn resources_multiple_owners_and_detach_do_not_retire_live_token() {
    let secret = ResourceSecret::fixture([19; 32]);
    let identity = identity(&secret, Value::Integer(37.into()));
    let mut registry = ResourceRegistry::default();
    let transaction = registry
        .prepare(
            &[],
            vec![(owner(81), identity.clone()), (owner(82), identity.clone())],
        )
        .unwrap();
    assert_eq!(
        registry.stats().records,
        0,
        "preparation must not publish membership"
    );
    registry.commit(transaction).unwrap();
    assert_eq!(registry.stats().bindings, 2);
    registry.remove_session(71, 81);
    assert!(
        registry
            .lookup(&secret, identity.id.as_str(), &["test.resource"])
            .is_ok()
    );
    registry.remove_instance(71);
    assert!(matches!(
        registry.lookup(&secret, identity.id.as_str(), &["test.resource"]),
        Err(RegistryError::Gone)
    ));
    assert_eq!(registry.stats().metadata_bytes, 0);
}

#[test]
fn resources_eviction_authentication_and_capacity_are_independent_of_resolution() {
    let secret = ResourceSecret::fixture([23; 32]);
    let first = identity(&secret, Value::Integer(101.into()));
    let second = identity(&secret, Value::Integer(102.into()));
    let mut registry = ResourceRegistry::new(RegistryLimits {
        records: 1,
        metadata_bytes: 4096,
    });
    let prepared = registry
        .prepare(&[], vec![(owner(91), first.clone())])
        .unwrap();
    registry.commit(prepared).unwrap();
    let stale = registry.prepare(&[], Vec::new()).unwrap();
    let prepared = registry
        .prepare(&[], vec![(owner(92), second.clone())])
        .unwrap();
    registry.commit(prepared).unwrap();
    assert!(matches!(
        registry.commit(stale),
        Err(RegistryError::StalePreparation)
    ));
    assert_eq!(registry.stats().evictions, 1);
    assert!(matches!(
        registry.lookup(&secret, first.id.as_str(), &["test.resource"]),
        Err(RegistryError::Gone)
    ));
    assert!(
        registry
            .lookup(&secret, second.id.as_str(), &["test.resource"])
            .is_ok()
    );
    assert!(matches!(
        registry.lookup(&secret, second.id.as_str(), &[]),
        Err(RegistryError::NotFound)
    ));
    assert!(matches!(
        registry.lookup(
            &ResourceSecret::fixture([24; 32]),
            second.id.as_str(),
            &["test.resource"]
        ),
        Err(RegistryError::NotFound)
    ));
    let mut tiny = ResourceRegistry::new(RegistryLimits {
        records: 10,
        metadata_bytes: 1,
    });
    assert!(matches!(
        tiny.prepare(&[], vec![(owner(93), second)]),
        Err(RegistryError::Capacity)
    ));
    tiny.remove_instance(71);
    assert_eq!(tiny.stats().records, 0);
}

#[test]
fn resources_replacement_and_dropped_transaction_are_atomic() {
    let secret = ResourceSecret::fixture([31; 32]);
    let first = identity(&secret, Value::Integer(111.into()));
    let second = identity(&secret, Value::Integer(112.into()));
    let mut registry = ResourceRegistry::default();
    let prepared = registry
        .prepare(&[], vec![(owner(141), first.clone())])
        .unwrap();
    registry.commit(prepared).unwrap();
    drop(
        registry
            .prepare(&[owner(141)], vec![(owner(142), second.clone())])
            .unwrap(),
    );
    assert!(
        registry
            .lookup(&secret, first.id.as_str(), &["test.resource"])
            .is_ok()
    );
    assert!(matches!(
        registry.lookup(&secret, second.id.as_str(), &["test.resource"]),
        Err(RegistryError::Gone)
    ));
    let prepared = registry
        .prepare(&[], vec![(owner(141), second.clone())])
        .unwrap();
    registry.commit(prepared).unwrap();
    assert_eq!(registry.stats().bindings, 1);
    assert!(matches!(
        registry.lookup(&secret, first.id.as_str(), &["test.resource"]),
        Err(RegistryError::Gone)
    ));
    let prepared = registry.prepare(&[owner(141)], Vec::new()).unwrap();
    registry.commit(prepared).unwrap();
    assert_eq!(registry.stats().metadata_bytes, 0);
}

#[test]
fn resources_deterministic_cbor_and_duplicate_keys() {
    let secret = ResourceSecret::fixture([43; 32]);
    let a = (Value::Integer(24.into()), Value::Text("integer".into()));
    let b = (Value::Integer((-1).into()), Value::Text("negative".into()));
    let left = identity(&secret, Value::Map(vec![a.clone(), b.clone()]));
    let right = identity(&secret, Value::Map(vec![b.clone(), a.clone()]));
    assert_eq!(left.id, right.id);
    assert_ne!(
        left.reference.key, right.reference.key,
        "original map order must survive for dispatch"
    );
    assert!(matches!(
        ReferenceIdentity::new(
            &secret,
            "test.resource",
            ReferenceClass::Resource,
            reference(Value::Map(vec![a.clone(), a]))
        ),
        Err(RegistryError::InvalidReference)
    ));
    let nan_a = identity(&secret, Value::Float(f64::NAN));
    let nan_b = identity(&secret, Value::Float(f64::from_bits(0x7ff8_0000_0000_0042)));
    assert_eq!(nan_a.id, nan_b.id);
    let tree = ReferenceIdentity::new(
        &secret,
        "test.resource",
        ReferenceClass::Tree,
        (*left.reference).clone(),
    )
    .unwrap();
    assert_ne!(left.id, tree.id);
    let mut deep = Value::Null;
    for _ in 0..34 {
        deep = Value::Array(vec![deep]);
    }
    assert!(matches!(
        ReferenceIdentity::new(
            &secret,
            "test.resource",
            ReferenceClass::Resource,
            reference(deep)
        ),
        Err(RegistryError::InvalidReference)
    ));
}

#[test]
fn resources_secret_persistence_and_malformed_existing_file_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resource-secret");
    let first = ResourceSecret::open_or_create(&path).unwrap();
    let token = identity(&first, Value::Integer(517.into()));
    let second = ResourceSecret::open_or_create(&path).unwrap();
    assert!(second.verify(token.id.as_str()).is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            matches!(ResourceSecret::open_or_create(&path), Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
        );
    }
    let malformed = dir.path().join("malformed");
    std::fs::write(&malformed, b"must-not-rotate").unwrap();
    assert!(ResourceSecret::open_or_create(&malformed).is_err());
    assert_eq!(std::fs::read(&malformed).unwrap(), b"must-not-rotate");
    assert!(ResourceSecret::open_or_create(dir.path()).is_err());
}

#[test]
fn resources_metadata_budget_bounds_repeated_owners() {
    let secret = ResourceSecret::fixture([59; 32]);
    let identity = identity(&secret, Value::Integer(235.into()));
    let mut registry = ResourceRegistry::new(RegistryLimits {
        records: 4096,
        metadata_bytes: 1024,
    });
    for session in 201..301 {
        let prepared = registry
            .prepare(&[], vec![(owner(session), identity.clone())])
            .unwrap();
        registry.commit(prepared).unwrap();
        assert!(registry.stats().metadata_bytes <= 2048);
        assert!(registry.stats().bindings < 20);
    }
    assert!(
        registry.stats().evictions > 0,
        "owner-only pressure must reach real eviction"
    );
    registry.remove_instance(71);
    assert_eq!(registry.stats().bindings, 0);
    assert_eq!(registry.stats().metadata_bytes, 0);
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]
    #[test]
    fn prop_resource_id_stable_and_authenticated(key in any::<i64>(), byte in any::<u8>()) {
        let secret = ResourceSecret::fixture([byte; 32]);
        let first = identity(&secret, Value::Integer(key.into()));
        let second = identity(&secret, Value::Integer(key.into()));
        prop_assert_eq!(&first.id, &second.id);
        prop_assert!(secret.verify(first.id.as_str()).is_ok());
        prop_assert!(ResourceSecret::fixture([byte.wrapping_add(1); 32]).verify(first.id.as_str()).is_err());
        let changed = identity(&secret, Value::Integer(key.wrapping_add(1).into()));
        prop_assert_ne!(&first.id, &changed.id);
    }

    #[test]
    fn prop_resource_id_tampered_or_arbitrary_tokens_fail(token in ".{0,600}") {
        let secret = ResourceSecret::fixture([67; 32]);
        prop_assert!(secret.verify(&token).is_err());
        let minted = identity(&secret, Value::Text(token));
        let mut changed = minted.id.as_str().as_bytes().to_vec();
        // Alter an authenticated digest byte, leaving the old tag in place.
        let digest_start = minted.id.as_str().match_indices('.').nth(2).unwrap().0 + 1;
        changed[digest_start] = if changed[digest_start] == b'A' { b'B' } else { b'A' };
        prop_assert!(secret.verify(std::str::from_utf8(&changed).unwrap()).is_err());
    }
}

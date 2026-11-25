use std::io::Write as _;

use sha2::{Digest, Sha256};

use super::library::{open_verified, sha256};
use super::*;

/// A library file whose writer is closed, as an installed package is: an
/// open writer would (correctly) make the Windows deny-write open fail.
fn library_file(bytes: &[u8]) -> tempfile::TempPath {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(bytes).unwrap();
    file.flush().unwrap();
    file.into_temp_path()
}

#[test]
fn sha256_streams_the_whole_object() {
    let bytes: Vec<u8> = (0..200_000_u32).map(|value| value as u8).collect();
    let expected: [u8; 32] = Sha256::digest(&bytes).into();
    assert_eq!(sha256(&mut bytes.as_slice()).unwrap(), expected);
    assert_eq!(
        sha256(&mut [].as_slice()).unwrap(),
        <[u8; 32]>::from(Sha256::digest(b""))
    );
}

#[test]
fn a_matching_library_loads_through_the_object_that_was_hashed() {
    let bytes = b"not really an ELF, only hashed here".to_vec();
    let file = library_file(&bytes);
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let verified = open_verified(&file, &digest).unwrap();
    #[cfg(target_os = "linux")]
    {
        let load_path = verified.load_path().to_str().unwrap().to_owned();
        assert!(load_path.starts_with("/proc/self/fd/"), "{load_path}");
        // Replacing the path after the check does not change what loads.
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, b"substituted").unwrap();
        assert_eq!(std::fs::read(verified.load_path()).unwrap(), bytes);
    }
    #[cfg(windows)]
    {
        assert_eq!(verified.load_path(), &*file);
        // The open handle refuses writers until loading finishes.
        assert!(std::fs::OpenOptions::new().write(true).open(&file).is_err());
    }
}

#[test]
fn a_changed_library_is_the_approval_stage() {
    let file = library_file(b"changed bytes");
    let failure = open_verified(&file, &[0; 32]).err().unwrap();
    assert_eq!(failure.stage, StartupStage::Approval);
    assert_eq!(failure.code, "digest_mismatch");
}

#[test]
fn a_missing_or_relative_library_is_the_library_stage() {
    let dir = tempfile::tempdir().unwrap();
    let failure = open_verified(&dir.path().join("missing.so"), &[0; 32])
        .err()
        .unwrap();
    assert_eq!(failure.stage, StartupStage::Library);
    assert_eq!(failure.code, "open_failed");
    assert!(failure.errno.is_some());
    let failure = open_verified(std::path::Path::new("lib/plugin.so"), &[0; 32])
        .err()
        .unwrap();
    assert_eq!(failure.stage, StartupStage::Library);
    assert_eq!(failure.code, "not_absolute");
}

#[test]
fn load_failures_map_to_library_or_abi() {
    assert_eq!(
        load_failure(&HostError::AbiMismatch {
            expected: 1,
            found: 2
        }),
        (StartupStage::Abi, "abi_mismatch")
    );
    assert_eq!(
        load_failure(&HostError::DescriptorTooSmall {
            minimum: 32,
            found: 8
        }),
        (StartupStage::Abi, "descriptor_too_small")
    );
    assert_eq!(
        load_failure(&HostError::MissingRequiredFunction),
        (StartupStage::Abi, "missing_required_function")
    );
    assert_eq!(
        load_failure(&HostError::MissingEntry),
        (StartupStage::Library, "missing_entry")
    );
}

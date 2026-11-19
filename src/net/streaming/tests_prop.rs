//! Property tests for the ZIP preflight scan (`preflight_zip_entries` /
//! `open_preflighted_zip` in `archive.rs`). 1.2.1 fixed two end-record bugs
//! (7b0a267, ed8d2c4: a decoder/preflight disagreement on which bytes form
//! the real end-of-central-directory record). These generate the archive
//! shapes that class of bug hides in, rather than re-asserting the exact
//! fixed byte sequences the unit tests above already cover.

use std::collections::HashSet;
use std::io::Write;

use proptest::prelude::*;
use tokio_util::sync::CancellationToken;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::super::*;
use crate::proptest_support::vosu_proptest_config;

fn entry_name() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_.-]{1,12}"
}

fn entry_body() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..64)
}

fn entries_strategy(max_entries: usize) -> impl Strategy<Value = Vec<(String, Vec<u8>, bool)>> {
    prop::collection::vec((entry_name(), entry_body(), any::<bool>()), 0..max_entries).prop_map(
        |entries| {
            // Entry names must be unique for a well-formed archive; dedupe
            // deterministically instead of discarding the whole case.
            let mut seen = HashSet::new();
            entries
                .into_iter()
                .filter(|(name, _, _)| seen.insert(name.clone()))
                .collect()
        },
    )
}

/// A comment with no byte window equal to the EOCD signature, so a valid
/// archive's acceptance in the differential tests never turns on an
/// accidental decoy.
fn clean_comment() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..16)
        .prop_filter("comment must not contain an EOCD signature", |bytes| {
            !bytes.windows(4).any(|window| window == b"PK\x05\x06")
        })
}

/// Builds a valid ZIP with the given entries (`Stored` or `Deflated`
/// compression chosen per entry, so a flipped bit or an embedded decoy
/// signature can land inside either raw or compressed payload bytes) and an
/// explicit raw comment.
fn build_zip(entries: &[(String, Vec<u8>, bool)], comment: &[u8]) -> Vec<u8> {
    let file = tempfile::NamedTempFile::new().unwrap();
    {
        let mut writer = ZipWriter::new(file.reopen().unwrap());
        for (name, body, stored) in entries {
            let options = SimpleFileOptions::default().compression_method(if *stored {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            });
            writer.start_file(name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer
            .set_raw_comment(comment.to_vec().into_boxed_slice())
            .unwrap();
        writer.finish().unwrap();
    }
    std::fs::read(file.path()).unwrap()
}

fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), bytes).unwrap();
    file
}

proptest! {
    #![proptest_config(vosu_proptest_config())]

    /// Differential oracle: whenever preflight accepts a valid archive, the
    /// real `zip` decoder (when it also accepts it) reads exactly the same
    /// entry count, `open_preflighted_zip` then succeeds, and the accepted
    /// count never exceeds the configured limit.
    #[test]
    fn prop_preflight_matches_decoder_on_valid_archives(
        entries in entries_strategy(12),
        comment in clean_comment(),
        maximum in 0usize..20,
    ) {
        let bytes = build_zip(&entries, &comment);
        let mut file = write_temp(&bytes).reopen().unwrap();
        let preflight = preflight_zip_entries(&mut file, maximum, &CancellationToken::new());
        if let Ok(count) = preflight {
            prop_assert!(count as usize <= maximum);
            let opened = zip::ZipArchive::new(write_temp(&bytes).reopen().unwrap());
            if let Ok(archive) = opened {
                prop_assert_eq!(archive.len() as u64, count);
            }
            prop_assert!(open_preflighted_zip(write_temp(&bytes).reopen().unwrap(), count).is_ok());
        }
    }

    /// A valid archive is always accepted and its claimed entry count always
    /// matches its real entry count, as long as the limit allows it: no
    /// false refusal of ordinary archives, including ones whose compressed
    /// payload bytes happen to contain `PK\x05\x06`.
    #[test]
    fn prop_preflight_accepts_valid_archives_within_the_limit(
        entries in entries_strategy(12),
        comment in clean_comment(),
    ) {
        let count = entries.len() as u64;
        let bytes = build_zip(&entries, &comment);
        let mut file = write_temp(&bytes).reopen().unwrap();
        let result = preflight_zip_entries(&mut file, entries.len().max(1), &CancellationToken::new());
        prop_assert_eq!(result.unwrap(), count);
    }

    /// Anything appended after the archive's real end record is refused,
    /// regardless of what it looks like, as long as it is not itself another
    /// complete, differently-positioned archive ending at the new EOF.
    #[test]
    fn prop_preflight_refuses_data_after_the_archive(
        entries in entries_strategy(6),
        comment in clean_comment(),
        trailing in prop::collection::vec(any::<u8>(), 1..32)
            .prop_filter("trailing data must not itself start a new end record", |bytes| {
                bytes.len() < 4 || bytes[..4] != *b"PK\x05\x06"
            }),
    ) {
        let mut bytes = build_zip(&entries, &comment);
        bytes.extend_from_slice(&trailing);
        let mut file = write_temp(&bytes).reopen().unwrap();
        let result = preflight_zip_entries(&mut file, usize::MAX, &CancellationToken::new());
        prop_assert!(matches!(
            result,
            Err(NetworkError::UnsafeZip(message)) if message == "no ZIP end record at the end of the file"
        ));
    }

    /// Any occurrence of the EOCD signature inside the archive's own comment,
    /// anywhere after the real record, is refused as ambiguous: real
    /// archives never contain one, and other ZIP readers search from the
    /// front while this scan (matching the `zip` crate) takes the last.
    #[test]
    fn prop_preflight_refuses_a_decoy_signature_in_the_comment(
        entries in entries_strategy(6),
        prefix in clean_comment(),
        suffix in prop::collection::vec(any::<u8>(), 0..16),
    ) {
        let mut comment = prefix;
        comment.extend_from_slice(b"PK\x05\x06");
        comment.extend_from_slice(&suffix);
        let bytes = build_zip(&entries, &comment);
        let mut file = write_temp(&bytes).reopen().unwrap();
        let result = preflight_zip_entries(&mut file, usize::MAX, &CancellationToken::new());
        prop_assert!(matches!(
            result,
            Err(NetworkError::UnsafeZip(message)) if message == "ZIP comment contains another end record"
        ));
    }

    /// No byte-level corruption of a valid archive (truncation or a single
    /// flipped byte) ever panics, and on the rare case corruption still
    /// leaves something preflight accepts, its count still matches whatever
    /// the real decoder reads when the decoder also accepts it.
    #[test]
    fn prop_preflight_never_panics_and_stays_consistent_under_corruption(
        entries in entries_strategy(6),
        comment in clean_comment(),
        truncate in any::<bool>(),
        position in 0usize..256,
        replacement in any::<u8>(),
    ) {
        let mut bytes = build_zip(&entries, &comment);
        if truncate {
            bytes.truncate(position % (bytes.len() + 1));
        } else {
            let index = position % bytes.len();
            bytes[index] = replacement;
        }
        let mut file = write_temp(&bytes).reopen().unwrap();
        let result = preflight_zip_entries(&mut file, usize::MAX, &CancellationToken::new());
        if let Ok(count) = result {
            let reopened = write_temp(&bytes).reopen().unwrap();
            if let Ok(archive) = zip::ZipArchive::new(reopened) {
                prop_assert_eq!(archive.len() as u64, count);
            }
        }
    }

    /// A fabricated ZIP64 end record is accepted exactly when its claimed
    /// total is within the limit, and refused with "entry limit exceeded"
    /// otherwise -- the bound the ZIP64 cap in `read_end_record` exists to
    /// enforce before any decoder reserves space for it.
    #[test]
    fn prop_zip64_total_is_capped_by_the_limit(
        total in 0u64..300_000,
        maximum in 0usize..200_000,
    ) {
        let mut bytes = vec![0u8; 56];
        bytes[..4].copy_from_slice(b"PK\x06\x06");
        bytes[4..12].copy_from_slice(&44u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&total.to_le_bytes());
        let mut locator = [0u8; 20];
        locator[..4].copy_from_slice(b"PK\x06\x07");
        locator[8..16].copy_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&locator);
        let mut eocd = [0u8; 22];
        eocd[..4].copy_from_slice(b"PK\x05\x06");
        eocd[8..12].copy_from_slice(&[0xff; 4]);
        bytes.extend_from_slice(&eocd);
        let mut file = write_temp(&bytes).reopen().unwrap();
        let result = preflight_zip_entries(&mut file, maximum, &CancellationToken::new());
        if total as usize <= maximum {
            prop_assert_eq!(result.unwrap(), total);
        } else {
            prop_assert!(matches!(
                result,
                Err(NetworkError::UnsafeZip(message)) if message == "entry limit exceeded"
            ));
        }
    }
}

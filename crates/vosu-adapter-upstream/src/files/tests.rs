use super::*;

#[test]
fn path_decodes_exactly_once_and_preserves_literal_plus() {
    assert_eq!(decode_once("%252e%252e/a+b").unwrap(), "%2e%2e/a+b");
    for bad in ["%", "%2", "%gg", "%ff"] {
        assert_eq!(decode_once(bad), Err(FileError::InvalidPath));
    }
    for bad in ["../outside", "..\\outside", "C:/outside", "//host/share"] {
        assert_eq!(
            RelativeChild::parse(bad).unwrap_err(),
            FileError::InvalidPath
        );
    }
}

#[tokio::test]
async fn missing_requester_never_opens_a_path_or_calculates() {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let provider = Provider::default();
        assert!(matches!(
            provider
                .resolve(Route::Songs, "/etc/passwd", "/Songs/etc/passwd")
                .await,
            Outcome::NotReady
        ));
        assert!(matches!(
            provider.resolve(Route::Skin, "", "/files/skin/").await,
            Outcome::Missing
        ));
        assert_eq!(
            provider
                .calculate(crate::calculate::PpQuery {
                    path: Some("/outside.osu".into()),
                    ..Default::default()
                })
                .await,
            crate::calculate::ResultBody::NotReady
        );
    })
    .await
    .expect("bounded fail-closed operation");
}

struct SyntheticDirectory {
    remaining: usize,
    name_len: usize,
}
impl vosu_surface::files::DirectoryReader for SyntheticDirectory {
    fn next_page(
        &mut self,
    ) -> vosu_surface::files::Operation<'_, vosu_surface::files::DirectoryPage> {
        Box::pin(async move {
            let count = self.remaining.min(1024);
            self.remaining -= count;
            Ok(vosu_surface::files::DirectoryPage {
                entries: (0..count)
                    .map(|_| DirectoryEntry {
                        name: "x".repeat(self.name_len),
                        kind: EntryKind::File,
                        size: 1,
                    })
                    .collect(),
                truncated: self.remaining != 0,
            })
        })
    }
}

#[tokio::test]
async fn listing_budget_distinguishes_exact_entry_and_byte_bound_from_overflow() {
    for (count, len, truncated) in [
        (MAX_DIRECTORY_ENTRIES, 1, false),
        (MAX_DIRECTORY_ENTRIES + 1, 1, true),
        (MAX_DIRECTORY_NAME_BYTES / 4096, 4096, false),
        (MAX_DIRECTORY_NAME_BYTES / 4096 + 1, 4096, true),
    ] {
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            directory_entries(Box::new(SyntheticDirectory {
                remaining: count,
                name_len: len,
            })),
        )
        .await
        .expect("bounded synthetic listing");
        let Outcome::Directory {
            entries,
            truncated: actual,
        } = outcome
        else {
            panic!("directory")
        };
        assert_eq!(actual, truncated);
        assert_eq!(
            entries.len(),
            count
                .min(MAX_DIRECTORY_ENTRIES)
                .min(MAX_DIRECTORY_NAME_BYTES / len)
        );
    }
}

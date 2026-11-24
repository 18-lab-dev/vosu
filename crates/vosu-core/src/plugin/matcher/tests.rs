//! [`MatcherEngine`]/[`CompiledMatcher`]/[`Prefilter`] unit tests (split out per
//! `docs/architecture/source-map.md`'s 1,000-line maintained-file limit).

use super::*;

fn table(process_names: &[&str]) -> MatchTable {
    MatchTable {
        process_names: process_names.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn candidate(name: &str, path: &str, command_line: &str) -> CandidateProcess {
    CandidateProcess {
        name: name.into(),
        path: path.into(),
        command_line: command_line.into(),
    }
}

#[test]
fn process_names_match_case_insensitively_on_windows_only() {
    let windows = CompiledMatcher::compile(MatchPlatform::Windows, &table(&["osu!.exe"])).unwrap();
    assert!(windows.matches(&candidate("OSU!.EXE", "C:/osu/osu!.exe", "")));

    let linux = CompiledMatcher::compile(MatchPlatform::Linux, &table(&["osu!.exe"])).unwrap();
    assert!(!linux.matches(&candidate("OSU!.EXE", "/home/user/osu!.exe", "")));
    assert!(linux.matches(&candidate("osu!.exe", "/home/user/osu!.exe", "")));
}

#[test]
fn path_patterns_are_full_match_anchored_and_normalize_separators() {
    // No backslash escapes in the pattern itself: §6.1's path
    // separator normalization rewrites only an escaped literal
    // backslash (`\\`) in the pattern text, so a bare `\` here would be
    // a dangling, invalid regex escape rather than a separator; a
    // pattern spells its own separators as `/` directly.
    let table = MatchTable {
        process_path_patterns: vec![".*/osu-lazer/osu!.exe".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Linux, &table).unwrap();
    assert!(matcher.matches(&candidate("osu!.exe", r"C:\games\osu-lazer\osu!.exe", "")));
    // Not a full match: extra trailing text after the pattern's own end.
    assert!(!matcher.matches(&candidate("osu!.exe", "/games/osu-lazer/osu!.exe.bak", "")));
}

#[test]
fn pattern_escaped_dot_is_not_corrupted_by_separator_normalization() {
    // `\.` is a regex-escaped literal dot, not a path separator: the
    // naive `\`→`/` rewrite used to turn it into `/.`, which no longer
    // matches a literal `.` (P1).
    let table = MatchTable {
        process_path_patterns: vec![r".*/osu!\.exe".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Linux, &table).unwrap();
    assert!(matcher.matches(&candidate("osu!.exe", "/home/u/osu!.exe", "")));
}

#[test]
fn pattern_escaped_parens_are_not_corrupted_by_separator_normalization() {
    // `\(x86\)` is a regex-escaped literal "(x86)", not a path
    // separator plus a capture group: the naive rewrite used to turn
    // it into `/(x86/)`, matching "/x86/" instead of the literal
    // parenthesized text (P1).
    let table = MatchTable {
        process_path_patterns: vec![r".*/Program Files \(x86\)/.*".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Linux, &table).unwrap();
    assert!(matcher.matches(&candidate(
        "osu!.exe",
        "/opt/Program Files (x86)/osu!.exe",
        ""
    )));
    assert!(!matcher.matches(&candidate("osu!.exe", "/opt/x86/osu!.exe", "")));
}

#[test]
fn windows_style_pattern_keeps_its_escaped_dot_while_its_separators_normalize() {
    // A pattern spelling its separators as escaped backslashes (valid
    // regex syntax for a literal `\`) must still normalize only those,
    // keeping `\.` intact.
    let table = MatchTable {
        process_path_patterns: vec![r".*\\osu!\.exe".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Windows, &table).unwrap();
    assert!(matcher.matches(&candidate("osu!.exe", r"C:\games\osu!.exe", "")));
    assert!(!matcher.matches(&candidate("osu!.exe", r"C:\games\osu!Xexe", "")));
}

#[test]
fn command_line_patterns_match_joined_argv() {
    let table = MatchTable {
        command_line_patterns: vec![r".*--tournament.*".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Linux, &table).unwrap();
    assert!(matcher.matches(&candidate(
        "osu!.exe",
        "/osu/osu!.exe",
        "osu!.exe --tournament"
    )));
    assert!(!matcher.matches(&candidate("osu!.exe", "/osu/osu!.exe", "osu!.exe")));
}

#[test]
fn comm_hint_over_15_bytes_is_rejected() {
    let table = MatchTable {
        comm_hints: vec!["this-hint-is-too-long".to_string()],
        ..Default::default()
    };
    assert!(matches!(
        CompiledMatcher::compile(MatchPlatform::Linux, &table),
        Err(MatcherError::HintTooLong { .. })
    ));
}

/// F1 fix review: an older gap — an empty `comm_hints` entry (`""`)
/// passed `compile` even after the NUL-byte fix below, then froze the
/// Linux pre-filter the same way (`CommHints::new` rejects it as
/// `Empty`) and made every other plugin's own matcher refuse to
/// install as overlapping it (empty is a substring of anything).
#[test]
fn empty_comm_hint_is_rejected() {
    let table = MatchTable {
        comm_hints: vec![String::new()],
        ..Default::default()
    };
    assert!(matches!(
        CompiledMatcher::compile(MatchPlatform::Linux, &table),
        Err(MatcherError::HintEmpty)
    ));
}

/// F2fix review P3: an empty `comm_hints` entry must be refused on
/// every platform, not only Linux -- on Windows this field is
/// "accepted but unused" (length/NUL never matter there), but an empty
/// entry would still silently make `overlaps` refuse every later
/// plugin's own matcher as overlapping (empty is a substring of
/// anything), the same freeze the Linux-only check already guarded
/// against.
#[test]
fn empty_comm_hint_is_rejected_on_windows_too() {
    let table = MatchTable {
        comm_hints: vec![String::new()],
        ..Default::default()
    };
    assert!(matches!(
        CompiledMatcher::compile(MatchPlatform::Windows, &table),
        Err(MatcherError::HintEmpty)
    ));
}

/// P3 (review): same freeze risk as `linux_name_contains` — a
/// `comm_hints` entry with a NUL byte must be refused here, not later
/// by `CommHints::new`.
#[test]
fn comm_hint_with_nul_byte_is_rejected() {
    let table = MatchTable {
        comm_hints: vec!["osu\0".to_string()],
        ..Default::default()
    };
    assert!(matches!(
        CompiledMatcher::compile(MatchPlatform::Linux, &table),
        Err(MatcherError::HintContainsNul { .. })
    ));
}

/// A1b/DB revised: on Linux a `linux_name_contains` entry is a
/// userspace matcher in its own right, additive to `comm_hints` (which
/// stays pre-filter-only, unchanged) — a process whose `name` (kernel
/// `comm`) *contains* the entry matches, exactly 1.x's Linux substring
/// name rule, which exact `process_names` cannot express (e.g.
/// `osulazer.exe`'s real `comm` is truncated/varies and would never
/// equal a `process_names` entry).
#[test]
fn linux_name_contains_matches_by_substring() {
    let table = MatchTable {
        linux_name_contains: vec!["osu!".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Linux, &table).unwrap();
    assert!(matcher.matches(&candidate("osu!lazer-bin", "/home/u/osu!lazer-bin", "")));
    assert!(!matcher.matches(&candidate("unrelated", "/home/u/unrelated", "")));

    // Case-sensitive on Linux, matching `linux_name_contains`'s own case
    // rule (§6.1: compared case-sensitively, by substring containment).
    assert!(!matcher.matches(&candidate("OSU!LAZER-BIN", "/home/u/OSU!LAZER-BIN", "")));
}

/// A `comm_hints` entry stays pre-filter-only (unchanged by A1b/DB): it
/// never matches on its own, even on Linux, even when it would be a
/// substring of the candidate's name.
#[test]
fn comm_hints_never_match_on_their_own() {
    let table = MatchTable {
        comm_hints: vec!["osu!".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Linux, &table).unwrap();
    assert!(!matcher.matches(&candidate("osu!lazer-bin", "/home/u/osu!lazer-bin", "")));
}

/// §6.1: "accepted but unused on Windows" — a `linux_name_contains`
/// entry never matches on Windows, even when it would be a substring of
/// the candidate's name.
#[test]
fn windows_ignores_linux_name_contains() {
    let table = MatchTable {
        linux_name_contains: vec!["osu!".to_string()],
        ..Default::default()
    };
    let matcher = CompiledMatcher::compile(MatchPlatform::Windows, &table).unwrap();
    assert!(!matcher.matches(&candidate("osu!lazer.exe", "C:/osu/osu!lazer.exe", "")));
}

#[test]
fn linux_name_contains_over_15_bytes_is_rejected() {
    let table = MatchTable {
        linux_name_contains: vec!["this-entry-is-too-long".to_string()],
        ..Default::default()
    };
    assert!(matches!(
        CompiledMatcher::compile(MatchPlatform::Linux, &table),
        Err(MatcherError::InvalidLinuxNameContains { .. })
    ));
}

#[test]
fn linux_name_contains_empty_entry_is_rejected() {
    let table = MatchTable {
        linux_name_contains: vec![String::new()],
        ..Default::default()
    };
    assert!(matches!(
        CompiledMatcher::compile(MatchPlatform::Linux, &table),
        Err(MatcherError::InvalidLinuxNameContains { .. })
    ));
}

/// P3 (review): an entry containing a NUL byte must be refused at
/// manifest validation, not left to fail later when `CommHints::new`
/// (the kernel/CN_PROC pre-filter sync) rejects it and silently freezes
/// the Linux pre-filter for every plugin.
#[test]
fn linux_name_contains_with_nul_byte_is_rejected() {
    let table = MatchTable {
        linux_name_contains: vec!["osu\0".to_string()],
        ..Default::default()
    };
    assert!(matches!(
        CompiledMatcher::compile(MatchPlatform::Linux, &table),
        Err(MatcherError::InvalidLinuxNameContains { .. })
    ));
}

#[test]
fn static_overlap_on_equal_process_names() {
    let a = table(&["osu!.exe"]);
    let b = table(&["osu!.exe"]);
    assert!(overlaps(MatchPlatform::Windows, &a, &b));
}

/// §6.1's pre-A1b D18 hint rule, restored: a `comm_hints` or
/// `linux_name_contains` entry overlaps the other plugin's exact
/// `process_names` entry when it is equal to, or a substring of, that
/// name.
#[test]
fn comm_hint_substring_of_process_name_overlaps() {
    let a = MatchTable {
        comm_hints: vec!["osu!".to_string()],
        ..Default::default()
    };
    let b = table(&["wine-osu!.exe"]);
    assert!(overlaps(MatchPlatform::Linux, &a, &b));
    assert!(overlaps(MatchPlatform::Linux, &b, &a));
}

/// `linux_name_contains` gets the exact same substring-of-`process_names`
/// overlap rule as `comm_hints` (it is treated like a hint for this
/// static check).
#[test]
fn linux_name_contains_substring_of_process_name_overlaps() {
    let a = MatchTable {
        linux_name_contains: vec!["osu!".to_string()],
        ..Default::default()
    };
    let b = table(&["wine-osu!.exe"]);
    assert!(overlaps(MatchPlatform::Linux, &a, &b));
    assert!(overlaps(MatchPlatform::Linux, &b, &a));
}

/// Two hints with no containment either way never statically overlap,
/// even though a real process name could in principle contain both
/// (review: the merged-length rule tried this and rejected almost every
/// real plugin pair, e.g. `"osu!"` next to an unrelated `"mygame"`).
#[test]
fn disjoint_hints_do_not_statically_overlap() {
    let a = MatchTable {
        comm_hints: vec!["osu!".to_string()],
        ..Default::default()
    };
    let b = MatchTable {
        linux_name_contains: vec!["mygame".to_string()],
        ..Default::default()
    };
    assert!(!overlaps(MatchPlatform::Linux, &a, &b));
    assert!(!overlaps(MatchPlatform::Linux, &b, &a));
}

#[test]
fn static_overlap_absent_for_disjoint_matchers() {
    let a = table(&["osu!.exe"]);
    let b = table(&["other-game.exe"]);
    assert!(!overlaps(MatchPlatform::Windows, &a, &b));
}

/// `"osu!"` and `"lazer"` share no containment either way, so they do
/// not statically overlap -- but a real process named `osu!lazer`
/// matches both plugins' `linux_name_contains` substring rule at
/// runtime, which is exactly what the runtime rule (not the static
/// check) exists to catch: both pass `register`, and `match_candidate`
/// reports the earlier-installed plugin as the winner and the later one
/// as shadowed rather than refusing either at install time.
#[test]
fn disjoint_hints_still_collide_at_runtime_on_a_shared_comm() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    assert!(!overlaps(
        MatchPlatform::Linux,
        &MatchTable {
            linux_name_contains: vec!["osu!".to_string()],
            ..Default::default()
        },
        &MatchTable {
            linux_name_contains: vec!["lazer".to_string()],
            ..Default::default()
        }
    ));
    engine
        .register(
            "vosu.first".to_string(),
            MatchTable {
                linux_name_contains: vec!["osu!".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    engine
        .register(
            "vosu.second".to_string(),
            MatchTable {
                linux_name_contains: vec!["lazer".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let outcome = engine.match_candidate(&candidate("osu!lazer", "/games/osu!lazer", ""));
    assert_eq!(outcome.winner.as_deref(), Some("vosu.first"));
    assert_eq!(outcome.shadowed, vec!["vosu.second".to_string()]);
}

#[test]
fn register_refuses_overlapping_second_plugin() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register("vosu.osu".to_string(), table(&["osu!.exe"]))
        .unwrap();
    let err = engine
        .register("vosu.other".to_string(), table(&["osu!.exe"]))
        .unwrap_err();
    assert!(matches!(
        err,
        RegisterError::Overlap(OverlapError {
            existing,
            candidate,
        }) if existing == "vosu.osu" && candidate == "vosu.other"
    ));
}

#[test]
fn engine_is_empty_until_first_register() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    assert!(engine.is_empty());
    engine
        .register("vosu.osu".to_string(), table(&["osu!.exe"]))
        .unwrap();
    assert!(!engine.is_empty());
}

#[test]
fn prefilter_union_includes_short_process_names_and_comm_hints_deduplicated() {
    // §6.1: the pre-filter is short `process_names` *plus*
    // `comm_hints`, not `comm_hints` alone (P1) -- and a plugin whose
    // `comm_hints` differ from its `process_names` contributes both.
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register(
            "vosu.first".to_string(),
            MatchTable {
                process_names: vec!["osu!.exe".to_string()],
                // A repeated hint within one plugin's own table
                // exercises dedup without triggering D18's
                // cross-plugin overlap rejection below.
                comm_hints: vec!["osu!".to_string(), "osu!".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    engine
        .register(
            "vosu.second".to_string(),
            MatchTable {
                comm_hints: vec!["other-game".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        engine.prefilter(),
        Prefilter::Names(vec![
            "osu!".to_string(),
            "osu!.exe".to_string(),
            "other-game".to_string(),
        ])
    );
}

/// Review item 5: `linux_name_contains` feeds the unified pre-filter
/// exactly like `comm_hints` (A1b/DB revised) — a single-plugin check,
/// so no cross-plugin overlap is at stake.
#[test]
fn prefilter_union_includes_linux_name_contains() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register(
            "vosu.osu".to_string(),
            MatchTable {
                process_names: vec!["osu!.exe".to_string()],
                linux_name_contains: vec!["osu!".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        engine.prefilter(),
        Prefilter::Names(vec!["osu!".to_string(), "osu!.exe".to_string()])
    );
}

#[test]
fn prefilter_accepts_a_plugin_with_only_process_names() {
    // The §8 manifest example (`osu!.exe` as the sole matcher): an
    // empty `comm_hints` must not make the pre-filter match nothing
    // (P1) -- the short `process_names` entry alone is enough.
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register("vosu.osu".to_string(), table(&["osu!.exe"]))
        .unwrap();
    assert_eq!(
        engine.prefilter(),
        Prefilter::Names(vec!["osu!.exe".to_string()])
    );
}

#[test]
fn prefilter_widens_for_a_path_pattern_plugin_with_no_usable_hint() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register(
            "vosu.paths".to_string(),
            MatchTable {
                process_path_patterns: vec![".*/osu/.*".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(engine.prefilter(), Prefilter::Unfiltered);
}

#[test]
fn prefilter_stays_narrow_when_a_path_pattern_plugin_also_has_a_hint() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register(
            "vosu.paths".to_string(),
            MatchTable {
                process_path_patterns: vec![".*/osu/.*".to_string()],
                comm_hints: vec!["osu!".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        engine.prefilter(),
        Prefilter::Names(vec!["osu!".to_string()])
    );
}

#[test]
fn prefilter_excludes_a_process_names_entry_over_15_bytes_on_linux() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register("vosu.long".to_string(), table(&["this-name-is-sixteen!"]))
        .unwrap();
    assert_eq!(engine.prefilter(), Prefilter::Names(Vec::new()));
}

#[test]
fn prefilter_on_windows_ignores_comm_hints_and_the_15_byte_limit() {
    // §6.1: `comm_hints` is "accepted but unused on Windows", and
    // Windows has no kernel `comm` field width to respect.
    let mut engine = MatcherEngine::new(MatchPlatform::Windows);
    engine
        .register(
            "vosu.osu".to_string(),
            MatchTable {
                process_names: vec!["this-name-is-sixteen!".to_string()],
                comm_hints: vec!["unused-on-windows".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        engine.prefilter(),
        Prefilter::Names(vec!["this-name-is-sixteen!".to_string()])
    );
}

#[test]
fn prefilter_on_windows_widens_for_any_path_pattern_plugin() {
    // Windows has no hint concept at all, so a path-pattern-only
    // plugin always widens there, unlike Linux's own
    // `prefilter_stays_narrow_when_a_path_pattern_plugin_also_has_a_hint`.
    let mut engine = MatcherEngine::new(MatchPlatform::Windows);
    engine
        .register(
            "vosu.paths".to_string(),
            MatchTable {
                process_path_patterns: vec![".*/osu/.*".to_string()],
                comm_hints: vec!["osu!".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(engine.prefilter(), Prefilter::Unfiltered);
}

#[test]
fn generation_bumps_on_register_and_unregister_but_not_on_a_no_op() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    assert_eq!(engine.generation(), 0);
    engine
        .register("vosu.osu".to_string(), table(&["osu!.exe"]))
        .unwrap();
    assert_eq!(engine.generation(), 1);
    engine.unregister("vosu.absent");
    assert_eq!(
        engine.generation(),
        1,
        "unregistering an unknown id is a no-op"
    );
    engine.unregister("vosu.osu");
    assert_eq!(engine.generation(), 2);
}

#[test]
fn runtime_match_prefers_earlier_installed_plugin_and_reports_shadowed() {
    // Only path patterns can collide at runtime despite passing static
    // validation (D18: path/command-line overlap is undecidable).
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register(
            "vosu.first".to_string(),
            MatchTable {
                process_path_patterns: vec![".*/osu/.*".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    engine
        .register(
            "vosu.second".to_string(),
            MatchTable {
                process_path_patterns: vec![".*/osu/.*".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
    let outcome = engine.match_candidate(&candidate("osu!.exe", "/games/osu/osu!.exe", ""));
    assert_eq!(outcome.winner.as_deref(), Some("vosu.first"));
    assert_eq!(outcome.shadowed, vec!["vosu.second".to_string()]);
}

#[test]
fn runtime_match_is_none_when_nothing_matches() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register("vosu.osu".to_string(), table(&["osu!.exe"]))
        .unwrap();
    let outcome = engine.match_candidate(&candidate("unrelated", "/bin/unrelated", ""));
    assert_eq!(outcome.winner, None);
    assert!(outcome.shadowed.is_empty());
}

#[test]
fn register_refuses_when_combined_prefilter_total_exceeds_256() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    let hints: Vec<String> = (0..256).map(|i| format!("h{i}")).collect();
    engine
        .register(
            "vosu.first".to_string(),
            MatchTable {
                comm_hints: hints,
                ..Default::default()
            },
        )
        .unwrap();
    let err = engine
        .register(
            "vosu.second".to_string(),
            MatchTable {
                comm_hints: vec!["extra".to_string()],
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        RegisterError::PreFilterLimit(PreFilterLimitError { candidate, total })
            if candidate == "vosu.second" && total == 257
    ));
}

/// Review item 5: `linux_name_contains` counts toward the combined
/// 256-entry pre-filter bound too, not just `comm_hints`/`process_names`.
#[test]
fn register_refuses_when_linux_name_contains_pushes_prefilter_over_256() {
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    let hints: Vec<String> = (0..256).map(|i| format!("h{i}")).collect();
    engine
        .register(
            "vosu.first".to_string(),
            MatchTable {
                comm_hints: hints,
                ..Default::default()
            },
        )
        .unwrap();
    let err = engine
        .register(
            "vosu.second".to_string(),
            MatchTable {
                linux_name_contains: vec!["extra".to_string()],
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        RegisterError::PreFilterLimit(PreFilterLimitError { candidate, total })
            if candidate == "vosu.second" && total == 257
    ));
}

#[test]
fn register_ignores_the_prefilter_limit_on_windows() {
    // The pre-filter is Linux-only (§6.1): Windows registration never
    // refuses on this ground, however many hints are declared.
    let mut engine = MatcherEngine::new(MatchPlatform::Windows);
    let hints: Vec<String> = (0..300).map(|i| format!("h{i}")).collect();
    engine
        .register(
            "vosu.osu".to_string(),
            MatchTable {
                comm_hints: hints,
                ..Default::default()
            },
        )
        .unwrap();
}

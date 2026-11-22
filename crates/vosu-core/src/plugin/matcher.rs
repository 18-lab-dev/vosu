//! Process matching (`docs/v2/plugin-abi.md` §6.1): compiles a game
//! plugin's `[match]` table (`vosu_plugin_api::MatchTable`) into a runtime
//! matcher, validates the D18 static overlap rule at install time, and
//! applies the D18 runtime tie-break (earlier installed plugin wins) over
//! synthetic process records. Discovery wiring (the real process list, the
//! Linux conditional fallback, the kernel/CN_PROC pre-filter) is a later
//! milestone (phase (d)); this module only reproduces the userspace
//! matching semantics §6.1 defines.

use std::fmt;

use regex::Regex;
use vosu_plugin_api::MatchTable;

/// An installed plugin's stable identity for ordering and error messages.
pub type PluginId = String;

/// Host target platform for §6.1's case and path-separator rules. Matching
/// behavior differs by platform, so a [`CompiledMatcher`] is compiled for
/// one platform and is not reusable across both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchPlatform {
    Windows,
    Linux,
}

/// One process to match against, exactly as discovery would observe it
/// (synthetic in M1 tests; `docs/v2/implementation-plan.md` M1 Exit).
#[derive(Debug, Clone)]
pub struct CandidateProcess {
    /// The executable file name only (no directory), e.g. `osu!.exe`.
    pub name: String,
    /// The full executable path.
    pub path: String,
    /// The joined argv.
    pub command_line: String,
}

/// Every way a `[match]` table fails to compile into a runtime matcher.
#[derive(Debug)]
pub enum MatcherError {
    /// `process_path_patterns` or `command_line_patterns` entry is not a
    /// valid `regex` crate pattern once full-match anchored (§6.1).
    InvalidPattern {
        field: &'static str,
        pattern: String,
        source: regex::Error,
    },
    /// A `comm_hints` entry is longer than 15 bytes, the kernel/CN_PROC
    /// `comm` field width (§6.1, D18).
    HintTooLong { hint: String },
}

impl fmt::Display for MatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPattern {
                field,
                pattern,
                source,
            } => write!(f, "{field} pattern {pattern:?} does not compile: {source}"),
            Self::HintTooLong { hint } => {
                write!(f, "comm_hints entry {hint:?} exceeds 15 bytes")
            }
        }
    }
}

impl std::error::Error for MatcherError {}

/// A compiled `[match]` table, ready to test candidate processes against
/// (§6.1).
pub struct CompiledMatcher {
    process_names: Vec<String>,
    process_path_patterns: Vec<Regex>,
    command_line_patterns: Vec<Regex>,
    platform: MatchPlatform,
}

/// Wraps a path-matching pattern in `^(?:…)$` (full-match anchoring, §6.1)
/// and, for a case-insensitive platform, an inline `(?i)` flag.
fn anchor(pattern: &str, case_insensitive: bool) -> String {
    let normalized = normalize_separators(pattern);
    if case_insensitive {
        format!("(?i)^(?:{normalized})$")
    } else {
        format!("^(?:{normalized})$")
    }
}

/// Normalizes path separators to `/` (§6.1: "Path separators in both the
/// candidate path and the pattern are normalized to `/` before matching").
fn normalize_separators(value: &str) -> String {
    value.replace('\\', "/")
}

fn case_insensitive_names(platform: MatchPlatform) -> bool {
    matches!(platform, MatchPlatform::Windows)
}

fn names_equal(platform: MatchPlatform, a: &str, b: &str) -> bool {
    if case_insensitive_names(platform) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Substring containment under the platform's case rule, used by the D18
/// overlap check: "a `comm_hints` entry of one is equal to, or a substring
/// of, any `process_names`/`comm_hints` entry of the other".
fn contains_under(platform: MatchPlatform, haystack: &str, needle: &str) -> bool {
    if case_insensitive_names(platform) {
        haystack
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    } else {
        haystack.contains(needle)
    }
}

impl CompiledMatcher {
    /// Compiles `table` for `platform`. `comm_hints` only fills the
    /// kernel/CN_PROC pre-filter in production (§6.1): this matcher
    /// validates its length here but never stores or consults it in
    /// [`Self::matches`] itself, matching "full matching … always runs in
    /// userspace against the candidate's full path and argv regardless of
    /// what the pre-filter already narrowed". The static D18 overlap check
    /// ([`overlaps`]) reads `comm_hints` from the raw [`MatchTable`]s
    /// instead, before compilation.
    pub fn compile(platform: MatchPlatform, table: &MatchTable) -> Result<Self, MatcherError> {
        for hint in &table.comm_hints {
            if hint.len() > 15 {
                return Err(MatcherError::HintTooLong { hint: hint.clone() });
            }
        }
        let compile_patterns =
            |field: &'static str, patterns: &[String], case_insensitive: bool| {
                patterns
                    .iter()
                    .map(|pattern| {
                        Regex::new(&anchor(pattern, case_insensitive)).map_err(|source| {
                            MatcherError::InvalidPattern {
                                field,
                                pattern: pattern.clone(),
                                source,
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            };
        let process_path_patterns = compile_patterns(
            "process_path_patterns",
            &table.process_path_patterns,
            case_insensitive_names(platform),
        )?;
        // `command_line_patterns` has no documented case rule (§6.1 names
        // only `process_names`/`process_path_patterns`), so argv patterns
        // compile exactly as written.
        let command_line_patterns =
            compile_patterns("command_line_patterns", &table.command_line_patterns, false)?;
        Ok(Self {
            process_names: table.process_names.clone(),
            process_path_patterns,
            command_line_patterns,
            platform,
        })
    }

    /// Whether `candidate` is accepted by this matcher (§6.1): an exact
    /// `process_names` match, a full-match `process_path_patterns` match
    /// against the normalized path, or a full-match `command_line_patterns`
    /// match against the joined argv.
    #[must_use]
    pub fn matches(&self, candidate: &CandidateProcess) -> bool {
        if self
            .process_names
            .iter()
            .any(|name| names_equal(self.platform, name, &candidate.name))
        {
            return true;
        }
        let path = normalize_separators(&candidate.path);
        if self
            .process_path_patterns
            .iter()
            .any(|re| re.is_match(&path))
        {
            return true;
        }
        self.command_line_patterns
            .iter()
            .any(|re| re.is_match(&candidate.command_line))
    }
}

/// D18's statically decidable overlap rule: equal `process_names` entries,
/// or a `comm_hints` entry of one equal to or a substring of a
/// `process_names`/`comm_hints` entry of the other (regex overlap on
/// `process_path_patterns`/`command_line_patterns` is undecidable in
/// general and is never checked here, `docs/v2/plugin-abi.md:121`).
#[must_use]
pub fn overlaps(platform: MatchPlatform, a: &MatchTable, b: &MatchTable) -> bool {
    if a.process_names
        .iter()
        .any(|x| b.process_names.iter().any(|y| names_equal(platform, x, y)))
    {
        return true;
    }
    let hints_hit = |hints: &[String], names: &[String], other_hints: &[String]| {
        hints.iter().any(|hint| {
            names.iter().any(|name| {
                names_equal(platform, hint, name) || contains_under(platform, name, hint)
            }) || other_hints.iter().any(|other| {
                names_equal(platform, hint, other)
                    || contains_under(platform, other, hint)
                    || contains_under(platform, hint, other)
            })
        })
    };
    hints_hit(&a.comm_hints, &b.process_names, &b.comm_hints)
        || hints_hit(&b.comm_hints, &a.process_names, &a.comm_hints)
}

/// A plugin whose matcher statically overlaps an already-registered one
/// (D18): the host refuses to install/start the second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlapError {
    pub existing: PluginId,
    pub candidate: PluginId,
}

impl fmt::Display for OverlapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "matcher for {:?} overlaps the already-installed {:?} (D18)",
            self.candidate, self.existing
        )
    }
}

impl std::error::Error for OverlapError {}

/// The result of matching one candidate process against every enabled
/// plugin's matcher, in install order (D18's runtime rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchOutcome {
    /// The earliest installed plugin whose matcher accepted the process,
    /// if any.
    pub winner: Option<PluginId>,
    /// Every other, later-installed plugin whose matcher also accepted the
    /// process: D18 says the host "logs once" when this is non-empty; this
    /// module only reports the fact; the caller decides how to log it.
    pub shadowed: Vec<PluginId>,
}

/// Every enabled plugin's compiled matcher, kept in install order, so
/// [`Self::register`] can enforce D18's static overlap rule and
/// [`Self::match_candidate`] can apply D18's runtime tie-break.
pub struct MatcherEngine {
    platform: MatchPlatform,
    entries: Vec<(PluginId, MatchTable, CompiledMatcher)>,
}

impl MatcherEngine {
    #[must_use]
    pub fn new(platform: MatchPlatform) -> Self {
        Self {
            platform,
            entries: Vec::new(),
        }
    }

    /// Compiles and installs `table` for `id`, appended after every
    /// already-registered plugin (install order). Refuses with
    /// [`OverlapError`] when `table` statically overlaps (D18) an
    /// already-registered plugin's table; refuses with [`MatcherError`]
    /// when `table` itself fails to compile. Neither failure installs the
    /// plugin.
    pub fn register(&mut self, id: PluginId, table: MatchTable) -> Result<(), RegisterError> {
        for (existing_id, existing_table, _) in &self.entries {
            if overlaps(self.platform, existing_table, &table) {
                return Err(RegisterError::Overlap(OverlapError {
                    existing: existing_id.clone(),
                    candidate: id,
                }));
            }
        }
        let compiled =
            CompiledMatcher::compile(self.platform, &table).map_err(RegisterError::Matcher)?;
        self.entries.push((id, table, compiled));
        Ok(())
    }

    /// Matches `candidate` against every registered plugin, in install
    /// order (D18's runtime rule: the earliest installed matching plugin
    /// wins; every other match is `shadowed` for the caller to log once).
    #[must_use]
    pub fn match_candidate(&self, candidate: &CandidateProcess) -> MatchOutcome {
        let mut hits = self
            .entries
            .iter()
            .filter(|(_, _, matcher)| matcher.matches(candidate))
            .map(|(id, _, _)| id.clone());
        let winner = hits.next();
        let shadowed = hits.collect();
        MatchOutcome { winner, shadowed }
    }
}

/// Everything [`MatcherEngine::register`] can fail with.
#[derive(Debug)]
pub enum RegisterError {
    Overlap(OverlapError),
    Matcher(MatcherError),
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overlap(e) => write!(f, "{e}"),
            Self::Matcher(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RegisterError {}

#[cfg(test)]
mod tests {
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
        let windows =
            CompiledMatcher::compile(MatchPlatform::Windows, &table(&["osu!.exe"])).unwrap();
        assert!(windows.matches(&candidate("OSU!.EXE", "C:/osu/osu!.exe", "")));

        let linux = CompiledMatcher::compile(MatchPlatform::Linux, &table(&["osu!.exe"])).unwrap();
        assert!(!linux.matches(&candidate("OSU!.EXE", "/home/user/osu!.exe", "")));
        assert!(linux.matches(&candidate("osu!.exe", "/home/user/osu!.exe", "")));
    }

    #[test]
    fn path_patterns_are_full_match_anchored_and_normalize_separators() {
        // No backslash escapes in the pattern itself: §6.1's path
        // separator normalization applies the same naive `\`→`/` rewrite
        // to the pattern text, so a path pattern must spell path
        // separators literally rather than escape regex metacharacters
        // with `\`.
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

    #[test]
    fn static_overlap_on_equal_process_names() {
        let a = table(&["osu!.exe"]);
        let b = table(&["osu!.exe"]);
        assert!(overlaps(MatchPlatform::Windows, &a, &b));
    }

    #[test]
    fn static_overlap_on_comm_hint_substring_of_process_name() {
        let a = MatchTable {
            comm_hints: vec!["osu!".to_string()],
            ..Default::default()
        };
        let b = table(&["osu!.exe"]);
        assert!(overlaps(MatchPlatform::Linux, &a, &b));
        assert!(overlaps(MatchPlatform::Linux, &b, &a));
    }

    #[test]
    fn static_overlap_absent_for_disjoint_matchers() {
        let a = table(&["osu!.exe"]);
        let b = table(&["other-game.exe"]);
        assert!(!overlaps(MatchPlatform::Windows, &a, &b));
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
}

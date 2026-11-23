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
    let normalized = normalize_pattern_separators(pattern);
    if case_insensitive {
        format!("(?i)^(?:{normalized})$")
    } else {
        format!("^(?:{normalized})$")
    }
}

/// Normalizes path separators to `/` in an actual candidate path (§6.1:
/// "Path separators in both the candidate path and the pattern are
/// normalized to `/` before matching"). A real filesystem path has no
/// regex escapes, so every `\` is literally a separator here.
fn normalize_path_separators(value: &str) -> String {
    value.replace('\\', "/")
}

/// Normalizes path separators to `/` in a *pattern's* source text. Unlike
/// [`normalize_path_separators`], this must not touch the pattern's own
/// regex escapes (`\.`, `\(`, `\d`, …) — only a literal escaped backslash
/// (`\\`, the regex spelling of one literal `\`) is a path separator here.
/// Rewriting every `\` naively would corrupt `\.` into `/.`, `\(x86\)`
/// into `/(x86/)`, and so on, silently changing what a valid `regex`
/// crate pattern matches (§6.1 "Patterns use Rust `regex` crate syntax").
fn normalize_pattern_separators(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.clone().next() {
            Some('\\') => {
                // `\\`: an escaped literal backslash, i.e. a path
                // separator written in regex syntax.
                chars.next();
                out.push('/');
            }
            Some(next) => {
                // Any other escape (`\.`, `\(`, `\d`, …): pass both
                // characters through unchanged.
                chars.next();
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out
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
        let path = normalize_path_separators(&candidate.path);
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

    /// Whether this matcher can only ever accept a process by an exact
    /// `process_names` entry — no `process_path_patterns`/
    /// `command_line_patterns` that could accept a process regardless of
    /// its executable name. The Windows enumeration-cost pre-filter
    /// ([`NamePrefilter`], M1 §4 phase d2b unit c) is only sound across
    /// *every* registered plugin when this holds for all of them: one
    /// plugin matching by path/command-line pattern alone can accept a
    /// process whose name was never in any `process_names` list.
    #[must_use]
    fn names_only(&self) -> bool {
        self.process_path_patterns.is_empty() && self.command_line_patterns.is_empty()
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

/// The enumeration-cost pre-filter (§6.1), unified across both released
/// targets (M1 §4 phase d2b): Windows's own "which pids are worth
/// opening at all" narrowing and Linux's kernel/CN_PROC comm pre-filter
/// are the same concept — a deduplicated set of names a process's bare
/// executable name (Windows) or `comm` (Linux, via substring) must hit
/// before anything else about it is read — so one type and one producer
/// ([`MatcherEngine::prefilter`]) serve both, and the widening rule is
/// identical in shape even though what counts as a "usable hint" differs
/// per platform (that method's own doc comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prefilter {
    /// At least one registered plugin matches by path or command-line
    /// pattern with no usable hint of its own: every process must still
    /// be read in full (§6.1's widening rule).
    Unfiltered,
    /// Every registered plugin either matches by `process_names` only,
    /// or declares a usable hint for its path/command-line patterns:
    /// this is the deduplicated union of those hint names (unsorted
    /// order not guaranteed to callers; only equality/membership is a
    /// contract).
    Names(Vec<String>),
}

impl Prefilter {
    /// Whether `name` (an observed process's bare executable file name
    /// on Windows, or its `comm` on Linux) can possibly match some
    /// registered plugin, under `platform`'s own case rule (§6.1:
    /// Windows name matching is case-insensitive, `names_equal`).
    /// Cheaply reproduces [`CompiledMatcher::matches`]'s own
    /// `process_names` check without needing a full [`CandidateProcess`]
    /// — a source consults this before reading anything else about the
    /// process at all.
    #[must_use]
    pub fn allows(&self, platform: MatchPlatform, name: &str) -> bool {
        match self {
            Self::Unfiltered => true,
            Self::Names(names) => names
                .iter()
                .any(|candidate| names_equal(platform, candidate, name)),
        }
    }
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

/// The combined kernel/CN_PROC pre-filter bound (§6.1, D18): `process_names`
/// entries that fit its 15-byte `comm` field width, plus every `comm_hints`
/// entry, across every enabled Linux game plugin.
const PREFILTER_LIMIT: usize = 256;

/// How many of `table`'s entries occupy the combined pre-filter (§6.1): a
/// `process_names` entry that is itself short enough to match `comm`
/// directly, plus every `comm_hints` entry.
fn prefilter_entries(table: &MatchTable) -> usize {
    table
        .process_names
        .iter()
        .filter(|name| name.len() <= 15)
        .count()
        + table.comm_hints.len()
}

/// Whether `table`'s own matcher has a hint the pre-filter can narrow
/// by, used only by [`MatcherEngine::prefilter`]'s widening rule
/// (§6.1): on Linux, a `comm_hints` entry or a `process_names` entry
/// short enough to match the kernel `comm` field; on Windows, always
/// `false` — Windows has no `comm`/CN_PROC pre-filter at all (§6.1:
/// "accepted but unused on Windows"), so a plugin there never has a
/// hint to fall back on once it declares a path/command-line pattern.
fn has_usable_hint(linux: bool, table: &MatchTable) -> bool {
    linux
        && (!table.comm_hints.is_empty() || table.process_names.iter().any(|name| name.len() <= 15))
}

/// A plugin whose `comm_hints`/short `process_names` would push the
/// combined Linux pre-filter past [`PREFILTER_LIMIT`] (§6.1, D18): refused
/// at manifest validation, in enable order, never silently dropped or
/// truncated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreFilterLimitError {
    pub candidate: PluginId,
    /// The combined total this registration would reach.
    pub total: usize,
}

impl fmt::Display for PreFilterLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "installing {:?} would push the combined pre-filter to {} entries, over the {}-entry bound (§6.1, D18)",
            self.candidate, self.total, PREFILTER_LIMIT
        )
    }
}

impl std::error::Error for PreFilterLimitError {}

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
    /// Bumped on every [`Self::register`]/[`Self::unregister`] that
    /// actually changes `entries` (P2: a plugin installed or torn down
    /// after discovery already tracked a process must still get a
    /// chance to match it, `DiscoveryService`'s own re-offer-on-change
    /// wiring).
    generation: u64,
}

impl MatcherEngine {
    #[must_use]
    pub fn new(platform: MatchPlatform) -> Self {
        Self {
            platform,
            entries: Vec::new(),
            generation: 0,
        }
    }

    /// Compiles and installs `table` for `id`, appended after every
    /// already-registered plugin (install order). Refuses with
    /// [`OverlapError`] when `table` statically overlaps (D18) an
    /// already-registered plugin's table; refuses with
    /// [`PreFilterLimitError`] when installing it would push the combined
    /// Linux pre-filter past [`PREFILTER_LIMIT`] (§6.1, D18); refuses with
    /// [`MatcherError`] when `table` itself fails to compile. No failure
    /// installs the plugin.
    pub fn register(&mut self, id: PluginId, table: MatchTable) -> Result<(), RegisterError> {
        for (existing_id, existing_table, _) in &self.entries {
            if overlaps(self.platform, existing_table, &table) {
                return Err(RegisterError::Overlap(OverlapError {
                    existing: existing_id.clone(),
                    candidate: id,
                }));
            }
        }
        if self.platform == MatchPlatform::Linux {
            let total: usize = self
                .entries
                .iter()
                .map(|(_, existing_table, _)| prefilter_entries(existing_table))
                .sum::<usize>()
                + prefilter_entries(&table);
            if total > PREFILTER_LIMIT {
                return Err(RegisterError::PreFilterLimit(PreFilterLimitError {
                    candidate: id,
                    total,
                }));
            }
        }
        let compiled =
            CompiledMatcher::compile(self.platform, &table).map_err(RegisterError::Matcher)?;
        self.entries.push((id, table, compiled));
        self.generation += 1;
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

    /// `id`'s position in install order, the Focus rule's "earliest-
    /// attached plugin" tie-break (`docs/v2/api-v1.md` "Focus rule"):
    /// `usize::MAX` for a plugin this engine never registered (a game
    /// plugin with no `[match]` table never matches a process at all, so
    /// it can never own a session the Focus rule needs to rank).
    #[must_use]
    pub fn install_order_of(&self, id: &str) -> usize {
        self.entries
            .iter()
            .position(|(existing, _, _)| existing == id)
            .unwrap_or(usize::MAX)
    }

    /// Removes `id`'s matcher, installed by [`Self::register`] (P2-5): a
    /// plugin whose own `create` then fails, or whose instance is
    /// destroyed losing a `destroy_all` shutdown race, must not keep a
    /// `[match]` table with no live instance behind it — `process_appeared`
    /// would otherwise keep handing every matching process to a plugin id
    /// nothing answers for, hiding that session from every later-installed,
    /// shadowed plugin forever. A no-op for an `id` this engine never
    /// registered (or already unregistered).
    pub fn unregister(&mut self, id: &str) {
        let before = self.entries.len();
        self.entries.retain(|(existing, _, _)| existing != id);
        if self.entries.len() != before {
            self.generation += 1;
        }
    }

    /// Whether any plugin is currently registered — the discovery
    /// service's own "zero cost when unused" gate
    /// (`docs/v2/implementation-plan.md` M1 §4, phase d2b): no registered
    /// plugin can ever match a process, so there is nothing for the
    /// service to usefully watch for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// This engine's own install-set generation (P2): bumped by every
    /// [`Self::register`]/[`Self::unregister`] that actually changes
    /// which plugins are installed. `DiscoveryService` re-reads this on
    /// every reconciliation pass and, on a change, re-offers every
    /// already-tracked process to [`PluginHost::process_appeared`] — a
    /// plugin installed (or re-installed) after discovery already saw a
    /// process must still get a chance to match it, and
    /// `PluginHost::process_appeared` is idempotent for one it already
    /// attached.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The unified enumeration-cost pre-filter (§6.1, [`Prefilter`]'s own
    /// doc comment): [`Prefilter::Unfiltered`] the moment any registered
    /// plugin's matcher is not [`CompiledMatcher::names_only`] *and* has
    /// no usable hint of its own (`has_usable_hint`); otherwise the
    /// deduplicated union of every registered plugin's hint names. A
    /// plugin's hint names are: on Linux, its `process_names` entries
    /// short enough to match the kernel `comm` field (`≤15` bytes) plus
    /// every `comm_hints` entry (§6.1); on Windows, every `process_names`
    /// entry verbatim (Windows has no `comm` field width limit and no
    /// `comm_hints` concept at all, so a plugin there either matches by
    /// name alone or widens the whole pre-filter the moment it also
    /// declares a path/command-line pattern — exactly 1.x's own
    /// name-filter-or-nothing behavior).
    #[must_use]
    pub fn prefilter(&self) -> Prefilter {
        let linux = self.platform == MatchPlatform::Linux;
        let widens = self
            .entries
            .iter()
            .any(|(_, table, compiled)| !compiled.names_only() && !has_usable_hint(linux, table));
        if widens {
            return Prefilter::Unfiltered;
        }
        let mut names: Vec<String> = self
            .entries
            .iter()
            .flat_map(|(_, table, _)| {
                table
                    .process_names
                    .iter()
                    .filter(move |name| !linux || name.len() <= 15)
                    .cloned()
                    .chain(if linux {
                        table.comm_hints.clone()
                    } else {
                        Vec::new()
                    })
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        Prefilter::Names(names)
    }
}

/// The running host's own [`MatchPlatform`] (§6.1's two released
/// targets, `docs/v2/plugin-abi.md:15`).
#[must_use]
pub const fn host_platform() -> MatchPlatform {
    #[cfg(windows)]
    {
        MatchPlatform::Windows
    }
    #[cfg(not(windows))]
    {
        MatchPlatform::Linux
    }
}

/// Everything [`MatcherEngine::register`] can fail with.
#[derive(Debug)]
pub enum RegisterError {
    Overlap(OverlapError),
    PreFilterLimit(PreFilterLimitError),
    Matcher(MatcherError),
}

impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overlap(e) => write!(f, "{e}"),
            Self::PreFilterLimit(e) => write!(f, "{e}"),
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
}

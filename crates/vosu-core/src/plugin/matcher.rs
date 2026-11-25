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
    /// Portable packaging validation is owned exclusively by the SDK.
    Document(vosu_plugin_api::manifest::ValidationError),
}

impl fmt::Display for MatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPattern {
                field,
                pattern,
                source,
            } => write!(f, "{field} pattern {pattern:?} does not compile: {source}"),
            Self::Document(error) => error.fmt(f),
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
    /// Linux only (empty on Windows, where it is "accepted but unused",
    /// §6.1): a `linux_name_contains` entry is a userspace matcher in
    /// its own right (A1b/DB revised) — a process whose `name` (its
    /// kernel `comm` value) *contains* the entry matches. `comm_hints`
    /// keeps its unchanged, pre-filter-only meaning and is never stored
    /// here.
    linux_name_contains: Vec<String>,
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
/// overlap check: a `comm_hints`/`linux_name_contains` entry of one is
/// equal to, or a substring of, a `process_names`/`comm_hints`/
/// `linux_name_contains` entry of the other.
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
    /// [`Self::matches`] itself — unchanged by A1b/DB. On Linux,
    /// `linux_name_contains` fills both the pre-filter *and*
    /// [`Self::matches`]'s own substring check against `candidate.name`
    /// (A1b/DB revised, additive field: changing `comm_hints`'s own
    /// meaning would be a breaking change for an existing plugin that
    /// declares it only for the pre-filter, §1); on Windows it is
    /// "accepted but unused" (§6.1) — stored but never consulted. The
    /// static D18 overlap check ([`overlaps`]) reads both fields from
    /// the raw [`MatchTable`]s instead, before compilation.
    pub fn compile(platform: MatchPlatform, table: &MatchTable) -> Result<Self, MatcherError> {
        table.validate_document().map_err(MatcherError::Document)?;
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
            linux_name_contains: table.linux_name_contains.clone(),
            platform,
        })
    }

    /// Whether `candidate` is accepted by this matcher (§6.1): an exact
    /// `process_names` match, a Linux `linux_name_contains` substring
    /// match against `candidate.name` (A1b/DB revised; never checked on
    /// Windows, where `linux_name_contains` is accepted but unused), a
    /// full-match `process_path_patterns` match against the normalized
    /// path, or a full-match `command_line_patterns` match against the
    /// joined argv. `comm_hints` is never consulted here — it stays
    /// pre-filter only, unchanged.
    #[must_use]
    pub fn matches(&self, candidate: &CandidateProcess) -> bool {
        if self
            .process_names
            .iter()
            .any(|name| names_equal(self.platform, name, &candidate.name))
        {
            return true;
        }
        if self.platform == MatchPlatform::Linux
            && self
                .linux_name_contains
                .iter()
                .any(|entry| candidate.name.contains(entry.as_str()))
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
    /// ([`Prefilter`], M1 §4 phase d2b unit c) is only sound across
    /// *every* registered plugin when this holds for all of them: one
    /// plugin matching by path/command-line pattern alone can accept a
    /// process whose name was never in any `process_names` list.
    #[must_use]
    fn names_only(&self) -> bool {
        self.process_path_patterns.is_empty() && self.command_line_patterns.is_empty()
    }
}

/// D18's statically decidable overlap rule (§6.1, pre-A1b rule restored
/// after a merged-length variant proved too strict — see the review
/// note below): equal `process_names` entries; a `comm_hints` or
/// `linux_name_contains` entry of one equal to or a substring of a
/// `process_names`/`comm_hints`/`linux_name_contains` entry of the
/// other, `linux_name_contains` treated exactly like `comm_hints` for
/// this static check (regex overlap on `process_path_patterns`/
/// `command_line_patterns` is undecidable in general and is never
/// checked here, `docs/v2/plugin-abi.md:121`).
///
/// A pairwise "could both match some ≤15-byte `comm`" merged-length
/// rule was tried and reverted: it made almost any two short entries
/// statically conflict (e.g. `linux_name_contains = ["mygame"]` against
/// `vosu.osu`'s `"osu!"`, since `"osu!mygame"` is 15 bytes or fewer),
/// rejecting most real plugin pairs outright. Static overlap stays a
/// cheap, conservative early refusal for the obvious case; the runtime
/// rule (any process two enabled matchers both accept,
/// `MatcherEngine::match_candidate`) is the authoritative check for a
/// pair like `"osu!"`/`"lazer"` that shares no containment yet
/// occasionally both match one real `comm` such as `osu!lazer`.
#[must_use]
pub fn overlaps(platform: MatchPlatform, a: &MatchTable, b: &MatchTable) -> bool {
    if a.process_names
        .iter()
        .any(|x| b.process_names.iter().any(|y| names_equal(platform, x, y)))
    {
        return true;
    }
    let a_hints: Vec<&String> = a
        .comm_hints
        .iter()
        .chain(a.linux_name_contains.iter())
        .collect();
    let b_hints: Vec<&String> = b
        .comm_hints
        .iter()
        .chain(b.linux_name_contains.iter())
        .collect();
    let hints_hit = |hints: &[&String], names: &[String], other_hints: &[&String]| {
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
    hints_hit(&a_hints, &b.process_names, &b_hints)
        || hints_hit(&b_hints, &a.process_names, &a_hints)
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
/// entries that fit its 15-byte `comm` field width, plus every
/// `comm_hints`/`linux_name_contains` entry, across every enabled Linux
/// game plugin.
const PREFILTER_LIMIT: usize = 256;

/// How many of `table`'s entries occupy the combined pre-filter (§6.1): a
/// `process_names` entry that is itself short enough to match `comm`
/// directly, plus every `comm_hints`/`linux_name_contains` entry (A1b/DB
/// revised: `linux_name_contains` also feeds the pre-filter, exactly
/// like `comm_hints`, so declaring it never widens the filter).
fn prefilter_entries(table: &MatchTable) -> usize {
    table
        .process_names
        .iter()
        .filter(|name| name.len() <= 15)
        .count()
        + table.comm_hints.len()
        + table.linux_name_contains.len()
}

/// Whether `table`'s own matcher has a hint the pre-filter can narrow
/// by, used only by [`MatcherEngine::prefilter`]'s widening rule
/// (§6.1): on Linux, a `comm_hints`/`linux_name_contains` entry or a
/// `process_names` entry short enough to match the kernel `comm` field;
/// on Windows, always `false` — Windows has no `comm`/CN_PROC pre-filter
/// at all (§6.1: "accepted but unused on Windows"), so a plugin there
/// never has a hint to fall back on once it declares a
/// path/command-line pattern.
fn has_usable_hint(linux: bool, table: &MatchTable) -> bool {
    linux
        && (!table.comm_hints.is_empty()
            || !table.linux_name_contains.is_empty()
            || table.process_names.iter().any(|name| name.len() <= 15))
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
    /// `PreFilterLimitError` when installing it would push the combined
    /// Linux pre-filter past `PREFILTER_LIMIT` (§6.1, D18); refuses with
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
    /// already-tracked process to [`super::PluginHost::process_appeared`] — a
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
    /// plugin's matcher is not `CompiledMatcher::names_only` *and* has
    /// no usable hint of its own (`has_usable_hint`); otherwise the
    /// deduplicated union of every registered plugin's hint names. A
    /// plugin's hint names are: on Linux, its `process_names` entries
    /// short enough to match the kernel `comm` field (`≤15` bytes) plus
    /// every `comm_hints`/`linux_name_contains` entry (§6.1, A1b/DB
    /// revised); on Windows, every `process_names` entry verbatim
    /// (Windows has no `comm` field width limit and no
    /// `comm_hints`/`linux_name_contains` concept at all, so a plugin
    /// there either matches by name alone or widens the whole
    /// pre-filter the moment it also declares a path/command-line
    /// pattern — exactly 1.x's own name-filter-or-nothing behavior).
    #[must_use]
    pub fn prefilter(&self) -> Prefilter {
        Prefilter::for_tables(
            self.platform,
            self.entries
                .iter()
                .map(|(_, table, compiled)| (table, compiled)),
        )
    }
}

/// Separate budget for unregistered observer interest (M6b G0 catalog game
/// offers): at most this many extra pre-filter names, on top of — never
/// displacing — installed interest, and never past [`PREFILTER_LIMIT`] on
/// Linux. Remote catalog content can therefore never widen the filter.
pub const OBSERVER_PREFILTER_BUDGET: usize = 64;

/// The pre-filter names one table contributes: on Linux, `process_names`
/// short enough to match `comm` plus every `comm_hints`/
/// `linux_name_contains` entry; on Windows, every `process_names` entry.
fn table_names(linux: bool, table: &MatchTable) -> impl Iterator<Item = &String> + Clone {
    table
        .process_names
        .iter()
        .filter(move |name| !linux || name.len() <= 15)
        .chain(
            table
                .comm_hints
                .iter()
                .chain(table.linux_name_contains.iter())
                .filter(move |_| linux),
        )
}

impl Prefilter {
    /// [`MatcherEngine::prefilter`]'s rule for *registered* (installed)
    /// matchers, including widening for a pattern matcher with no hint.
    fn for_tables<'a>(
        platform: MatchPlatform,
        tables: impl Iterator<Item = (&'a MatchTable, &'a CompiledMatcher)> + Clone,
    ) -> Self {
        let linux = platform == MatchPlatform::Linux;
        let widens = tables
            .clone()
            .any(|(table, compiled)| !compiled.names_only() && !has_usable_hint(linux, table));
        if widens {
            return Self::Unfiltered;
        }
        let mut names: Vec<String> = tables
            .flat_map(|(table, _)| table_names(linux, table).cloned())
            .collect();
        names.sort_unstable();
        names.dedup();
        Self::Names(names)
    }

    /// Bounded hint names for matchers that are deliberately *not*
    /// registered (catalog game offers): never [`Self::Unfiltered`]. A table
    /// with no usable name (path/command-line only) contributes nothing, and
    /// a table whose new names would exceed `budget` contributes none of
    /// them; both can still match processes discovery observes anyway.
    #[must_use]
    pub fn hint_names<'a>(
        platform: MatchPlatform,
        tables: impl Iterator<Item = &'a MatchTable>,
        budget: usize,
    ) -> Vec<String> {
        let linux = platform == MatchPlatform::Linux;
        let mut names: Vec<String> = Vec::new();
        for table in tables {
            let mut fresh: Vec<&String> = table_names(linux, table)
                .filter(|name| !names.contains(*name))
                .collect();
            fresh.sort_unstable();
            fresh.dedup();
            if fresh.is_empty() || names.len() + fresh.len() > budget {
                continue;
            }
            names.extend(fresh.into_iter().cloned());
        }
        names
    }

    /// Adds an observer's bounded extra names to this installed pre-filter.
    /// The installed side keeps its own rules: [`Self::Unfiltered`] stays
    /// unfiltered, and an installed Linux set beyond [`PREFILTER_LIMIT`]
    /// still widens. The observer side can never widen or displace installed
    /// names: it adds at most [`OBSERVER_PREFILTER_BUDGET`] valid names and,
    /// on Linux, only into the remaining kernel/CN_PROC table capacity.
    #[must_use]
    pub fn union(self, observer: &[String], platform: MatchPlatform) -> Self {
        let Self::Names(mut names) = self else {
            return Self::Unfiltered;
        };
        let linux = platform == MatchPlatform::Linux;
        if linux && names.len() > PREFILTER_LIMIT {
            return Self::Unfiltered;
        }
        let capacity = if linux {
            OBSERVER_PREFILTER_BUDGET.min(PREFILTER_LIMIT - names.len())
        } else {
            OBSERVER_PREFILTER_BUDGET
        };
        let mut added = 0;
        for name in observer {
            if added == capacity {
                break;
            }
            let usable = !name.is_empty() && !name.contains('\0') && (!linux || name.len() <= 15);
            if usable && !names.contains(name) {
                names.push(name.clone());
                added += 1;
            }
        }
        names.sort_unstable();
        names.dedup();
        Self::Names(names)
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
mod tests;

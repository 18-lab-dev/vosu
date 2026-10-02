import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { CURRENT_ORACLE, SOURCE_REVISION, OSU_REVISION, CALCULATOR_REVISION, CALCULATOR_PACKAGE, ROSU_BASELINE, UnknownRecord, fail, record, stringField, arrayField } from "./binding.ts";

export function gitRevision(path: string): string {
  return execFileSync("git", ["-C", path, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
}

export function validateReference(root: string): void {
  const packageJson = record(JSON.parse(readFileSync(join(root, "lib/package.json"), "utf8")) as unknown, "calculator package");
  if (stringField(packageJson, "version", "calculator package") !== CURRENT_ORACLE.calculatorPackage) fail("wrong installed C# package version");
  const osu = record(packageJson.osu, "calculator package.osu");
  if (stringField(osu, "revision", "calculator package.osu") !== CURRENT_ORACLE.osu) fail("wrong osu! source revision");
  if (gitRevision(root) !== CURRENT_ORACLE.lazerCalculator) fail("wrong calculator checkout revision");
  const tree = execFileSync("git", ["-C", root, "rev-parse", "HEAD^{tree}"], { encoding: "utf8" }).trim();
  if (tree !== CURRENT_ORACLE.calculatorTree) fail("wrong calculator source tree");
  execFileSync("git", ["-C", root, "diff", "--exit-code", "HEAD", "--"]);
}

export function validateOracleInputs(manifestPath: string): void {
  const root = join(dirname(manifestPath), "inputs/osu-97e94bab");
  const sourceManifest = record(
    JSON.parse(readFileSync(join(root, "manifest.json"), "utf8")) as unknown,
    "oracle input manifest",
  );
  if (stringField(sourceManifest, "revision", "oracle input manifest") !== OSU_REVISION) {
    fail("wrong canonical oracle input revision");
  }
  for (const [index, rawFile] of arrayField(sourceManifest, "files", "oracle input manifest").entries()) {
    const file = record(rawFile, `oracle input file ${index}`);
    const path = stringField(file, "path", `oracle input file ${index}`);
    const expected = stringField(file, "sha256", `oracle input file ${index}`);
    const actual = createHash("sha256").update(readFileSync(join(root, path))).digest("hex");
    if (actual !== expected) fail(`canonical oracle input checksum mismatch: ${path}`);
  }
}

export function validateCsharpEvidence(path: string): UnknownRecord {
  if (!existsSync(path)) fail(`missing executed pinned C# test evidence: ${path}`);
  const evidence = record(JSON.parse(readFileSync(path, "utf8")) as unknown, "C# test evidence");
  if (evidence.schema !== 1) fail("wrong C# test evidence schema");
  if (stringField(evidence, "osuRevision", "C# test evidence") !== OSU_REVISION) {
    fail("wrong C# test evidence osu! revision");
  }
  const version = stringField(evidence, "dotnetVersion", "C# test evidence");
  if (!/^8\./.test(version)) fail(`wrong C# oracle runtime: ${version}`);
  const expected = new Map<string, string>([
    ["osu.Game.Rulesets.Osu.Tests/osu.Game.Rulesets.Osu.Tests.csproj", "FullyQualifiedName~OsuScoreMultiplierTest"],
    ["osu.Game.Rulesets.Taiko.Tests/osu.Game.Rulesets.Taiko.Tests.csproj", "FullyQualifiedName~TaikoScoreMultiplierTest|FullyQualifiedName~TaikoBeatmapConversionTest"],
    ["osu.Game.Rulesets.Catch.Tests/osu.Game.Rulesets.Catch.Tests.csproj", "FullyQualifiedName~CatchScoreMultiplierTest|FullyQualifiedName~CatchBeatmapConversionTest"],
    ["osu.Game.Rulesets.Mania.Tests/osu.Game.Rulesets.Mania.Tests.csproj", "FullyQualifiedName~ManiaScoreMultiplierTest|FullyQualifiedName~ManiaBeatmapConversionTest"],
  ]);
  const tests = arrayField(evidence, "tests", "C# test evidence");
  if (tests.length !== expected.size) fail("C# test evidence must contain all four ruleset suites");
  for (const [index, raw] of tests.entries()) {
    const test = record(raw, `C# test evidence.tests[${index}]`);
    const project = stringField(test, "project", "C# test evidence test");
    const filter = stringField(test, "filter", "C# test evidence test");
    if (expected.get(project) !== filter) fail(`unexpected C# oracle project/filter: ${project} ${filter}`);
    const hash = stringField(test, "stdoutSha256", "C# test evidence test");
    if (!/^[0-9a-f]{64}$/.test(hash)) fail(`invalid C# oracle stdout hash: ${project}`);
    expected.delete(project);
  }
  if (expected.size !== 0) fail("C# test evidence omitted a required ruleset suite");
  return evidence;
}

export function validateManifest(path: string): UnknownRecord {
  const manifest = record(JSON.parse(readFileSync(path, "utf8")) as unknown, "manifest");
  const pins = record(manifest.pins, "manifest.pins");
  const expected: Record<string, string> = {
    sourceRevision: SOURCE_REVISION,
    rosuBaseline: ROSU_BASELINE,
    osuRevision: OSU_REVISION,
    calculatorRevision: CALCULATOR_REVISION,
    calculatorPackage: CALCULATOR_PACKAGE,
  };
  for (const [key, value] of Object.entries(expected)) {
    if (stringField(pins, key, "manifest.pins") !== value) fail(`wrong manifest pin: ${key}`);
  }
  const items = arrayField(manifest, "items", "manifest");
  const ids = new Set<string>();
  for (const [index, rawItem] of items.entries()) {
    const item = record(rawItem, `manifest.items[${index}]`);
    const id = stringField(item, "id", `manifest.items[${index}]`);
    if (ids.has(id)) fail(`duplicate manifest item: ${id}`);
    ids.add(id);
    if (arrayField(item, "vectors", `manifest item ${id}`).length === 0) fail(`uncovered manifest item: ${id}`);
    for (const field of ["location", "baselineLocation", "rustLocation", "delta"]) {
      if (stringField(item, field, `manifest item ${id}`).trim().length === 0) {
        fail(`manifest item ${id} has empty ${field}`);
      }
    }
  }
  return manifest;
}

export function derivedCapabilities(scenario: UnknownRecord): Set<string> {
  const id = stringField(scenario, "id", "scenario");
  const expected = record(scenario.expected, `scenario ${id}.expected`);
  const capabilities = new Set<string>();

  if (typeof scenario.mode === "string" && Array.isArray(scenario.mods)) {
    const mode = scenario.mode;
    const variant = id.slice(mode.length + 1);
    const mapName = typeof scenario.map === "string" ? scenario.map : "fixture.osu";
    const mapCapability = mapName === "fixture.osu" ? "fixture-osu" : "pinned-diffcalc";
    for (const capability of [
      `mode.${mode}`, `parse.${mapCapability}.success`, `convert.${mapCapability}.to-${mode}.success`,
      `mods.${mode}.${variant}.applied`, `difficulty.${mode}.${mapCapability}.full`,
      `performance.${mode}.${mapCapability}.full-95pct`, `strains.${mode}.${mapCapability}.full`,
      `score.${mode}.${mapCapability}.generated-95pct`, `accuracy.${mode}.${mapCapability}.generated-score`,
      `gradual.${mode}.${mapCapability}.prefix-one`, `gradual.${mode}.${mapCapability}.full`,
      `gradual.${mode}.${mapCapability}.fresh-replay`, `json.${mode}.${mapCapability}.exact-fields`,
    ]) capabilities.add(capability);
    if (variant === "branch-rich") capabilities.add(`branch.${mode}.nondegenerate-pinned-diffcalc`);
    if (mapName !== "fixture.osu") {
      for (const section of ["difficulty", "performance"] as const) {
        const values = record(expected[section], `${id}.${section}`);
        for (const [field, value] of Object.entries(values)) {
          if (typeof value === "number" && Number.isFinite(value) && value !== 0) {
            capabilities.add(`signal.${mode}.${section}.${field}.nonzero`);
          }
        }
      }
      const objectCount = expected.objectCount;
      if (typeof objectCount === "number" && objectCount > 16) {
        capabilities.add(`signal.${mode}.object-count.gt16`);
      }
    }
    if (variant === "dt125") capabilities.add("mod.dt.speed_change.1_25.applied");
    if (variant === "ht075") capabilities.add("mod.ht.speed_change.0_75.applied");
    if (variant === "mirror-string") capabilities.add("mod.mr.reflection.string.applied");
    if (variant === "difficulty-adjust") capabilities.add("mod.da.four-difficulty-settings.applied");
    if (variant === "classic") capabilities.add("mod.classic.applied");
    if (variant === "random-seeded") capabilities.add(`mod.${mode}.random.seed-12345.applied`);
    if (variant === "numeric-string") capabilities.add("mod.da.overall-difficulty.string-7_4.coerced");
    const branchGeometry: Record<string, string[]> = {
      "osu-branches/snap.osu": ["input.osu.aim.snap.alternating-acute-wide-jumps"],
      "osu-branches/flow.osu": ["input.osu.aim.flow.equal-spacing-smooth-arc"],
      "osu-branches/agility.osu": ["input.osu.aim.agility.repeated-90-180-reversals"],
      "osu-branches/speed-rhythm.osu": ["input.osu.speed.rhythm.intervals-100-100-150-75"],
      "osu-branches/reading-fl.osu": ["input.osu.reading-fl.mixed-angle-slider-occlusion"],
      "osu-branches/sliders.osu": ["input.osu.slider.reversal-high-travel-mixed-circles"],
      "osu-branches/sections.osu": ["input.osu.sections.objects-at-399-400-401"],
    };
    for (const predicate of branchGeometry[mapName] ?? []) capabilities.add(predicate);
    if (mapName === "osu-branches/aggregate-skills.osu") {
      capabilities.add(`branch.osu.${id}`);
    }
    return capabilities;
  }
  if (scenario.kind === "generated-score-boundary") {
    const mode = stringField(scenario, "mode", `scenario ${id}`);
    const score = record(expected.score, `${id}.score`);
    for (const key of ["greats", "goods", "oks", "mehs", "misses", "largeTickMisses", "sliderEndHits"]) {
      if (typeof score[key] !== "number") fail(`generated score ${id} missing ${key}`);
    }
    capabilities.add(`score-generation.${mode}.${id.slice("generated-score-".length)}`);
    capabilities.add(`score-generation.${mode}.source-branch-boundary`);
    return capabilities;
  }


  if (scenario.kind === "canonical-conversion") {
    if (scenario.sourceRevision !== OSU_REVISION) fail(`untrusted conversion revision: ${id}`);
    const input = stringField(scenario, "input", `scenario ${id}`);
    const expectedSource = stringField(scenario, "expectedSource", `scenario ${id}`);
    const pairs: Record<string, string> = {
      "mania/basic.osu|mania/basic-expected-conversion.json": "convert.mania.ds",
      "mania/1K.osu|mania/1K-expected-conversion.json": "convert.mania.in",
      "mania/7K.osu|mania/7K-expected-conversion.json": "convert.mania.rd-seeded",
      "mania/10K.osu|mania/10K-expected-conversion.json": "convert.mania.tp-seeded",
      "mania/mania-specific-spinner.osu|mania/mania-specific-spinner-expected-conversion.json": "convert.mania.native-legacy-spinner-short",
      "mania/zero-length-slider.osu|mania/zero-length-slider-expected-conversion.json": "convert.mania.spinner-hold",
      "mania/18K.osu|mania/18K-expected-conversion.json": "convert.mania.simultaneous",
      "catch/basic-hyperdash.osu|catch/basic-hyperdash-expected-conversion.json": "convert.catch.hyperdash",
      "catch/slider.osu|catch/slider-expected-conversion.json": "convert.catch.linear-slider",
      "catch/tiny-ticks.osu|catch/tiny-ticks-expected-conversion.json": "convert.catch.tiny-droplets",
      "catch/spinner-and-circles.osu|catch/spinner-and-circles-expected-conversion.json": "convert.catch.nested-count",
      "catch/high-speed-multiplier-precision.osu|catch/high-speed-multiplier-precision-expected-conversion.json": "convert.catch.precision",
      "taiko/basic.osu|taiko/basic-expected-conversion.json": "convert.taiko.base-conversion",
      "taiko/sample-to-type-conversions.osu|taiko/sample-to-type-conversions-expected-conversion.json": "convert.taiko.sample-type",
      "taiko/slider-conversion-v14.osu|taiko/slider-conversion-v14-expected-conversion.json": "convert.taiko.slider-v14",
    };
    const capability = pairs[`${input}|${expectedSource}`] ?? fail(`unknown canonical conversion pair: ${input}`);
    if (Object.keys(expected).length === 0) fail(`empty canonical conversion result: ${id}`);
    capabilities.add(capability);
    return capabilities;
  }

  if (id.startsWith("score-multiplier-")) {
    if (scenario.sourceRevision !== OSU_REVISION || !Array.isArray(expected.mods) || typeof expected.multiplier !== "number") {
      fail(`malformed pinned multiplier proof: ${id}`);
    }
    capabilities.add(`score.multiplier.${id.slice("score-multiplier-".length)}.combination`);
    return capabilities;
  }
  if (id.startsWith("gradual-actions-")) {
    const mode = id.slice("gradual-actions-".length);
    for (const key of ["advanceOne", "skipOneThenAdvance", "before", "start", "end", "nan", "skipEnd", "skipEndStrains", "replay"]) {
      if (!(key in expected)) fail(`gradual proof ${id} missing ${key}`);
    }
    for (const branch of ["advance-one", "skip-count-boundary", "skip-to-time-before", "skip-to-time-start", "skip-to-time-end", "skip-to-time-nan", "skip-end", "full-prefix-strains", "fresh-replay"]) {
      capabilities.add(`gradual.${mode}.${branch}`);
    }
    return capabilities;
  }
  if (id.startsWith("custom-score-")) {
    const mode = id.slice("custom-score-".length);
    const score = record(expected.score, `${id}.score`);
    for (const key of ["sliderEndHits", "comboBreaks", "ignoreHits", "ignoreMisses", "largeBonuses", "smallBonuses", "largeTickHits", "largeTickMisses", "smallTickHits", "smallTickMisses", "perfects", "greats", "goods", "oks", "mehs", "misses"]) {
      if (typeof score[key] !== "number") fail(`custom score ${id} missing ${key}`);
    }
    if (scenario.inputAccuracy !== "NaN" || score.accuracy !== null) {
      fail(`custom score ${id} must preserve the explicit NaN input marker`);
    }
    capabilities.add(`score.${mode}.all-hit-result-counters`);
    capabilities.add(`score.${mode}.negative-total-score`);
    capabilities.add(`score.${mode}.nonfinite-input`);
    if (mode === "osu") capabilities.add("score.osu.classic-slider-tail-large-tick-miss");
    if (mode === "catch") capabilities.add("score.catch.signed-large-tick-miss");
    return capabilities;
  }
  if (id === "wrapper-action-contract") {
    for (const key of [
      "original", "before", "adjusted", "replacement", "convertedDifficulty",
      "basePerformance", "adjustedPerformance", "retainedPerformance", "baseAttrs", "adjustedAttrs",
    ]) {
      if (!(key in expected)) fail(`wrapper action proof missing ${key}`);
    }
    for (const capability of [
      "wrapper.original-difficulty.exact",
      "wrapper.adjusted-difficulty.da-known-setting",
      "wrapper.unknown-setting.isolated",
      "wrapper.apply-mods.rebuilds-from-original",
      "wrapper.conversion.drops-prior-mods",
      "wrapper.performance.retained-attrs",
      "wrapper.performance.raw-components",
      "wrapper.structured-mod-array",
    ]) capabilities.add(capability);
    return capabilities;
  }
  if (scenario.kind === "wrapper-display-retained-attrs") {
    const displays = record(expected.displays, id);
    const retained = record(expected.retained, id);
    const crossMode = record(expected.crossMode, id);
    for (const mode of ["osu", "taiko", "catch", "mania"]) {
      const values = record(displays[mode], `${id}.${mode}`);
      for (const variant of [
        "base", "hr", "ez", "dt", "ht", "mixedFixedRates", "duplicateFixedRates",
        "windUp", "windDown", "da",
      ]) if (!(variant in values)) fail(`${id} missing ${mode} ${variant} display`);
      if (mode !== "catch" && !("adaptiveSpeed" in values))
        fail(`${id} missing ${mode} adaptive-speed display`);
      capabilities.add(`wrapper.display.${mode}.exact`);
      if (mode === "mania") {
        for (const rateVariant of [
          "dt", "ht", "mixedFixedRates", "duplicateFixedRates", "windUp", "windDown", "adaptiveSpeed",
        ]) if (JSON.stringify(values.base) !== JSON.stringify(values[rateVariant]))
          fail(`mania display ${rateVariant} must be rate-invariant`);
        capabilities.add("wrapper.display.mania.no-rate-invariance");
      } else {
        capabilities.add(`wrapper.display.${mode}.dt-ht-rate`);
        capabilities.add(`wrapper.display.${mode}.ordered-rate-surface`);
      }
    }
    for (const mode of ["osu", "taiko", "catch"]) {
      const values = record(retained[mode], `${id}.${mode}.retained`);
      if (!("hr" in values) || !("da" in values)) fail(`${id} missing ${mode} HR/DA retained attrs`);
      capabilities.add(`wrapper.retained-attrs.${mode}.hr-da`);
    }
    if (Object.keys(crossMode).length !== 12 || Object.values(crossMode).some((value) => value !== true))
      fail("every ordered cross-mode retained-attribute pairing must throw");
    capabilities.add("wrapper.retained-attrs.cross-mode-throws");
    capabilities.add("wrapper.display.mania.hr-ez-key-count");
    return capabilities;
  }
  if (id === "gradual-osu-partial-strains") {
    if (expected.freshThrows !== true || expected.partialThrows !== true || !("full" in expected))
      fail("partial osu strains contract must throw for fresh/partial and retain full output");
    capabilities.add("gradual.osu.partial-strains-error");
    capabilities.add("gradual.osu.full-strains-success");
    return capabilities;
  }
  if (id === "random-seed-persistence") {
    for (const name of ["osu-rd", "osu-tp", "taiko-rd", "mania-rd"]) {
      const result = record(expected[name], `random seed persistence ${name}`);
      if (result.difficultyStable !== true)
        fail(`seedless ${name} must remain stable within one playable-map lifecycle`);
      capabilities.add(`seed-persistence.${name}.repeated-difficulty`);
    }
    return capabilities;
  }
  if (scenario.kind === "mania-performance-branches") {
    const difficultyGate = record(expected.difficultyGate, id);
    const totalScore = record(expected.totalScore, id);
    const scoreV2 = record(expected.scoreV2, id);
    const totalHits = record(expected.totalHits, id);
    if (JSON.stringify(difficultyGate.high) === JSON.stringify(difficultyGate.low))
      fail("mania difficulty gate discriminator produced identical outputs");
    if (JSON.stringify(totalScore.half) !== JSON.stringify(totalScore.full))
      fail("mania performance must be invariant to TotalScore");
    if (JSON.stringify(scoreV2.base) !== JSON.stringify(scoreV2.modded))
      fail("mania performance must be invariant to ScoreV2 normalization");
    if (JSON.stringify(totalHits.normal) === JSON.stringify(totalHits.long))
      fail("mania total-hit-count discriminator produced identical outputs");
    for (const capability of [
      "branch.mania.performance.aggregate",
      "branch.mania.performance.difficulty-gate",
      "branch.mania.performance.total-score-negative-invariance",
      "branch.mania.performance.score-v2-negative-invariance",
      "branch.mania.performance.total-hits-scaling",
    ]) capabilities.add(capability);
    return capabilities;
  }
  if (scenario.kind === "taiko-performance-branches") {
    if (typeof expected.objectCount !== "number" || expected.objectCount <= 16)
      fail("taiko performance discriminator requires more than sixteen objects");
    if (JSON.stringify(expected.perfect) === JSON.stringify(expected.mixed))
      fail("taiko performance discriminator produced identical outputs");
    for (const capability of [
      "branch.taiko.performance.aggregate",
      "branch.taiko.performance.difficulty-pp",
      "branch.taiko.performance.accuracy-pp",
      "branch.taiko.performance.estimated-unstable-rate",
      "branch.taiko.performance.effective-miss-count",
    ]) capabilities.add(capability);
    return capabilities;
  }
  if (scenario.kind === "osu-performance-miss-branches") {
    for (const key of ["effectiveMiss", "legacyTail", "lazerTick"])
      if (!(key in expected)) fail(`osu performance branch contract missing ${key}`);
    for (const capability of [
      "branch.osu.performance.aggregate",
      "branch.osu.performance.effective-miss-count",
      "branch.osu.performance.legacy-slider-tail-misses",
      "branch.osu.performance.lazer-tick-accuracy",
    ]) capabilities.add(capability);
    return capabilities;
  }
  if (scenario.kind === "paired-ruleset-discriminator") {
    if (!("left" in expected) || !("right" in expected))
      fail(`paired ruleset discriminator ${id} must contain both exact sides`);
    const mode = stringField(scenario, "mode", id);
    capabilities.add(`pair.${mode}.${id}.exact`);
    return capabilities;
  }
  if (id === "exhaustive-score-multiplier-matrix") {
    const rows = arrayField(expected, "rows", id);
    const expectedSourceRows: Record<string, number> = { osu: 147, taiko: 89, catch: 88, mania: 101 };
    if (rows.length < 500) fail("multiplier matrix must cover every pinned source row and boundary family");
    for (const [mode, expectedCount] of Object.entries(expectedSourceRows)) {
      const sourceRows = rows.filter((value) => {
        const row = record(value, id);
        return row.mode === mode && typeof row.id === "string" && row.id.startsWith(`${mode}-source-line-`);
      });
      if (sourceRows.length !== expectedCount)
        fail(`multiplier matrix has ${sourceRows.length}/${expectedCount} pinned ${mode} source rows`);
      if (sourceRows.some((value) => typeof record(value, id).source !== "string"))
        fail(`multiplier matrix has an unpinned ${mode} source row`);
    }
    for (const mode of ["osu", "taiko", "catch", "mania"])
      capabilities.add(`multiplier.${mode}.exhaustive-branch-matrix`);
    return capabilities;
  }
  if ("throws" in expected && id === "gradual-prefix-zero-attrs") {
    capabilities.add("gradual.prefix-zero.attrs.accepted");
    return capabilities;
  }
  if (["osuOd5", "taikoOd10", "catchOd5", "maniaOd0"].every((key) => key in expected)) {
    capabilities.add("hit-window.ruleset-od-boundaries");
    return capabilities;
  }
  fail(`cannot derive capabilities from scenario actions/results: ${id}`);
}

export function validateCoverage(manifestValue: UnknownRecord, fixtureValue: unknown): void {
  const fixture = record(fixtureValue, "fixture");
  const scenarios = new Map<string, Set<string>>();
  for (const collection of ["vectors", "contracts"]) {
    for (const [index, rawScenario] of arrayField(fixture, collection, "fixture").entries()) {
      const scenario = record(rawScenario, `fixture.${collection}[${index}]`);
      const id = stringField(scenario, "id", `fixture.${collection}[${index}]`);
      const actions = arrayField(scenario, "actions", `scenario ${id}`);
      if (actions.length === 0 || actions.some((value) => typeof value !== "string")) {
        fail(`scenario ${id} must name every oracle action`);
      }
      const capabilities = derivedCapabilities(scenario);
      const observed = arrayField(scenario, "observedCapabilities", `scenario ${id}`).map((value) => {
        if (typeof value !== "string") fail(`scenario ${id} observed capability must be a string`);
        return value;
      });
      const derived = [...capabilities].sort();
      if (JSON.stringify([...observed].sort()) !== JSON.stringify(derived)) {
        fail(`scenario ${id} observed capabilities do not match its action/result shape`);
      }
      if (capabilities.size === 0) fail(`scenario ${id} has no derived capabilities`);
      if (scenarios.has(id)) fail(`duplicate fixture scenario: ${id}`);
      scenarios.set(id, capabilities);
    }
  }

  for (const [index, rawItem] of arrayField(manifestValue, "items", "manifest").entries()) {
    const item = record(rawItem, `manifest.items[${index}]`);
    const id = stringField(item, "id", `manifest.items[${index}]`);
    const requirements = arrayField(item, "requires", `manifest item ${id}`).map((value) => {
      if (typeof value !== "string") fail(`manifest item ${id} requirement must be a string`);
      return value;
    });
    if (requirements.length === 0) fail(`manifest item has no behavioral requirements: ${id}`);
    const covered = arrayField(item, "vectors", `manifest item ${id}`).some((scenarioId) => {
      if (typeof scenarioId !== "string") fail(`manifest item ${id} scenario id must be a string`);
      const capabilities = scenarios.get(scenarioId);
      return capabilities !== undefined && requirements.every((requirement) => capabilities.has(requirement));
    });
    if (!covered) fail(`manifest item has no behaviorally capable scenario: ${id}`);
  }
}

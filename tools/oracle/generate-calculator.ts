import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createRequire } from "node:module";

const SOURCE_REVISION = "1a67fd64cc38fe60306328dde0a429e3211ca4ed";
const OSU_REVISION = "97e94babde9d5ecbe574ecdd33509db27452cd0f";
const CALCULATOR_REVISION = "9a22c4c74f8e584fa64133a79a204dd045646bac";
const CALCULATOR_PACKAGE = "0.5.0-20260702-main.0";
const ROSU_BASELINE = "v4.0.1 / osu! 28c846b4";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type UnknownRecord = Record<string, unknown>;

interface Options {
  source: string;
  calculatorSource: string;
  manifest: string;
  map: string;
  out: string;
  binding?: string;
  verifyOnly: boolean;
}

function fail(message: string): never {
  throw new Error(message);
}

function record(value: unknown, label: string): UnknownRecord {
  if (typeof value !== "object" || value === null || Array.isArray(value)) fail(`${label} must be an object`);
  return value as UnknownRecord;
}

function stringField(value: UnknownRecord, key: string, label: string): string {
  const field = value[key];
  if (typeof field !== "string") fail(`${label}.${key} must be a string`);
  return field;
}

function arrayField(value: UnknownRecord, key: string, label: string): unknown[] {
  const field = value[key];
  if (!Array.isArray(field)) fail(`${label}.${key} must be an array`);
  return field;
}

function call(target: unknown, method: string, args: unknown[] = []): unknown {
  const object = record(target, `receiver for ${method}`);
  const candidate = object[method];
  if (typeof candidate !== "function") fail(`oracle binding is missing ${method}()`);
  return Reflect.apply(candidate, target, args) as unknown;
}

function staticCall(target: unknown, method: string, args: unknown[] = []): unknown {
  if ((typeof target !== "object" || target === null) && typeof target !== "function") {
    return fail(`static receiver for ${method} must be an object or constructor`);
  }
  const candidate = Reflect.get(target, method) as unknown;
  if (typeof candidate !== "function") fail(`oracle binding is missing static ${method}()`);
  return Reflect.apply(candidate, target, args) as unknown;
}

function parseArgs(argv: string[]): Options {
  const values = new Map<string, string>();
  let verifyOnly = false;
  for (let index = 0; index < argv.length; index += 1) {
    const name = argv[index];
    if (name === "--verify-only") { verifyOnly = true; continue; }
    if (!name?.startsWith("--")) fail(`unexpected argument: ${name ?? "<missing>"}`);
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) fail(`missing value for ${name}`);
    values.set(name.slice(2), value);
    index += 1;
  }
  const required = (name: string): string => values.get(name) ?? fail(`missing --${name}`);
  return {
    source: resolve(required("source")),
    calculatorSource: resolve(required("calculator-source")),
    manifest: resolve(values.get("manifest") ?? "tools/oracle/calculator-diff-manifest.json"),
    map: resolve(values.get("map") ?? "crates/vosu-lazer-pp/tests/fixture.osu"),
    out: resolve(required("out")),
    binding: values.has("binding") ? resolve(required("binding")) : undefined,
    verifyOnly,
  };
}

function gitRevision(path: string): string {
  return execFileSync("git", ["-C", path, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
}

function validateReference(root: string): void {
  const packageJson = record(JSON.parse(readFileSync(join(root, "lib/package.json"), "utf8")) as unknown, "calculator package");
  if (stringField(packageJson, "version", "calculator package") !== CALCULATOR_PACKAGE) fail("wrong installed C# package version");
  const osu = record(packageJson.osu, "calculator package.osu");
  if (stringField(osu, "revision", "calculator package.osu") !== OSU_REVISION) fail("wrong osu! source revision");

  const sums = readFileSync(join(root, "SHA256SUMS"), "utf8").trim().split(/\r?\n/);
  if (sums.length < 20) fail("calculator SHA256SUMS is incomplete");
  for (const line of sums) {
    const match = /^([0-9a-f]{64})  (.+)$/.exec(line);
    if (match === null) fail(`malformed SHA256SUMS line: ${line}`);
    const file = join(root, match[2]);
    const actual = createHash("sha256").update(readFileSync(file)).digest("hex");
    if (actual !== match[1]) fail(`calculator reference checksum mismatch: ${match[2]}`);
  }
}

function validateOracleInputs(manifestPath: string): void {
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

function validateCsharpEvidence(path: string): UnknownRecord {
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

function validateManifest(path: string): UnknownRecord {
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

function derivedCapabilities(scenario: UnknownRecord): Set<string> {
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
  if ("throws" in expected && "isNull" in expected && id === "unsupported-conversion-null") {
    capabilities.add("convert.unsupported.mode-99.throws");
    return capabilities;
  }
  if (["osuGreatOd5", "taikoGreatOd10", "catchGreatOd5", "maniaGreatOd0"].every((key) => key in expected)) {
    capabilities.add("hit-window.ruleset-od-boundaries");
    return capabilities;
  }
  fail(`cannot derive capabilities from scenario actions/results: ${id}`);
}

function validateCoverage(manifestValue: UnknownRecord, fixtureValue: unknown): void {
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

function jsonValue(value: unknown, label: string): Json {
  const encoded = JSON.stringify(value, (_key: string, nested: unknown): unknown => {
    if (ArrayBuffer.isView(nested)) {
      return Array.from(nested as unknown as ArrayLike<unknown>);
    }
    return nested;
  });
  if (encoded === undefined) fail(`${label} is not JSON serializable`);
  return JSON.parse(encoded) as Json;
}

function makeMods(mode: number, variant: string): UnknownRecord[] {
  if (variant === "base") return [];
  if (variant === "dt125") {
    const mods: UnknownRecord[] = [{ acronym: "DT", settings: new Map<string, unknown>([["speed_change", 1.25]]) }];
    if (mode === 0) mods.push({ acronym: "HD", settings: new Map<string, unknown>() });
    return mods;
  }
  if (variant === "ht075") return [{ acronym: "HT", settings: new Map<string, unknown>([["speed_change", 0.75]]) }];
  if (variant === "classic") return [{ acronym: "CL", settings: new Map<string, unknown>() }];
  if (variant === "difficulty-adjust") return [{
    acronym: "DA",
    settings: new Map<string, unknown>([
      ["approach_rate", 8.3],
      ["circle_size", mode === 3 ? 5 : 4.2],
      ["drain_rate", 6.1],
      ["overall_difficulty", 7.4],
    ]),
  }];
  if (variant === "mirror-string") return [{
    acronym: "MR",
    settings: new Map<string, unknown>([["reflection", "1"]]),
  }];
  if (variant === "branch-rich") {
    if (mode === 0) {
      return [
        { acronym: "HD", settings: new Map<string, unknown>() },
        { acronym: "FL", settings: new Map<string, unknown>() },
      ];
    }
    if (mode === 1 || mode === 2) {
      return [{ acronym: "HD", settings: new Map<string, unknown>() }];
    }
    return [{ acronym: "DA", settings: new Map<string, unknown>([["overall_difficulty", 8.0]]) }];
  }
  if (variant === "random-seeded") {
    return [{ acronym: "RD", settings: new Map<string, unknown>([["seed", 12345]]) }];
  }
  if (variant === "numeric-string") {
    return [{ acronym: "DA", settings: new Map<string, unknown>([["overall_difficulty", "7.4"]]) }];
  }
  if (variant === "hidden-reading") {
    return [{ acronym: "HD", settings: new Map<string, unknown>() }];
  }
  if (variant === "flashlight-branch") {
    return [{ acronym: "FL", settings: new Map<string, unknown>() }];
  }
  if (variant === "freeze-frame") return [{ acronym: "FR", settings: new Map<string, unknown>() }];
  if (variant === "simplified-one-third") return [{
    acronym: "SR", settings: new Map<string, unknown>([
      ["one_third_conversion", true], ["one_sixth_conversion", false], ["one_eighth_conversion", false],
    ]),
  }];
  if (variant === "simplified-one-sixth") return [{
    acronym: "SR", settings: new Map<string, unknown>([
      ["one_third_conversion", false], ["one_sixth_conversion", true], ["one_eighth_conversion", false],
    ]),
  }];
  if (variant === "simplified-one-eighth") return [{
    acronym: "SR", settings: new Map<string, unknown>([
      ["one_third_conversion", false], ["one_sixth_conversion", false], ["one_eighth_conversion", true],
    ]),
  }];
  if (variant === "simplified-combined") return [{
    acronym: "SR", settings: new Map<string, unknown>([
      ["one_third_conversion", true], ["one_sixth_conversion", true], ["one_eighth_conversion", true],
    ]),
  }];
  if (variant === "swap") return [{ acronym: "SW", settings: new Map<string, unknown>() }];
  if (variant === "swap-duplicate") return [
    { acronym: "SW", settings: new Map<string, unknown>() },
    { acronym: "SW", settings: new Map<string, unknown>() },
  ];
  if (variant === "hold-off") return [{ acronym: "HO", settings: new Map<string, unknown>() }];
  if (variant === "invert") return [{ acronym: "IN", settings: new Map<string, unknown>() }];
  if (variant === "mania-mirror") return [{ acronym: "MR", settings: new Map<string, unknown>() }];
  if (variant === "mania-transform-order") return [
    { acronym: "HO", settings: new Map<string, unknown>() },
    { acronym: "IN", settings: new Map<string, unknown>() },
    { acronym: "MR", settings: new Map<string, unknown>() },
    { acronym: "RD", settings: new Map<string, unknown>([["seed", 12345]]) },
  ];
  if (variant === "classic-score-v2") return [
    { acronym: "CL", settings: new Map<string, unknown>() },
    { acronym: "SV2", settings: new Map<string, unknown>() },
  ];
  return fail(`unknown vector variant: ${variant}`);
}

function oracleVector(playBeatmap: UnknownRecord, mode: number, name: string, variant: string, map = "fixture.osu", vectorId = `${name}-${variant}`, includeStrains = true): Json {
  const converted = call(playBeatmap, "convert", [mode]);
  if (converted === null) fail(`C# oracle refused ${name} conversion`);
  const mods = makeMods(mode, variant);
  call(converted, "applyMods", [mods]);

  const counter = call(converted, "createGradualDifficulty");
  let objectCount = 0;
  while (call(counter, "advance") === true) objectCount += 1;

  const makePoint = (count: number): Json => {
    const gradual = call(converted, "createGradualDifficulty");
    for (let index = 0; index < count; index += 1) {
      if (call(gradual, "advance") !== true) fail(`${name} oracle ended before object ${count}`);
    }
    const attrs = call(gradual, "createDifficultyAttrs");
    const score = call(gradual, "createProgressiveScore", [0.95]);
    const point: UnknownRecord = {
      processed: count,
      difficulty: call(attrs, "getData"),
      performance: call(converted, "calculatePerformance", [attrs, score]),
      score,
      accuracy: call(gradual, "calculateProgressiveAccuracy", [score]),
    };
    // The pinned wrapper's compatibility strain adapter indexes previous
    // objects and throws on the first prefix. Full-prefix strains remain
    // authoritative while prefix-one still covers difficulty/performance.
    if (includeStrains && count === objectCount) point.strains = call(gradual, "getCurrentStrains");
    return jsonValue(point, `${name} gradual point`);
  };

  const fullGradual = call(converted, "createGradualDifficulty");
  call(fullGradual, "skipToEnd");
  const fullAttrs = call(fullGradual, "createDifficultyAttrs");
  const fullScore = call(converted, "createScore", [0.95]);
  let strains: unknown;
  if (includeStrains) {
    try {
      strains = call(fullGradual, "getCurrentStrains");
    } catch (error) {
      fail(`${vectorId} full-prefix strains failed: ${String(error)}`);
    }
  }
  const actions = [
    "PlayBeatmap.convert",
    "PlayBeatmap.applyMods",
    "PlayBeatmap.createGradualDifficulty",
    "GradualDifficulty.advance/skipToEnd",
    "GradualDifficulty.createDifficultyAttrs/getCurrentStrains/calculatePerformance",
    "PlayBeatmap.createScore/calculateAccuracy/calculatePerformance",
    "fresh gradual replay",
  ];
  return jsonValue({
    id: vectorId, mode: name, map, actions, mods: mods.map((mod) => ({ acronym: mod.acronym, settings: Object.fromEntries(mod.settings as Map<string, unknown>) })), targetAccuracy: 0.95,
    expected: {
      mode: name, objectCount,
      difficulty: call(fullAttrs, "getData"),
      ...(includeStrains ? { strains } : {}),
      performance: call(converted, "calculatePerformance", [fullAttrs, fullScore]), score: fullScore,
      accuracy: call(converted, "calculateAccuracy", [fullScore]),
      gradual: { prefix1: makePoint(1), full: makePoint(objectCount), replay: makePoint(objectCount) },
    },
  }, `${name} vector`);
}

const options = parseArgs(process.argv.slice(2));
function fullAttrs(playBeatmap: UnknownRecord): unknown {
  const gradual = call(playBeatmap, "createGradualDifficulty");
  call(gradual, "skipToEnd");
  return call(gradual, "createDifficultyAttrs");
}

function wrapperActionContract(playBeatmap: UnknownRecord): Json {
  const base = record(call(playBeatmap, "convert", [0]), "wrapper action base");
  const original = call(base, "getOriginalBeatmapDifficulty");
  const before = call(base, "getBeatmapDifficulty");
  const baseAttrs = fullAttrs(base);
  const baseScore = call(base, "createScore", [0.95]);
  const basePerformance = call(base, "calculatePerformance", [baseAttrs, baseScore]);

  call(base, "applyMods", [[{
    acronym: "DA",
    settings: new Map<string, unknown>([
      ["overall_difficulty", 7.4],
      ["unknown_setting", "ignored"],
    ]),
  }]]);
  const adjusted = call(base, "getBeatmapDifficulty");
  const adjustedAttrs = fullAttrs(base);
  const adjustedScore = call(base, "createScore", [0.95]);
  const adjustedPerformance = call(base, "calculatePerformance", [adjustedAttrs, adjustedScore]);
  const retainedPerformance = call(base, "calculatePerformance", [baseAttrs, baseScore]);

  call(base, "applyMods", [[{ acronym: "HD", settings: new Map<string, unknown>() }]]);
  const replacement = call(base, "getBeatmapDifficulty");
  const convertedAfterMods = record(call(base, "convert", [1]), "wrapper conversion after mods");

  return jsonValue({
    id: "wrapper-action-contract",
    actions: [
      "read original and playable difficulty",
      "apply DA with one known and one unknown setting",

      "calculate adjusted and retained-attribute performance",
      "replace DA with HD and rebuild playable map",
      "convert the modded osu map to taiko and observe conversion drops mods",
    ],
    expected: {
      original,
      before,
      adjusted,
      replacement,
      convertedDifficulty: call(convertedAfterMods, "getBeatmapDifficulty"),
      basePerformance,
      adjustedPerformance,
      retainedPerformance,
      baseAttrs: call(record(baseAttrs, "base attrs"), "getData"),
      adjustedAttrs: call(record(adjustedAttrs, "adjusted attrs"), "getData"),
    },
  }, "wrapper action contract");
}

function wrapperDisplayAndRetainedAttrsContract(playBeatmap: UnknownRecord): Json {
  const modes: ReadonlyArray<readonly [number, string]> = [
    [0, "osu"], [1, "taiko"], [2, "catch"], [3, "mania"],
  ];
  const fresh = (mode: number): UnknownRecord =>
    record(call(playBeatmap, "convert", [mode]), `display mode ${mode}`);
  const display = (mode: number, mods: UnknownRecord[]): unknown => {
    const map = fresh(mode);
    call(map, "applyMods", [mods]);
    return call(map, "getBeatmapDifficulty");
  };
  const mod = (acronym: string, settings: ReadonlyArray<readonly [string, unknown]> = []): UnknownRecord => ({
    acronym,
    settings: new Map<string, unknown>(settings),
  });
  const displays: Record<string, Json> = {};
  for (const [mode, name] of modes) {
    displays[name] = jsonValue({
      base: display(mode, []),
      hr: display(mode, [mod("HR")]),
      ez: display(mode, [mod("EZ")]),
      dt: display(mode, [mod("DT", [["speed_change", 1.25]])]),
      ht: display(mode, [mod("HT", [["speed_change", 0.75]])]),
      mixedFixedRates: display(mode, [
        mod("DT", [["speed_change", 1.1]]),
        mod("HT", [["speed_change", 0.8]]),
      ]),
      duplicateFixedRates: display(mode, [
        mod("DT", [["speed_change", 1.1]]),
        mod("DT", [["speed_change", 1.4]]),
      ]),
      windUp: display(mode, [mod("WU", [["initial_rate", 0.8], ["final_rate", 1.4]])]),
      windDown: display(mode, [mod("WD", [["initial_rate", 1.2], ["final_rate", 0.7]])]),
      ...(mode === 2 ? {} : {
        adaptiveSpeed: display(mode, [mod("AS", [["initial_rate", 1.35]])]),
      }),
      da: display(mode, [mod("DA", [
        ["approach_rate", 8.3],
        ["circle_size", mode === 3 ? 7 : 4.2],
        ["drain_rate", 6.1],
        ["overall_difficulty", 7.4],
      ])]),
    }, `${name} display difficulty`);
  }
  const retained: Record<string, Json> = {};
  for (const [mode, name] of modes.slice(0, 3)) {
    const calculate = (structuredMod: UnknownRecord): unknown => {
      const map = fresh(mode);
      const attrs = fullAttrs(map);
      const score = call(map, "createScore", [0.95]);
      call(map, "applyMods", [[structuredMod]]);
      return call(map, "calculatePerformance", [attrs, score]);
    };
    retained[name] = jsonValue({
      hr: calculate(mod("HR")),
      da: calculate(mod("DA", [["overall_difficulty", 7.4]])),
    }, `${name} retained attrs`);
  }
  const crossMode: Record<string, boolean> = {};
  for (const [sourceMode, sourceName] of modes) {
    const source = fresh(sourceMode);
    const attrs = fullAttrs(source);
    for (const [targetMode, targetName] of modes) {
      if (sourceMode === targetMode) continue;
      const target = fresh(targetMode);
      const score = call(target, "createScore", [0.95]);
      let throws = false;
      try {
        call(target, "calculatePerformance", [attrs, score]);
      } catch {
        throws = true;
      }
      crossMode[`${sourceName}-attrs-on-${targetName}`] = throws;
    }
  }
  return jsonValue({
    id: "wrapper-display-retained-attrs-contract",
    kind: "wrapper-display-retained-attrs",
    actions: [
      "get exact displayed beatmap difficulty after HR, EZ, DA, DT 1.25, and HT 0.75 in every mode",
      "calculate osu, taiko, and catch HR/DA performance from exact pre-mod retained attributes",
      "attempt every ordered cross-mode difficulty-attribute/performance pairing",
    ],
    expected: { displays, retained, crossMode },
  }, "wrapper display and retained attrs contract");
}

function gradualOsuPartialStrainsContract(playBeatmap: UnknownRecord): Json {
  const converted = record(call(playBeatmap, "convert", [0]), "partial osu strains conversion");
  const observe = (advance: boolean): boolean => {
    const gradual = record(call(converted, "createGradualDifficulty"), "partial osu strains");
    if (advance) call(gradual, "advance");
    try {
      call(gradual, "getCurrentStrains");
      return false;
    } catch {
      return true;
    }
  };
  const full = record(call(converted, "createGradualDifficulty"), "full osu strains");
  call(full, "skipToEnd");
  return jsonValue({
    id: "gradual-osu-partial-strains",
    actions: [
      "GetCurrentStrains before Advance",
      "Advance once then GetCurrentStrains on an incomplete osu prefix",
      "SkipToEnd then GetCurrentStrains",
    ],
    expected: {

      freshThrows: observe(false),
      partialThrows: observe(true),
      full: call(full, "getCurrentStrains"),
    },
  }, "partial osu strains contract");
}

function pairedRulesetContract(
  parse: unknown,
  playType: UnknownRecord,
  root: string,
  mode: number,
  modeName: string,
  id: string,
  leftMap: string,
  rightMap: string,
): Json {
  const calculate = (map: string, side: string): Json => {
    const content = readFileSync(join(root, map), "utf8");
    const beatmap = record(
      Reflect.apply(parse as (...args: unknown[]) => unknown, playType, [content]) as unknown,
      `${id} ${side}`,
    );
    const vector = record(
      oracleVector(beatmap, mode, modeName, "base", map, `${id}-${side}`),
      `${id} ${side} vector`,
    );
    return jsonValue(vector.expected, `${id} ${side} expected`);
  };
  return jsonValue({
    id,
    kind: "paired-ruleset-discriminator",
    mode: modeName,
    actions: [
      `calculate exact pinned C# difficulty, strains, performance, score, accuracy, and gradual outputs for ${leftMap}`,
      `calculate the paired control through the identical pipeline for ${rightMap}`,
    ],
    inputs: { left: leftMap, right: rightMap },
    expected: {
      left: calculate(leftMap, "left"),
      right: calculate(rightMap, "right"),
    },
  }, id);
}

function maniaPerformanceContract(
  parse: unknown,
  playType: UnknownRecord,
  root: string,
): Json {
  const mapName = "mania-branches/performance-4k.osu";
  const content = readFileSync(join(root, mapName), "utf8");
  const parsed = record(
    Reflect.apply(parse as (...args: unknown[]) => unknown, playType, [content]) as unknown,
    "mania performance map",
  );
  const map = record(call(parsed, "convert", [3]), "mania performance conversion");
  call(map, "applyMods", [[]]);
  const attrs = fullAttrs(map);
  const perfect = record(call(map, "createScore", [1]), "mania perfect score");
  const score = (overrides: UnknownRecord): UnknownRecord => ({ ...perfect, ...overrides });
  const performance = (candidate: UnknownRecord): unknown =>
    call(map, "calculatePerformance", [attrs, candidate]);
  const scoreV2Map = record(call(parsed, "convert", [3]), "mania score-v2 conversion");
  call(scoreV2Map, "applyMods", [[{ acronym: "SV2", settings: new Map() }]]);
  const scoreV2Attrs = fullAttrs(scoreV2Map);
  const scoreV2Performance = (candidate: UnknownRecord): unknown =>
    call(scoreV2Map, "calculatePerformance", [scoreV2Attrs, candidate]);
  const highAccuracy = score({ perfects: 21, greats: 3, goods: 0, oks: 0, mehs: 0, misses: 0 });
  const lowAccuracy = score({ perfects: 0, greats: 0, goods: 0, oks: 0, mehs: 24, misses: 0 });
  const totalHalf = score({ perfects: 21, greats: 3, totalScore: 500_000 });
  const totalFull = score({ perfects: 21, greats: 3, totalScore: 1_000_000 });
  const longScore = score({ perfects: 1_500, greats: 0, totalScore: 500_000 });
  return jsonValue({
    id: "mania-performance-branch-contract",
    kind: "mania-performance-branches",
    map: mapName,
    actions: [
      "calculate pinned C# mania performance from retained full-difficulty attributes",
      "exercise the <=0.95 accuracy difficulty gate using 21 perfects + 3 greats versus 24 mehs",
      "hold hit results constant while varying totalScore from 500000 to 1000000",
      "hold hit results and totalScore constant while applying ScoreV2",
      "increase total hit count from 24 to 1500 with the same retained attributes",
    ],
    expected: {
      aggregate: performance(perfect),
      difficultyGate: { high: performance(highAccuracy), low: performance(lowAccuracy) },
      totalScore: { half: performance(totalHalf), full: performance(totalFull) },
      scoreV2: { base: performance(totalHalf), modded: scoreV2Performance(totalHalf) },
      totalHits: { normal: performance(totalHalf), long: performance(longScore) },
      scores: { perfect, highAccuracy, lowAccuracy, totalHalf, totalFull, longScore },
    },
  }, "mania performance branch contract");
}

function taikoPerformanceContract(
  parse: unknown,
  playType: UnknownRecord,
  root: string,
): Json {
  const mapName = "taiko-diffcalc-test.osu";
  const content = readFileSync(join(root, mapName), "utf8");
  const parsed = record(
    Reflect.apply(parse as (...args: unknown[]) => unknown, playType, [content]) as unknown,
    "taiko performance map",
  );
  const map = record(call(parsed, "convert", [1]), "taiko performance conversion");
  const attrs = fullAttrs(map);
  const perfect = record(call(map, "createScore", [1]), "taiko perfect score");
  const objectCount = Number(perfect.greats ?? 0) + Number(perfect.oks ?? 0) + Number(perfect.misses ?? 0);
  if (objectCount <= 16) fail("taiko performance branch map must contain more than sixteen hits");
  const score = (overrides: UnknownRecord): UnknownRecord => ({ ...perfect, ...overrides });
  const mixed = score({
    greats: objectCount - 8,
    oks: 5,
    misses: 3,
    maxCombo: Math.max(1, Math.floor(objectCount / 3)),
  });
  const performance = (candidate: UnknownRecord): unknown =>
    call(map, "calculatePerformance", [attrs, candidate]);
  return jsonValue({
    id: "taiko-performance-branch-contract",
    kind: "taiko-performance-branches",
    map: mapName,
    actions: [
      "calculate pinned C# taiko performance for a greater-than-sixteen-object perfect score",
      "recalculate from retained attributes with great, ok, miss, and reduced-combo counters",
      "compare exact difficulty pp, accuracy pp, estimated unstable rate, and effective miss outputs",
    ],
    expected: {
      objectCount,
      perfect: performance(perfect),
      mixed: performance(mixed),
      scores: { perfect, mixed },
    },
  }, "taiko performance branch contract");
}

function osuPerformanceContract(
  parse: unknown,
  playType: UnknownRecord,
  root: string,
): Json {
  const mapName = "osu-branches/slider-miss-performance.osu";
  const content = readFileSync(join(root, mapName), "utf8");
  const parsed = record(
    Reflect.apply(parse as (...args: unknown[]) => unknown, playType, [content]) as unknown,
    "osu slider-miss performance map",
  );
  const map = record(call(parsed, "convert", [0]), "osu slider-miss conversion");
  const attrs = record(fullAttrs(map), "osu slider-miss attributes");
  const attrsData = record(call(attrs, "getData"), "osu slider-miss attribute data");
  const generated = record(call(map, "createScore", [1]), "osu slider-miss perfect score");
  const perfect = { ...generated, largeTickHits: 4, largeTickMisses: 0 };
  const score = (overrides: UnknownRecord): UnknownRecord => ({ ...perfect, ...overrides });
  const effectiveMiss = score({
    maxCombo: Math.max(1, Math.floor(Number(attrsData.maxCombo) / 3)),
  });
  const legacyTail = score({
    isLegacyScore: true,
    sliderEndHits: Math.max(0, Number(perfect.sliderEndHits) - 3),
  });
  const lazerTick = score({
    isLegacyScore: false,
    largeTickHits: 0,
    largeTickMisses: 4,
  });
  const performance = (candidate: UnknownRecord): unknown =>
    call(map, "calculatePerformance", [attrs, candidate]);
  return jsonValue({
    id: "osu-performance-miss-branches",
    kind: "osu-performance-miss-branches",
    map: mapName,
    actions: [
      "derive hit results and slider-tail totals from pinned C# ScoreSimulator, then install four explicit lazer large-tick hits",
      "reduce combo to one-third the retained difficulty max combo without adding misses",
      "enable legacy scoring and subtract exactly three slider-tail hits",
      "move four lazer large-tick hits to misses while retaining head and tail results",
    ],
    expected: {
      effectiveMiss: performance(effectiveMiss),
      legacyTail: performance(legacyTail),
      lazerTick: performance(lazerTick),
      scores: { effectiveMiss, legacyTail, lazerTick },
    },
  }, "osu performance miss branches");
}

function randomSeedPersistenceContract(playBeatmap: UnknownRecord): Json {
  const cases = [
    [0, "osu-rd", "RD"],
    [0, "osu-tp", "TP"],
    [1, "taiko-rd", "RD"],
    [3, "mania-rd", "RD"],
  ] as const;
  const expected: Record<string, Json> = {};
  for (const [mode, name, acronym] of cases) {
    const converted = record(call(playBeatmap, "convert", [mode]), `${name} seed persistence map`);
    call(converted, "applyMods", [[{ acronym, settings: new Map<string, unknown>() }]]);
    const firstAttrs = record(fullAttrs(converted), `${name} first attrs`);
    const secondAttrs = record(fullAttrs(converted), `${name} second attrs`);
    const firstData = jsonValue(call(firstAttrs, "getData"), `${name} first data`);
    const secondData = jsonValue(call(secondAttrs, "getData"), `${name} second data`);
    expected[name] = {
      difficultyStable: JSON.stringify(firstData) === JSON.stringify(secondData),
    };
  }
  return jsonValue({
    id: "random-seed-persistence",
    actions: [
      "apply seedless osu RD once and calculate difficulty twice",
      "apply seedless osu TP once and calculate difficulty twice",
      "apply seedless taiko RD once and calculate difficulty twice",
      "apply seedless mania RD once and calculate difficulty twice",
    ],
    expected,
  }, "random seed persistence contract");
}

function gradualActionContract(playBeatmap: UnknownRecord, mode: number, name: string): Json {
  const converted = record(call(playBeatmap, "convert", [mode]), `${name} gradual conversion`);
  const action = (steps: (gradual: UnknownRecord) => unknown): Json => {
    const gradual = record(call(converted, "createGradualDifficulty"), `${name} gradual`);
    return jsonValue(steps(gradual), `${name} gradual action`);
  };
  return jsonValue({
    id: `gradual-actions-${name}`,
    actions: [
      "Advance once", "Skip(1) then Advance", "SkipToTime before/start/end/NaN",
      "SkipToEnd", "fresh replay SkipToEnd",
    ],
    expected: {
      advanceOne: action((gradual) => call(gradual, "advance")),
      skipOneThenAdvance: action((gradual) => {
        call(gradual, "skip", [1]);
        return call(gradual, "advance");
      }),
      before: action((gradual) => call(gradual, "skipToTime", [999])),
      start: action((gradual) => call(gradual, "skipToTime", [1000])),
      end: action((gradual) => call(gradual, "skipToTime", [3500])),
      nan: action((gradual) => call(gradual, "skipToTime", [Number.NaN])),
      skipEnd: action((gradual) => {
        call(gradual, "skipToEnd");
        return call(call(gradual, "createDifficultyAttrs"), "getData");
      }),
      skipEndStrains: action((gradual) => {
        call(gradual, "skipToEnd");
        return call(gradual, "getCurrentStrains");
      }),
      replay: action((gradual) => {
        call(gradual, "skipToEnd");
        return call(call(gradual, "createDifficultyAttrs"), "getData");
      }),
    },
  }, `${name} gradual contract`);
}

function customScoreContract(playBeatmap: UnknownRecord, mode: number, name: string): Json {
  const converted = record(call(playBeatmap, "convert", [mode]), `${name} score conversion`);
  if (name === "osu") call(converted, "applyMods", [[{ acronym: "CL", settings: new Map() }]]);
  const attrs = fullAttrs(converted);
  const score = {
    totalScore: -123456789,
    isLegacyScore: name === "osu",
    accuracy: Number.NaN,
    maxCombo: 7,
    sliderEndHits: 1,
    comboBreaks: 2,
    ignoreHits: 3,
    ignoreMisses: 4,
    largeBonuses: 5,
    smallBonuses: 6,
    largeTickHits: 7,
    largeTickMisses: 8,
    smallTickHits: 9,
    smallTickMisses: 10,
    perfects: 11,
    greats: 12,
    goods: 13,
    oks: 14,
    mehs: 15,
    misses: 16,
  };
  return jsonValue({
    id: `custom-score-${name}`,
    actions: ["construct signed ScoreInfoData with every hit-result counter", "CalculateAccuracy", "CalculatePerformance"],
    inputAccuracy: "NaN",
    expected: {
      score,
      accuracy: call(converted, "calculateAccuracy", [score]),
      performance: call(converted, "calculatePerformance", [attrs, score]),
    },
  }, `${name} custom score contract`);
}

function generatedScoreContract(
  playBeatmap: UnknownRecord,
  mode: number,
  name: string,
  target: number,
  variant: "base" | "classic" = "base",
): Json {
  const converted = call(playBeatmap, "convert", [mode]);
  if (converted === null) fail(`C# oracle refused ${name} score-generation conversion`);
  const mods = variant === "classic" ? makeMods(mode, "classic") : [];
  call(converted, "applyMods", [mods]);
  const targetLabel = Number.isInteger(target) ? `${target}` : `${target}`.replace(".", "_");
  return jsonValue({
    id: `generated-score-${name}-${targetLabel}-${variant}`,
    kind: "generated-score-boundary",
    mode: name,
    targetAccuracy: target,
    classic: variant === "classic",
    actions: ["PlayBeatmap.convert/applyMods", `ScoreSimulator ${name} target=${target} classic=${variant === "classic"}`],
    expected: { score: call(converted, "createScore", [target]) },
  }, `${name} generated score ${target} ${variant}`);
}

function multiplierContract(_playBeatmap: UnknownRecord, _mode: number, name: string): Json {
  const cases: Record<string, { source: string; mods: UnknownRecord[]; expected: number }> = {
    osu: {
      source: "score-multiplier/OsuScoreMultiplierTest.cs:206",
      mods: [{ acronym: "HD", settings: {} }, { acronym: "HR", settings: {} }],
      expected: 1.04 * 1.09,
    },
    taiko: {
      source: "score-multiplier/TaikoScoreMultiplierTest.cs:152",
      mods: [{ acronym: "HD", settings: {} }, { acronym: "HR", settings: {} }],
      expected: 1.06 * 1.06,
    },
    catch: {
      source: "score-multiplier/CatchScoreMultiplierTest.cs:150",
      mods: [{ acronym: "HD", settings: {} }, { acronym: "HR", settings: {} }],
      expected: 1.06 * 1.12,
    },
    mania: {
      source: "score-multiplier/ManiaScoreMultiplierTest.cs:164",
      mods: [{ acronym: "EZ", settings: {} }, { acronym: "4K", settings: {} }],
      expected: 0.5 * 0.9,
    },
  };
  const oracle = cases[name];
  const evidenceTests = arrayField(csharpEvidence, "tests", "C# test evidence");
  const evidence = evidenceTests
    .map((value, index) => record(value, `C# test evidence.tests[${index}]`))
    .find((test) => stringField(test, "project", "C# test evidence test").includes(`.${name[0].toUpperCase()}${name.slice(1)}.Tests`));
  if (evidence === undefined) fail(`missing executed C# multiplier suite for ${name}`);
  const sourceFile = oracle.source.replace(/:\d+$/, "");
  const sourcePath = join(dirname(options.manifest), "inputs/osu-97e94bab", sourceFile);
  const sourceSha256 = createHash("sha256").update(readFileSync(sourcePath)).digest("hex");
  return jsonValue({
    id: `score-multiplier-${name}`, kind: "multiplier-source-case",
    actions: ["execute exact pinned target NUnit score multiplier suite", "replay pinned TestCase in Rust"],
    sourceRevision: OSU_REVISION,
    source: oracle.source,
    sourceSha256,
    evidence: {
      project: evidence.project,
      filter: evidence.filter,
      stdoutSha256: evidence.stdoutSha256,
    },
    expected: { mods: oracle.mods, multiplier: oracle.expected },
  }, `${name} multiplier contract`);
}

interface PinnedMultiplierCases {
  rows: Json[];
  cases: Json[];
  sources: Json[];
}

function pinnedMultiplierSourceRows(manifestPath: string): PinnedMultiplierCases {
  const root = join(dirname(manifestPath), "inputs/osu-97e94bab/score-multiplier");
  const files: ReadonlyArray<readonly [string, string, string]> = [
    ["osu", "OsuScoreMultiplierTest.cs", "osu.Game.Rulesets.Osu.Tests.OsuScoreMultiplierTest"],
    ["taiko", "TaikoScoreMultiplierTest.cs", "osu.Game.Rulesets.Taiko.Tests.TaikoScoreMultiplierTest"],
    ["catch", "CatchScoreMultiplierTest.cs", "osu.Game.Rulesets.Catch.Tests.CatchScoreMultiplierTest"],
    ["mania", "ManiaScoreMultiplierTest.cs", "osu.Game.Rulesets.Mania.Tests.ManiaScoreMultiplierTest"],
  ];
  const acronyms: Record<string, string> = {
    AccuracyChallenge: "AC", AdaptiveSpeed: "AS", Alternate: "AL", ApproachDifferent: "AD",
    Autopilot: "AP", Autoplay: "AT", BarrelRoll: "BR", Blinds: "BL", Bloom: "BM",
    Bubbles: "BU", Cinema: "CN", Classic: "CL", ConstantSpeed: "CS", Cover: "CO",
    Daycore: "DC", Deflate: "DF", Depth: "DP", DifficultyAdjust: "DA",
    DoubleTime: "DT", DualStages: "DS", Easy: "EZ", FadeIn: "FI",
    Flashlight: "FL", FloatingFruits: "FF", FreezeFrame: "FF", Grow: "GR",
    HalfTime: "HT", HardRock: "HR", Hidden: "HD", HoldOff: "HO", Invert: "IN",
    Magnetised: "MG", Mirror: "MR", MovingFast: "MF", Muted: "MU", Nightcore: "NC",
    NoFail: "NF", NoRelease: "NR", NoScope: "NS", Perfect: "PF", Random: "RD",
    Relax: "RX", Repel: "RP", ScoreV2: "SV2", SimplifiedRhythm: "SR", SingleTap: "SG",
    SpinIn: "SI", SpunOut: "SO", StrictTracking: "ST", SuddenDeath: "SD", Swap: "SW",
    Synesthesia: "SY", TargetPractice: "TP", TouchDevice: "TD", Traceable: "TC",
    Transform: "TR", Wiggle: "WG", WindDown: "WD", WindUp: "WU",
  };
  const rows: Json[] = [];
  const cases: Json[] = [];
  const sources: Json[] = [];
  const ids = new Set<string>();

  const numericExpression = (raw: string, label: string): number => {
    const expression = raw.replaceAll("f", "").replaceAll(" ", "");
    if (!/^[0-9.+*]+$/.test(expression)) fail(`unsupported numeric expression at ${label}: ${raw}`);
    const value = expression
      .split("+")
      .map((term) => term.split("*").map(Number).reduce((product, factor) => product * factor, 1))
      .reduce((sum, term) => sum + term, 0);
    if (!Number.isFinite(value)) fail(`invalid numeric expression at ${label}: ${raw}`);
    return value;
  };
  const add = (
    className: string,
    method: string,
    sourceSet: string,
    index: number,
    source: string,
    parameters: Json,
    row: UnknownRecord,
  ): void => {
    const caseId = `${className}.${method}/${sourceSet}[${index}]`;
    if (ids.has(caseId)) fail(`duplicate pinned NUnit case identity: ${caseId}`);
    ids.add(caseId);
    const identity = { caseId, method: `${className}.${method}`, sourceSet, source, parameters };
    cases.push(identity);
    rows.push({ caseId, parameterSet: parameters, sourceRevision: OSU_REVISION, ...row });
  };

  for (const [mode, file, className] of files) {
    const content = readFileSync(join(root, file), "utf8");
    const lines = content.split(/\r?\n/);
    sources.push({
      path: `score-multiplier/${file}`,
      sha256: createHash("sha256").update(content).digest("hex"),
    });

    let sourceIndex = 0;
    for (const [lineIndex, raw] of lines.entries()) {
      const line = raw.trim();
      if (!line.startsWith("[new Mod[]")) continue;
      const expectedMatch = line.match(/\},\s*([^,\]]+(?:\s*\*\s*[^,\]]+)*)\],?$/);
      if (!expectedMatch) fail(`cannot parse ${file}:${lineIndex + 1} expected multiplier`);
      const expected = numericExpression(expectedMatch[1], `${file}:${lineIndex + 1}`);
      const mods: UnknownRecord[] = [];
      const modStarts: number[] = [];
      const classPattern = /new (?:(?:Osu|Taiko|Catch|Mania)?Mod)([A-Za-z0-9]+)(?:\(\)|\s*\{)/g;
      for (const match of line.matchAll(classPattern)) {
        const suffix = match[1];
        const keyMatch = suffix.match(/^Key(10|[1-9])$/);
        const acronym = keyMatch ? `${keyMatch[1]}K` : acronyms[suffix];
        if (!acronym) fail(`unknown multiplier mod ${suffix} at ${file}:${lineIndex + 1}`);
        modStarts.push(match.index);
        mods.push({ acronym, settings: {} });
      }
      if (mods.length === 0) fail(`no multiplier mods parsed at ${file}:${lineIndex + 1}`);
      for (const setting of line.matchAll(/(\w+)\s*=\s*\{\s*Value\s*=\s*((?:[0-9.]+)f?|true|false)\s*\}/g)) {
        const key = setting[1][0].toLowerCase() + setting[1].slice(1);
        const value = setting[2] === "true" ? true : setting[2] === "false" ? false : Number(setting[2].replace(/f$/, ""));
        const owner = modStarts.findLastIndex((start) => start < setting.index);
        if (owner < 0) fail(`setting has no owning mod at ${file}:${lineIndex + 1}`);
        record(mods[owner].settings, `settings for ${file}:${lineIndex + 1}`)[key] = value;
      }
      const parameters = { mods, expectedMultiplier: expected };
      add(className, "TestMultipliers", "test_cases", sourceIndex++, `score-multiplier/${file}:${lineIndex + 1}`,
        parameters, { id: `${mode}-source-line-${lineIndex + 1}`, mode, source: `score-multiplier/${file}:${lineIndex + 1}`,
          mods, context: null, expected });
    }

    let classicIndex = 0;
    let difficultyIndex = 0;
    for (const [lineIndex, raw] of lines.entries()) {
      const match = raw.trim().match(/^\[TestCase\((.*)\)\]$/);
      if (!match) continue;
      let method = "";
      for (let next = lineIndex + 1; next < lines.length; next++) {
        const methodMatch = lines[next].match(/public void (\w+)\(/);
        if (methodMatch) {
          method = methodMatch[1];
          break;
        }
      }
      const values = match[1].split(",").map((value) => value.trim());
      if (method === "TestClassicMultiplierVersioning") {
        if (values.length !== 2) fail(`wrong classic TestCase arity at ${file}:${lineIndex + 1}`);
        const totalScoreVersion = values[0] === "null" ? null : Number(values[0]);
        const expected = numericExpression(values[1], `${file}:${lineIndex + 1}`);
        const current = mode === "osu" ? 0.985 : 1.0;
        if (expected !== (totalScoreVersion !== null && totalScoreVersion <= 30000016 ? 0.96 : current)) {
          fail(`classic expected value disagrees with pinned source semantics at ${file}:${lineIndex + 1}`);
        }
        const parameters = { totalScoreVersion, expectedMultiplier: expected };
        add(className, method, "TestCase", classicIndex++, `score-multiplier/${file}:${lineIndex + 1}`,
          parameters, { id: `${mode}-classic-TestCase-${classicIndex - 1}`, mode,
            source: `score-multiplier/${file}:${lineIndex + 1}`, mods: [{ acronym: "CL", settings: {} }],
            context: totalScoreVersion === null ? null : { totalScoreVersion, clientVersion: "", dateUnixMs: 0 },
            expected });
      } else if (method === "TestDifficultyAdjust") {
        if (mode !== "osu" || values.length !== 5) fail(`wrong difficulty-adjust TestCase at ${file}:${lineIndex + 1}`);
        const parsed = values.map((value, index) => value === "null" ? null :
          numericExpression(value, `${file}:${lineIndex + 1} parameter ${index}`));
        const [cs, ar, od, hp, expected] = parsed;
        if (expected === null) fail(`missing difficulty-adjust expected value at ${file}:${lineIndex + 1}`);
        const parameters = { circleSize: cs, approachRate: ar, overallDifficulty: od, drainRate: hp,
          expectedMultiplier: expected };
        const settings: Record<string, Json> = {};
        for (const [key, value] of [
          ["circleSize", cs], ["approachRate", ar], ["overallDifficulty", od], ["drainRate", hp],
        ] as const) {
          if (value !== null) settings[key] = value;
        }
        add(className, method, "TestCase", difficultyIndex++, `score-multiplier/${file}:${lineIndex + 1}`,
          parameters, { id: `osu-da-TestCase-${difficultyIndex - 1}`, mode: "osu",
            source: `score-multiplier/${file}:${lineIndex + 1}`, mods: [{ acronym: "DA", settings }],
            difficulty: { circleSize: 3, approachRate: 4, overallDifficulty: 5, drainRate: 6 },
            context: null, expected });
      } else {
        fail(`unhandled pinned NUnit TestCase method ${method || "<missing>"} at ${file}:${lineIndex + 1}`);
      }
    }

    if (mode === "mania") {
      let keyIndex = 0;
      for (const [lineIndex, raw] of lines.entries()) {
        const match = raw.trim().match(
          /^\[new DateTimeOffset\((\d+),\s*(\d+),\s*(\d+),\s*(\d+),\s*(\d+),\s*(\d+)(?:,\s*0)?,\s*TimeSpan\.Zero\),\s*"([^"]*)",\s*([0-9.]+)\],?$/,
        );
        if (!match) {
          if (raw.trim().startsWith("[new DateTimeOffset(")) {
            fail(`cannot parse pinned key-mod TestCaseSource row at ${file}:${lineIndex + 1}`);
          }
          continue;
        }
        const [, year, month, day, hour, minute, second, clientVersion, expectedRaw] = match;
        const dateUnixMs = Date.UTC(Number(year), Number(month) - 1, Number(day), Number(hour), Number(minute), Number(second));
        const expected = Number(expectedRaw);
        const parameters = { endDateUnixMs: dateUnixMs, clientVersion, expectedMultiplier: expected };
        add(className, "TestKeyModMultiplierCompatibility", "key_mod_multiplier_test_cases", keyIndex++,
          `score-multiplier/${file}:${lineIndex + 1}`, parameters,
          { id: `mania-key-source-${keyIndex - 1}`, mode: "mania", source: `score-multiplier/${file}:${lineIndex + 1}`,
            mods: [{ acronym: "4K", settings: {} }],
            context: { totalScoreVersion: 30000017, clientVersion, dateUnixMs }, expected });
      }
    }
  }

  const rowIds = new Set(rows.map((row) => stringField(record(row, "pinned multiplier row"), "caseId", "pinned multiplier row")));
  const caseIds = new Set(cases.map((item) => stringField(record(item, "pinned NUnit case"), "caseId", "pinned NUnit case")));
  if (rowIds.size !== rows.length || caseIds.size !== cases.length || rowIds.size !== caseIds.size) {
    fail("pinned NUnit inventory has missing or duplicate replay rows");
  }
  for (const id of caseIds) if (!rowIds.has(id)) fail(`pinned NUnit case has no Rust replay row: ${id}`);
  for (const id of rowIds) if (!caseIds.has(id)) fail(`Rust replay row has no pinned NUnit case: ${id}`);
  return { rows, cases, sources };
}

function exhaustiveMultiplierContract(manifestPath: string): Json {
  const row = (
    id: string,
    mode: string,
    acronym: string,
    expected: number,
    settings: Record<string, Json> = {},
    context: Json = null,
  ): Json => ({ id, mode, mods: [{ acronym, settings }], context, expected });
  const rows: Json[] = [
    row("osu-ez-default", "osu", "EZ", 0.8),
    row("osu-ez-retries-3", "osu", "EZ", 0.7, { retries: 3 }),
    row("osu-ez-retries-5", "osu", "EZ", 0.5, { retries: 5 }),
    row("osu-ez-floor", "osu", "EZ", 0.4, { retries: 8 }),
    row("osu-nf", "osu", "NF", 0.5),
    row("osu-ht-050", "osu", "HT", 0.2, { speedChange: 0.5 }),
    row("osu-ht-075", "osu", "HT", 0.55, { speedChange: 0.75 }),
    row("osu-dc-099", "osu", "DC", 0.83, { speedChange: 0.99 }),
    row("osu-hr", "osu", "HR", 1.09),
    row("osu-dt-101", "osu", "DT", 1.0, { speedChange: 1.01 }),
    row("osu-dt-110", "osu", "DT", 1.036, { speedChange: 1.1 }),
    row("osu-dt-150", "osu", "DT", 1.23, { speedChange: 1.5 }),
    row("osu-nc-200", "osu", "NC", 1.45, { speedChange: 2.0 }),
    row("osu-hd", "osu", "HD", 1.04),
    row("osu-hd-fade", "osu", "HD", 1.02, { onlyFadeApproachCircles: true }),
    row("osu-tc", "osu", "TC", 1.02),
    row("osu-fl", "osu", "FL", 1.2),
    row("osu-fl-size-clamp", "osu", "FL", 1.02, { sizeMultiplier: 2.0 }),
    row("osu-fl-no-combo", "osu", "FL", 1.04, { comboBasedSize: false }),
    row("osu-bl", "osu", "BL", 1.24),
    row("osu-tp", "osu", "TP", 0.01),
    row("osu-da", "osu", "DA", 0.5, { circleSize: 5 }),
    row("osu-cl-default", "osu", "CL", 0.985),
    row("osu-cl-unlocked", "osu", "CL", 0.96, { classicNoteLock: false }),
    row("osu-rd", "osu", "RD", 0.7),
    row("osu-rx", "osu", "RX", 0.1),
    row("osu-ap", "osu", "AP", 0.1),
    row("osu-as", "osu", "AS", 0.1),
    row("osu-so", "osu", "SO", 0.95),
    row("osu-df", "osu", "DF", 0.96, { startScale: 4 }),
    row("osu-wu", "osu", "WU", 1.046),
    row("osu-wd", "osu", "WD", 0.64),
    row("osu-ad", "osu", "AD", 0.7),
    row("osu-mg-default", "osu", "MG", 0.4),
    row("osu-mg-005", "osu", "MG", 0.67, { attractionStrength: 0.05 }),
    row("osu-mg-100", "osu", "MG", 0.1, { attractionStrength: 1.0 }),
    row("osu-sy", "osu", "SY", 0.99),
    row("taiko-ez", "taiko", "EZ", 0.5),
    row("taiko-nf", "taiko", "NF", 0.5),
    row("taiko-ht", "taiko", "HT", 0.3, { speedChange: 0.75 }),
    row("taiko-dc", "taiko", "DC", 0.3, { speedChange: 0.75 }),
    row("taiko-dt", "taiko", "DT", 1.1, { speedChange: 1.5 }),
    row("taiko-nc", "taiko", "NC", 1.1, { speedChange: 1.5 }),
    row("taiko-cl-old", "taiko", "CL", 0.96, {}, { totalScoreVersion: 30000016, clientVersion: "", dateUnixMs: 1752796799999 }),
    row("taiko-cl-current", "taiko", "CL", 1.0, {}, { totalScoreVersion: 30000017, clientVersion: "", dateUnixMs: 1752796800000 }),
    row("taiko-wu", "taiko", "WU", 0.5),
    row("taiko-wd", "taiko", "WD", 0.5),
    row("taiko-sr", "taiko", "SR", 0.6),
    row("taiko-hr", "taiko", "HR", 1.06),
    row("taiko-hd", "taiko", "HD", 1.06),
    row("taiko-fl-default", "taiko", "FL", 1.12),
    row("taiko-fl-custom", "taiko", "FL", 1.0, { sizeMultiplier: 1.1 }),
    row("taiko-da", "taiko", "DA", 0.5),
    row("taiko-cs", "taiko", "CS", 0.9),
    row("taiko-rx", "taiko", "RX", 0.1),
    row("taiko-as", "taiko", "AS", 0.5),
    row("catch-ez", "catch", "EZ", 0.5),
    row("catch-nf", "catch", "NF", 0.5),
    row("catch-ht", "catch", "HT", 0.3, { speedChange: 0.75 }),
    row("catch-dc", "catch", "DC", 0.3, { speedChange: 0.75 }),
    row("catch-dt", "catch", "DT", 1.1, { speedChange: 1.5 }),
    row("catch-nc", "catch", "NC", 1.1, { speedChange: 1.5 }),
    row("catch-da", "catch", "DA", 0.5),
    row("catch-wu", "catch", "WU", 0.5),
    row("catch-wd", "catch", "WD", 0.5),
    row("catch-hr", "catch", "HR", 1.12),
    row("catch-hd", "catch", "HD", 1.06),
    row("catch-fl-default", "catch", "FL", 1.12),
    row("catch-sy", "catch", "SY", 0.8),
    row("mania-ez", "mania", "EZ", 0.5),
    row("mania-nf", "mania", "NF", 0.5),
    row("mania-ht", "mania", "HT", 0.3, { speedChange: 0.75 }),
    row("mania-dc", "mania", "DC", 0.3, { speedChange: 0.75 }),
    row("mania-wd", "mania", "WD", 0.5),
    row("mania-cl-old", "mania", "CL", 0.96, {}, { totalScoreVersion: 30000016, clientVersion: "", dateUnixMs: 1752796799999 }),
    row("mania-cl-current", "mania", "CL", 1.0, {}, { totalScoreVersion: 30000017, clientVersion: "", dateUnixMs: 1752796800000 }),
    row("mania-nr", "mania", "NR", 0.9),
    row("mania-cs", "mania", "CS", 0.9),
    row("mania-ho", "mania", "HO", 0.9),
    row("mania-da", "mania", "DA", 0.5),
    row("mania-wu", "mania", "WU", 0.5),
    row("mania-as", "mania", "AS", 0.5),
    row("classic-old", "catch", "CL", 0.96, {}, { totalScoreVersion: 30000016, clientVersion: "", dateUnixMs: 1752796799999 }),
    row("classic-current", "catch", "CL", 1.0, {}, { totalScoreVersion: 30000017, clientVersion: "", dateUnixMs: 1752796800000 }),
    row("mania-key-old-client", "mania", "7K", 1.0, {}, { totalScoreVersion: 30000017, clientVersion: "2025.717.0", dateUnixMs: 0 }),
    row("mania-key-current-client", "mania", "7K", 0.9, {}, { totalScoreVersion: 30000017, clientVersion: "2025.718.0", dateUnixMs: 0 }),
    row("mania-key-old-date", "mania", "4K", 1.0, {}, { totalScoreVersion: 30000017, clientVersion: "", dateUnixMs: 1752796799999 }),
    row("mania-key-current-date", "mania", "4K", 0.9, {}, { totalScoreVersion: 30000017, clientVersion: "", dateUnixMs: 1752796800000 }),
  ];
  rows.push(
    { id: "osu-combo-hd-hr", mode: "osu", mods: [
      { acronym: "HD", settings: {} }, { acronym: "HR", settings: {} },
    ], context: null, expected: 1.04 * 1.09 },
    { id: "osu-combo-hd-bl-consumes-hd", mode: "osu", mods: [
      { acronym: "HD", settings: {} }, { acronym: "BL", settings: {} },
    ], context: null, expected: 1.24 },
    { id: "osu-combo-hd-df", mode: "osu", mods: [
      { acronym: "HD", settings: {} }, { acronym: "DF", settings: { startScale: 4 } },
    ], context: null, expected: 1.02 * 0.96 },
    { id: "osu-combo-fl-ff", mode: "osu", mods: [
      { acronym: "FL", settings: {} }, { acronym: "FF", settings: {} },
    ], context: null, expected: 1.1 },
    { id: "osu-combo-hd-bl-hr", mode: "osu", mods: [
      { acronym: "HD", settings: {} }, { acronym: "BL", settings: {} }, { acronym: "HR", settings: {} },
    ], context: null, expected: 1.24 * 1.09 },
    { id: "taiko-combo-hd-hr", mode: "taiko", mods: [
      { acronym: "HD", settings: {} }, { acronym: "HR", settings: {} },
    ], context: null, expected: 1.06 * 1.06 },
    { id: "catch-combo-hd-hr", mode: "catch", mods: [
      { acronym: "HD", settings: {} }, { acronym: "HR", settings: {} },
    ], context: null, expected: 1.06 * 1.12 },
  );
  const pinned = pinnedMultiplierSourceRows(manifestPath);
  rows.push(...pinned.rows);
  return jsonValue({
    id: "exhaustive-score-multiplier-matrix",
    actions: [
      "enumerate every pinned NUnit TestCase and TestCaseSource parameter set with a stable case identity",
      "reject missing, duplicate, or extra pinned NUnit cases and Rust replay rows",
      "replay each distinct multiplier branch, setting boundary, combination family, and score-context boundary in Rust",
    ],
    expected: {
      sourceRevision: OSU_REVISION,
      sourceEvidence: pinned.sources,
      pinnedCaseCount: pinned.cases.length,
      pinnedCases: pinned.cases,
      rows,
    },
  }, "exhaustive multiplier matrix");
}

function validatePinnedMultiplierCompleteness(manifestPath: string, fixture: UnknownRecord): number {
  const contracts = arrayField(fixture, "contracts", "committed fixture");
  const rawContract = contracts.find((value) =>
    record(value, "committed contract").id === "exhaustive-score-multiplier-matrix");
  if (rawContract === undefined) fail("committed fixture omitted exhaustive score multiplier matrix");
  const contract = record(rawContract, "committed multiplier contract");
  const expected = record(contract.expected, "committed multiplier contract.expected");
  if (stringField(expected, "sourceRevision", "committed multiplier contract.expected") !== OSU_REVISION) {
    fail("committed multiplier inventory has the wrong pinned source revision");
  }

  const pinned = pinnedMultiplierSourceRows(manifestPath);
  if (JSON.stringify(expected.sourceEvidence) !== JSON.stringify(pinned.sources)) {
    fail("committed multiplier source evidence differs from the pinned source files");
  }
  if (expected.pinnedCaseCount !== pinned.cases.length) {
    fail(`committed pinned NUnit case count is ${String(expected.pinnedCaseCount)}; expected ${pinned.cases.length}`);
  }

  const committedCases = arrayField(expected, "pinnedCases", "committed multiplier contract.expected");
  const committedRows = arrayField(expected, "rows", "committed multiplier contract.expected")
    .filter((value) => record(value, "committed multiplier row").caseId !== undefined);
  const ids = (values: unknown[], label: string): string[] => values.map((value, index) =>
    stringField(record(value, `${label}[${index}]`), "caseId", `${label}[${index}]`));
  const exact = (actual: string[], wanted: string[], label: string): void => {
    const seen = new Set<string>();
    for (const id of actual) {
      if (seen.has(id)) fail(`duplicate ${label}: ${id}`);
      seen.add(id);
    }
    const wantedSet = new Set(wanted);
    for (const id of wanted) if (!seen.has(id)) fail(`missing ${label}: ${id}`);
    for (const id of actual) if (!wantedSet.has(id)) fail(`extra ${label}: ${id}`);
  };
  const expectedIds = ids(pinned.cases, "source-derived pinned NUnit cases");
  exact(ids(committedCases, "committed pinned NUnit cases"), expectedIds, "pinned NUnit case");
  exact(ids(committedRows, "committed pinned Rust replay rows"), expectedIds, "pinned Rust replay row");
  if (JSON.stringify(committedCases) !== JSON.stringify(pinned.cases)) {
    fail("committed pinned NUnit parameter sets differ from exact source enumeration");
  }
  if (JSON.stringify(committedRows) !== JSON.stringify(pinned.rows)) {
    fail("committed pinned Rust replay rows differ from exact source enumeration");
  }
  return pinned.cases.length;
}

function canonicalConversionContracts(manifestPath: string): Json[] {
  const root = join(dirname(manifestPath), "inputs/osu-97e94bab");
  const cases: ReadonlyArray<readonly [string, string, string, string]> = [
    ["mania", "ds", "basic.osu", "basic-expected-conversion.json"],
    ["mania", "in", "1K.osu", "1K-expected-conversion.json"],
    ["mania", "rd-seeded", "7K.osu", "7K-expected-conversion.json"],
    ["mania", "tp-seeded", "10K.osu", "10K-expected-conversion.json"],
    ["mania", "native-legacy-spinner-short", "mania-specific-spinner.osu", "mania-specific-spinner-expected-conversion.json"],
    ["mania", "spinner-hold", "zero-length-slider.osu", "zero-length-slider-expected-conversion.json"],
    ["mania", "simultaneous", "18K.osu", "18K-expected-conversion.json"],
    ["catch", "hyperdash", "basic-hyperdash.osu", "basic-hyperdash-expected-conversion.json"],
    ["catch", "linear-slider", "slider.osu", "slider-expected-conversion.json"],
    ["catch", "tiny-droplets", "tiny-ticks.osu", "tiny-ticks-expected-conversion.json"],
    ["catch", "nested-count", "spinner-and-circles.osu", "spinner-and-circles-expected-conversion.json"],
    ["catch", "precision", "high-speed-multiplier-precision.osu", "high-speed-multiplier-precision-expected-conversion.json"],
    ["taiko", "base-conversion", "basic.osu", "basic-expected-conversion.json"],
    ["taiko", "sample-type", "sample-to-type-conversions.osu", "sample-to-type-conversions-expected-conversion.json"],
    ["taiko", "slider-v14", "slider-conversion-v14.osu", "slider-conversion-v14-expected-conversion.json"],
  ];
  return cases.map(([mode, branch, map, expected]) => {
    const expectedValue = JSON.parse(readFileSync(join(root, mode, expected), "utf8")) as unknown;
    const expectedRecord = record(expectedValue, `${mode}/${expected}`);
    if (Object.keys(expectedRecord).length === 0) fail(`empty canonical conversion oracle: ${mode}/${expected}`);
    return jsonValue({
      id: `canonical-conversion-${mode}-${branch}`,
      kind: "canonical-conversion",
      actions: ["decode pinned osu! input", "run pinned target ruleset converter", "compare exact expected object mappings"],
      sourceRevision: OSU_REVISION,
      input: `${mode}/${map}`,
      expectedSource: `${mode}/${expected}`,
      expected: expectedValue,
    }, `${mode} ${branch} conversion`);
  });
}

if (gitRevision(options.source) !== SOURCE_REVISION) fail("wrong pinned tosu source revision");
validateReference(options.calculatorSource);
validateOracleInputs(options.manifest);
const manifest = validateManifest(options.manifest);
const csharpEvidencePath = join(dirname(options.out), "csharp-test-evidence.json");
const csharpEvidence = validateCsharpEvidence(csharpEvidencePath);
if (options.verifyOnly) {
  if (!existsSync(options.out)) fail(`fixture does not exist: ${options.out}`);
  const committed = record(JSON.parse(readFileSync(options.out, "utf8")) as unknown, "committed fixture");
  const oracle = record(committed.oracle, "committed fixture.oracle");
  const embeddedEvidence = record(oracle.csharpTestEvidence, "committed fixture.oracle.csharpTestEvidence");
  if (JSON.stringify(embeddedEvidence) !== JSON.stringify(csharpEvidence)) {
    fail("committed fixture C# evidence differs from the externally executed evidence");
  }
  validateCoverage(manifest, committed);
  const pinnedCaseCount = validatePinnedMultiplierCompleteness(options.manifest, committed);
  console.log(`validated ${arrayField(manifest, "items", "manifest").length} manifest items, ${pinnedCaseCount} exact pinned NUnit parameter cases with one Rust replay each, exact external C# evidence, and committed fixture coverage`);
} else {
  const bindingPath = options.binding ?? join(options.calculatorSource, "lib/native/dist/binding.node");
  if (!existsSync(bindingPath)) fail(`C# oracle binding not installed: ${bindingPath}`);
  if (options.binding !== undefined) {
    const bindingPackagePath = join(dirname(bindingPath), "package.json");
    if (!existsSync(bindingPackagePath)) {
      fail(`explicit C# binding is missing adjacent package.json: ${bindingPackagePath}`);
    }
    const bindingPackage = record(
      JSON.parse(readFileSync(bindingPackagePath, "utf8")) as unknown,
      "installed C# binding package",
    );
    if (stringField(bindingPackage, "version", "installed C# binding package") !== CALCULATOR_PACKAGE) {
      fail("wrong installed C# binding package version");
    }
  }
  const require = createRequire(import.meta.url);
  const binding = record(require(bindingPath) as unknown, "C# oracle binding");
  const playType = binding.PlayBeatmap;
  if ((typeof playType !== "object" || playType === null) && typeof playType !== "function") {
    fail("C# oracle PlayBeatmap export must be an object or constructor");
  }
  const parse = Reflect.get(playType, "parse") as unknown;
  if (typeof parse !== "function") fail("C# oracle binding is missing PlayBeatmap.parse()");
  const content = readFileSync(options.map, "utf8");
  const beatmap = record(Reflect.apply(parse, playType, [content]) as unknown, "parsed beatmap");
  const modes: ReadonlyArray<readonly [number, string]> = [[0, "osu"], [1, "taiko"], [2, "catch"], [3, "mania"]];
  const variants = ["base", "dt125", "ht075", "classic", "difficulty-adjust", "mirror-string"];
  const vectors = modes.flatMap(([mode, name]) => variants.map((variant) => oracleVector(beatmap, mode, name, variant)));
  const oracleInputRoot = join(dirname(options.manifest), "inputs/osu-97e94bab");
  for (const [mode, name] of modes) {
    const mapName = `${name}-diffcalc-test.osu`;
    const richContent = readFileSync(join(oracleInputRoot, mapName), "utf8");
    const richBeatmap = record(Reflect.apply(parse, playType, [richContent]) as unknown, `${name} rich beatmap`);
    vectors.push(oracleVector(richBeatmap, mode, name, "branch-rich", mapName));
    if (name === "taiko" || name === "mania") {
      vectors.push(oracleVector(richBeatmap, mode, name, "random-seeded", mapName));
    }
    const playableVariants: Record<string, string[]> = {
      osu: ["freeze-frame"],
      taiko: [
        "simplified-one-third", "simplified-one-sixth", "simplified-one-eighth",
        "simplified-combined", "swap", "swap-duplicate",
      ],
      mania: ["hold-off", "invert", "mania-mirror", "mania-transform-order", "classic-score-v2"],
      catch: [],
    };
    for (const variant of playableVariants[name]) {
      vectors.push(oracleVector(richBeatmap, mode, name, variant, mapName));
    }
  }
  vectors.push(oracleVector(beatmap, 0, "osu", "numeric-string"));
  for (const [branch, variant] of [
    ["snap", "base"],
    ["flow", "base"],
    ["agility", "base"],
    ["speed-rhythm", "base"],
    ["reading-fl", "hidden-reading"],
    ["reading-fl", "flashlight-branch"],
    ["sliders", "base"],
    ["sections", "base"],
  ] as const) {
    const mapName = `osu-branches/${branch}.osu`;
    const branchContent = readFileSync(join(oracleInputRoot, mapName), "utf8");
    const branchBeatmap = record(Reflect.apply(parse, playType, [branchContent]) as unknown, `osu ${branch} beatmap`);
    vectors.push(oracleVector(branchBeatmap, 0, "osu", variant, mapName, `osu-${branch}-${variant}`, false));
  }
  {
    const mapName = "osu-branches/aggregate-skills.osu";
    const aggregateContent = readFileSync(join(oracleInputRoot, mapName), "utf8");
    const aggregate = record(
      Reflect.apply(parse, playType, [aggregateContent]) as unknown,
      "osu aggregate skills beatmap",
    );
    for (const variant of ["base", "branch-rich", "dt125"]) {
      vectors.push(oracleVector(
        aggregate, 0, "osu", variant, mapName, `osu-aggregate-skills-${variant}`, false,
      ));
    }
  }
  const invalidConversion = (() => {
    try {
      return { throws: false, isNull: call(beatmap, "convert", [99]) === null };
    } catch {
      return { throws: true, isNull: false };
    }
  })();
  const observe = (value: Json): Json => {
    const scenario = record(value, "generated scenario");
    return jsonValue({
      ...scenario,
      observedCapabilities: [...derivedCapabilities(scenario)].sort(),
    }, `observed scenario ${stringField(scenario, "id", "generated scenario")}`);
  };
  const contracts = [
    {
      id: "unsupported-conversion-null",
      actions: ["PlayBeatmap.convert(99)"],
      expected: invalidConversion,
    },
    {
      id: "gradual-prefix-zero-attrs",
      actions: ["PlayBeatmap.createGradualDifficulty on a non-empty map", "GradualDifficulty.createDifficultyAttrs before Advance"],
      expected: {
        throws: (() => {
          const gradual = call(beatmap, "createGradualDifficulty");
          try {
            call(gradual, "createDifficultyAttrs");
            return false;
          } catch {
            return true;
          }
        })(),
      },
    },
    {
      id: "hit-window-boundaries",
      actions: ["HitWindows.getGreatHitWindow at ruleset/OD boundaries"],
      expected: {
        osuGreatOd5: staticCall(binding.HitWindows, "getGreatHitWindow", [0, 5]),
        taikoGreatOd10: staticCall(binding.HitWindows, "getGreatHitWindow", [1, 10]),
        catchGreatOd5: staticCall(binding.HitWindows, "getGreatHitWindow", [2, 5]),
        maniaGreatOd0: staticCall(binding.HitWindows, "getGreatHitWindow", [3, 0]),
      },
    },
    wrapperActionContract(beatmap),
    wrapperDisplayAndRetainedAttrsContract(beatmap),
    gradualOsuPartialStrainsContract(beatmap),
    randomSeedPersistenceContract(beatmap),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 3, "mania", "mania-individual-strain-pair",
      "mania-branches/individual-jack.osu", "mania-branches/individual-alternate.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 3, "mania", "mania-overall-strain-pair",
      "mania-branches/overall-chord.osu", "mania-branches/overall-separated.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 3, "mania", "mania-hold-factor-pair",
      "mania-branches/hold-nested.osu", "mania-branches/hold-control.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 1, "taiko", "taiko-group-logistic-pair",
      "taiko-branches/group-two-left.osu", "taiko-branches/group-two-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 1, "taiko", "taiko-long-gap-pair",
      "taiko-branches/long-gap-left.osu", "taiko-branches/long-gap-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 1, "taiko", "taiko-reading-band-pair",
      "taiko-branches/reading-band-left.osu", "taiko-branches/reading-band-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 1, "taiko", "taiko-rhythm-island-pair",
      "taiko-branches/island-left.osu", "taiko-branches/island-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 1, "taiko", "taiko-mono-stamina-pair",
      "taiko-branches/mono-left.osu", "taiko-branches/mono-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 1, "taiko", "taiko-nonhit-pair",
      "taiko-branches/nonhit-left.osu", "taiko-branches/nonhit-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 2, "catch", "catch-movement-pair",
      "catch-branches/movement-left.osu", "catch-branches/movement-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 2, "catch", "catch-direction-pair",
      "catch-branches/direction-left.osu", "catch-branches/direction-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 2, "catch", "catch-hyperdash-pair",
      "catch-branches/hyperdash-left.osu", "catch-branches/hyperdash-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 2, "catch", "catch-cs-pair",
      "catch-branches/cs-left.osu", "catch-branches/cs-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 2, "catch", "catch-sections-pair",
      "catch-branches/sections-left.osu", "catch-branches/sections-right.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 3, "mania", "mania-section-peaks-pair",
      "mania-branches/section-peaks-4k.osu", "mania-branches/performance-4k.osu",
    ),
    pairedRulesetContract(
      parse, playType, oracleInputRoot, 3, "mania", "mania-column-count-pair",
      "mania-branches/column-count-4k.osu", "mania-branches/column-count-7k.osu",
    ),
    maniaPerformanceContract(parse, playType, oracleInputRoot),
    taikoPerformanceContract(parse, playType, oracleInputRoot),
    osuPerformanceContract(parse, playType, oracleInputRoot),
    ...modes.map(([mode, name]) => gradualActionContract(beatmap, mode, name)),
    generatedScoreContract(beatmap, 0, "osu", 0.25),
    generatedScoreContract(beatmap, 0, "osu", 1 / 6),
    generatedScoreContract(beatmap, 1, "taiko", 0.2),
    generatedScoreContract(beatmap, 1, "taiko", 1.2),
    generatedScoreContract(beatmap, 2, "catch", 0),
    generatedScoreContract(beatmap, 2, "catch", 1),
    generatedScoreContract(beatmap, 3, "mania", 0.95),
    generatedScoreContract(beatmap, 3, "mania", 0.95, "classic"),
    ...modes.map(([mode, name]) => customScoreContract(beatmap, mode, name)),
    ...modes.map(([mode, name]) => multiplierContract(beatmap, mode, name)),
    exhaustiveMultiplierContract(options.manifest),
    ...canonicalConversionContracts(options.manifest),
  ];
  const fixture = {
    schema: 1,
    oracle: {
      calculatorRevision: CALCULATOR_REVISION,
      osuRevision: OSU_REVISION,
      rosuBaseline: ROSU_BASELINE,
      packageVersion: CALCULATOR_PACKAGE,
      csharpTestEvidence: csharpEvidence,
    },
    map: "fixture.osu",
    vectors: vectors.map(observe),
    contracts: contracts.map(observe),
  };
  validateCoverage(manifest, fixture);
  writeFileSync(options.out, `${JSON.stringify(fixture, null, 2)}\n`);
  console.log(`wrote ${vectors.length} C# oracle vectors and ${contracts.length} contracts to ${options.out}`);
  process.exit(0);
}

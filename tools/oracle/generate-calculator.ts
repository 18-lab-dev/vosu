import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createRequire } from "node:module";

import { SOURCE_REVISION, OSU_REVISION, CALCULATOR_REVISION, CALCULATOR_PACKAGE, ROSU_BASELINE, Json, fail, record, stringField, arrayField, call, staticCall, jsonValue } from "./calculator/binding.ts";
import { gitRevision, validateReference, validateOracleInputs, validateCsharpEvidence, validateManifest, derivedCapabilities, validateCoverage } from "./calculator/validation.ts";
import { oracleVector, wrapperActionContract, wrapperDisplayAndRetainedAttrsContract, gradualOsuPartialStrainsContract, pairedRulesetContract, maniaPerformanceContract, taikoPerformanceContract, osuPerformanceContract, randomSeedPersistenceContract, gradualActionContract, customScoreContract, generatedScoreContract, multiplierContract } from "./calculator/contracts.ts";
import { exhaustiveMultiplierContract, validatePinnedMultiplierCompleteness, canonicalConversionContracts } from "./calculator/pinned.ts";

interface Options {
  source: string;
  calculatorSource: string;
  manifest: string;
  map: string;
  out: string;
  binding?: string;
  verifyOnly: boolean;
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
    map: resolve(values.get("map") ?? "crates/vosu-pp/tests/fixture.osu"),
    out: resolve(required("out")),
    binding: values.has("binding") ? resolve(required("binding")) : undefined,
    verifyOnly,
  };
}




const options = parseArgs(process.argv.slice(2));


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

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { OSU_REVISION, Json, UnknownRecord, fail, record, stringField, arrayField, jsonValue } from "./binding.ts";

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

export function exhaustiveMultiplierContract(manifestPath: string): Json {
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

export function validatePinnedMultiplierCompleteness(manifestPath: string, fixture: UnknownRecord): number {
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

export function canonicalConversionContracts(manifestPath: string): Json[] {
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

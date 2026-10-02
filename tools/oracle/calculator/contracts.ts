import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { OSU_REVISION, Json, UnknownRecord, fail, record, stringField, arrayField, call, jsonValue } from "./binding.ts";

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

export function oracleVector(playBeatmap: UnknownRecord, mode: number, name: string, variant: string, map = "fixture.osu", vectorId = `${name}-${variant}`, includeStrains = true): Json {
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

function fullAttrs(playBeatmap: UnknownRecord): unknown {
  const gradual = call(playBeatmap, "createGradualDifficulty");
  call(gradual, "skipToEnd");
  return call(gradual, "createDifficultyAttrs");
}

export function wrapperActionContract(playBeatmap: UnknownRecord): Json {
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

export function wrapperDisplayAndRetainedAttrsContract(playBeatmap: UnknownRecord): Json {
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

export function gradualOsuPartialStrainsContract(playBeatmap: UnknownRecord): Json {
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

export function pairedRulesetContract(
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

export function maniaPerformanceContract(
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

export function taikoPerformanceContract(
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

export function osuPerformanceContract(
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

export function randomSeedPersistenceContract(playBeatmap: UnknownRecord): Json {
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

export function gradualActionContract(playBeatmap: UnknownRecord, mode: number, name: string): Json {
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

export function customScoreContract(playBeatmap: UnknownRecord, mode: number, name: string): Json {
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

export function generatedScoreContract(
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

export function multiplierContract(manifestPath: string, csharpEvidence: UnknownRecord, name: string): Json {
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
  const sourcePath = join(dirname(manifestPath), "inputs/osu-97e94bab", sourceFile);
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

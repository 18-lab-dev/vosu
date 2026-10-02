import { execFileSync } from 'node:child_process';
import { readFileSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createRequire, registerHooks } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

interface Options {
    source: string;
    calculatorSource: string;
    out: string;
}

interface Lock {
    tosu: string;
    lazerCalculator: string;
    osu: string;
    calculatorPackage: string;
    calculatorTree: string;
}

interface ScenarioSpec {
    id: string;
    tags: string[];
    source?: 'stable' | 'lazer';
    status?: number;
    mode?: number;
    mods?: ModSet;
    bpm?: [number, number, number];
    leaderboard?: boolean;
    preciseShort?: boolean;
    tournament?: 'stable' | 'lazer';
}

interface ModSetting { acronym: string; settings?: Record<string, string | number | boolean>; }
interface ModSet { checksum: string; number: number; name: string; array: ModSetting[]; rate: number; }
interface InputFixture extends ScenarioSpec { services: Services; preciseClients: PreciseClient[]; }
interface PreciseClient { ipcId: number; keys: Key[]; }
interface Key { name: string; isPressed: boolean; count: number; }
interface Statistics { perfect: number; great: number; good: number; ok: number; meh: number; miss: number; smallTickMiss: number; smallTickHit: number; largeTickMiss: number; largeTickHit: number; smallBonus: number; largeBonus: number; ignoreMiss: number; ignoreHit: number; comboBreak: number; sliderTailHit: number; legacyComboIncrease: number; }
interface Services { [name: string]: unknown; global: Record<string, unknown>; menu: Record<string, unknown>; gameplay: Record<string, unknown>; resultScreen: Record<string, unknown>; beatmapPP: Record<string, unknown>; settings: Record<string, unknown>; user: Record<string, unknown>; bassDensity: Record<string, unknown>; rankedPlay: Record<string, unknown>; tourneyManager: Record<string, unknown>; lazerMultiSpectating: Record<string, unknown>; }
interface SourceInstance { client: number; customServerEndpoint?: string; isTourneyManager: boolean; isTourneySpectator: boolean; ipcId: number; getServices(names: string[]): Record<string, unknown>; }
interface SourceManager { focusedClient: number | undefined; gameFocused: boolean; osuInstances: Record<string, SourceInstance>; getInstance(client: number | undefined): SourceInstance | undefined; }
interface InventoryEntry { id: string; tags: string[]; input: string; expected: Record<string, string>; }
interface Inventory { format: number; revisions: Lock; requiredCoverage: string[]; cases: InventoryEntry[]; }

function parseOptions(argv: string[]): Options {
    const values = new Map<string, string>();
    for (let index = 0; index < argv.length; index += 2) {
        const key = argv[index];
        const value = argv[index + 1];
        if (!key?.startsWith('--') || value === undefined) throw new Error(`invalid argument list near ${key ?? '<end>'}`);
        values.set(key.slice(2), value);
    }
    const source = values.get('source');
    const calculatorSource = values.get('calculator-source');
    const out = values.get('out');
    if (!source || !calculatorSource || !out) throw new Error('usage: generate.ts --source PATH --calculator-source PATH --out PATH');
    return { source: resolve(source), calculatorSource: resolve(calculatorSource), out: resolve(out) };
}

function parseJson<T>(path: string): T {
    return JSON.parse(readFileSync(path, 'utf8')) as T;
}


function verifyPins(options: Options, lock: Lock): void {
    const sourceRevision = execFileSync('git', ['-C', options.source, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
    if (sourceRevision !== lock.tosu) throw new Error(`tosu source revision mismatch: expected ${lock.tosu}, got ${sourceRevision}`);
    execFileSync('git', ['-C', options.source, 'diff', '--exit-code', 'HEAD', '--']);
    const resolveFromSource = createRequire(join(options.source, 'packages/tosu/package.json'));
    const installedPackagePath = resolveFromSource.resolve('@tosuapp/lazer-calculator-prebuilt/package.json');
    const installedPackage = parseJson<{ version: string }>(installedPackagePath);
    if (installedPackage.version !== lock.calculatorPackage) {
        throw new Error(`installed C# calculator package mismatch: expected ${lock.calculatorPackage}, got ${installedPackage.version}`);
    }

    const calculatorPackage = parseJson<{ version: string; osu: { revision: string } }>(join(options.calculatorSource, 'lib/package.json'));
    if (calculatorPackage.version !== lock.calculatorPackage) throw new Error(`calculator package mismatch: expected ${lock.calculatorPackage}, got ${calculatorPackage.version}`);
    if (calculatorPackage.osu.revision !== lock.osu) throw new Error(`osu revision mismatch: expected ${lock.osu}, got ${calculatorPackage.osu.revision}`);

    const calculatorRevision = execFileSync('git', ['-C', options.calculatorSource, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
    const calculatorTree = execFileSync('git', ['-C', options.calculatorSource, 'rev-parse', 'HEAD^{tree}'], { encoding: 'utf8' }).trim();
    if (calculatorRevision !== lock.lazerCalculator || calculatorTree !== lock.calculatorTree) {
        throw new Error('calculator checkout revision/tree mismatch');
    }
    execFileSync('git', ['-C', options.calculatorSource, 'diff', '--exit-code', 'HEAD', '--']);
}

const zeroStats = (): Statistics => ({ perfect: 0, great: 0, good: 0, ok: 0, meh: 0, miss: 0, smallTickMiss: 0, smallTickHit: 0, largeTickMiss: 0, largeTickHit: 0, smallBonus: 0, largeBonus: 0, ignoreMiss: 0, ignoreHit: 0, comboBreak: 0, sliderTailHit: 0, legacyComboIncrease: 0 });
const noMods = (): ModSet => ({ checksum: '', number: 0, name: '', array: [], rate: 1 });

function canonicalServices(): Services {
    const mods = noMods();
    const stats = zeroStats();
    const leaderboardPlayer = { name: '', score: 0, combo: 0, maxCombo: 0, mods, statistics: stats, team: 0, position: 0, isPassing: false };
    return {
        bassDensity: { density: 0 },
        global: { isWatchingReplay: false, isReplayUiHidden: false, isMultiSpectating: false, showInterface: false, chatStatus: 0, status: 0, paused: false, gameTime: 0, playTime: 0, menuMods: mods, gameFolder: '', skinFolder: '', songsFolder: '' },
        menu: { gamemode: 0, artist: '', artistOriginal: '', title: '', titleOriginal: '', audioFilename: '', backgroundFilename: '', folder: '', creator: '', filename: '', difficulty: '', mapID: 0, setID: 0, checksum: '', rankedStatus: 0, mp3Length: 0 },
        gameplay: { failed: false, retries: 0, playerName: '', mode: 0, mods, score: 0, playerHP: 0, playerHPSmooth: 0, accuracy: 100, statistics: stats, maximumStatistics: stats, combo: 0, maxCombo: 0, hitSB: 0, unstableRate: 0, gradeCurrent: '', gradeExpected: '', keyOverlay: [], hitErrors: [], isLeaderboardVisible: false, leaderboardPlayer, leaderboardScores: [], isReplayUiHidden: false },
        resultScreen: { onlineId: 0, playerName: '', mods, mode: 0, maxCombo: 0, score: 0, statistics: stats, maximumStatistics: stats, grade: '', date: '', accuracy: 0, pp: 0, fcPP: 0 },
        beatmapPP: { mode: 0, isKiai: false, isBreak: false, timings: { firstObj: 0, full: 0 }, previewtime: 0, ppAcc: Object.fromEntries(Array.from({ length: 11 }, (_, index) => [90 + index, 0])), strains: [], strainsAll: { series: [], xaxis: [] }, calculatedMapAttributes: { ar: 0, arConverted: 0, cs: 0, csConverted: 0, hp: 0, hpConverted: 0, od: 0, odConverted: 0, circles: 0, sliders: 0, spinners: 0, holds: 0, maxCombo: 0, fullStars: 0, stars: 0, hitWindow: {} }, currAttributes: { stars: 0, pp: 0, fcPP: 0, maxAchievable: 0 }, currPPAttributes: { ppAim: 0, ppSpeed: 0, ppAccuracy: 0, ppDifficulty: 0, ppFlashlight: 0, pp: 0 }, fcPPAttributes: { ppAim: 0, ppSpeed: 0, ppAccuracy: 0, ppDifficulty: 0, ppFlashlight: 0, pp: 0 }, realtimeBPM: 0, commonBPM: 0, minBPM: 0, maxBPM: 0, breaks: [], timingPoints: [] },
        settings: { leaderboardType: 0, progressBarType: 0, sortType: 0, groupType: 0, resolution: { fullscreen: false, width: 0, height: 0, widthFullscreen: 0, heightFullscreen: 0 }, client: { updateAvailable: false, branch: 0, version: '' }, scoreMeter: { type: 0, size: 0 }, cursor: { useSkinCursor: false, autoSize: false, size: 0, menuSize: 0 }, mouse: { rawInput: false, disableButtons: false, disableWheel: false, sensitivity: 0 }, tablet: { enabled: false, x: 0, y: 0, width: 0, height: 0, rotation: 0, pressureThreshold: 0 }, mania: { speedBPMScale: false, usePerBeatmapSpeedScale: false, scrollSpeed: 0, scrollDirection: 0 }, skin: { useDefaultSkinInEditor: false, ignoreBeatmapSkins: false, tintSliderBall: false, useTaikoSkin: false, name: '' }, audio: { ignoreBeatmapSounds: false, useSkinSamples: false, volume: { masterInactive: 0, master: 0, music: 0, effect: 0 }, offset: { universal: 0 } }, background: { storyboard: false, video: false, blur: 0, dim: 0 }, keybinds: { osu: { k1: '', k2: '', smokeKey: '' }, fruits: { k1: '', k2: '', Dash: '' }, taiko: { innerLeft: '', innerRight: '', outerLeft: '', outerRight: '' }, quickRetry: '' } },
        user: { rawLoginStatus: 0, rawBanchoStatus: 0, id: 0, name: '', playMode: 0, rankedScore: 0, level: 0, accuracy: 0, performancePoints: 0, playCount: 0, rank: 0, countryCode: 0, backgroundColour: 0, matchmaking: null },
        rankedPlay: { info: undefined },
        room: { info: undefined },
        tourneyManager: { ipcState: 0, leftStars: 0, rightStars: 0, bestOf: 0, starsVisible: false, scoreVisible: false, firstTeamName: '', secondTeamName: '', firstTeamScore: 0, secondTeamScore: 0, messages: [], userAccuracy: 0, userRankedScore: 0, userPlayCount: 0, userGlobalRank: 0, userPP: 0, userName: '', userCountry: '', userID: 0 },
        lazerMultiSpectating: { lazerSpectatingData: undefined }
    };
}

const scenarios: ScenarioSpec[] = [
    { id: 'no-client', tags: ['no-client'] },
    { id: 'stable-defaults', tags: ['defaults', 'stable', 'menu', 'leaderboard-empty', 'known-omissions', 'placeholders'], source: 'stable' },
    { id: 'lazer-menu-hd', tags: ['lazer', 'menu', 'HD'], source: 'lazer', mods: { checksum: 'hd', number: 8, name: 'HD', array: [{ acronym: 'HD' }], rate: 1 } },
    { id: 'stable-play-osu-fl', tags: ['play', 'stable', 'osu', 'FL', 'leaderboard-populated'], source: 'stable', status: 2, mode: 0, mods: { checksum: 'fl', number: 1024, name: 'FL', array: [{ acronym: 'FL' }], rate: 1 }, leaderboard: true },
    { id: 'lazer-play-taiko-dt', tags: ['play', 'lazer', 'taiko', 'DT', 'structured-speed'], source: 'lazer', status: 2, mode: 1, mods: { checksum: 'dt', number: 64, name: 'DT', array: [{ acronym: 'DT', settings: { speed_change: 1.25 } }], rate: 1.25 } },
    { id: 'stable-result-catch-ht', tags: ['result', 'stable', 'catch', 'HT'], source: 'stable', status: 7, mode: 2, mods: { checksum: 'ht', number: 256, name: 'HT', array: [{ acronym: 'HT' }], rate: 0.75 } },
    { id: 'lazer-play-mania-cl', tags: ['play', 'lazer', 'mania', 'CL'], source: 'lazer', status: 2, mode: 3, mods: { checksum: 'cl', number: 0, name: 'CL', array: [{ acronym: 'CL' }], rate: 1 } },
    { id: 'lazer-menu-da', tags: ['lazer', 'menu', 'DA'], source: 'lazer', mods: { checksum: 'da', number: 0, name: 'DA', array: [{ acronym: 'DA', settings: { approach_rate: 8.5, circle_size: 4.2 } }], rate: 1 } },
    { id: 'stable-tournament', tags: ['stable-tournament', 'stable', 'multi-IPC'], source: 'stable', tournament: 'stable' },
    { id: 'lazer-lobby-tournament', tags: ['lazer-tournament', 'lazer', 'lobby', 'multi-IPC'], source: 'lazer', status: 11, tournament: 'lazer' },
    { id: 'precise-short-keys', tags: ['precise-short-keys'], source: 'stable', status: 2, preciseShort: true },
    { id: 'sc-equal-bpm', tags: ['SC-equal-BPM'], source: 'stable', bpm: [180, 180, 180] },
    { id: 'sc-range-bpm', tags: ['SC-range-BPM'], source: 'stable', bpm: [120, 240, 180] }
];

function materialize(spec: ScenarioSpec): InputFixture {
    const services = canonicalServices();
    const status = spec.status ?? 0;
    const mode = spec.mode ?? 0;
    services.global.status = status;
    services.menu.gamemode = mode;
    services.gameplay.mode = mode;
    services.resultScreen.mode = mode;
    services.beatmapPP.mode = mode;
    if (spec.mods) {
        services.global.menuMods = spec.mods;
        services.gameplay.mods = spec.mods;
        services.resultScreen.mods = spec.mods;
    }
    if (spec.bpm) {
        const [min, max, common] = spec.bpm;
        const beatLength = (value: number): number => Math.round((60_000 / value) * 1000) / 1000;
        const normalized = (value: number): number => 60_000 / beatLength(value);
        services.beatmapPP.minBPM = normalized(min);
        services.beatmapPP.maxBPM = normalized(max);
        services.beatmapPP.commonBPM = normalized(common);
        services.beatmapPP.realtimeBPM = normalized(common);
        const timingPoints = min === max
            ? [{ group: { startTime: 0 }, beatLength: beatLength(common) }]
            : [
                { group: { startTime: 0 }, beatLength: beatLength(min) },
                { group: { startTime: 1000 }, beatLength: beatLength(max) },
                { group: { startTime: 2000 }, beatLength: beatLength(common) }
            ];
        services.beatmapPP.timingPoints = timingPoints;
        services.beatmapPP.timings = { firstObj: 10_000, full: 10_000 };
        services.beatmapPP.previewtime = -1;
        const attributes = services.beatmapPP.calculatedMapAttributes;
        if (attributes && typeof attributes === 'object') {
            Object.assign(attributes, { circles: 1, maxCombo: 0 });
        }
    }
    if (spec.leaderboard) {
        services.gameplay.leaderboardScores = [{ userId: 42, name: 'Alice', score: 123456, combo: 321, accuracy: 99.5, maxCombo: 400, mods: spec.mods ?? noMods(), statistics: { ...zeroStats(), great: 300, miss: 1 }, team: 1, position: 2, isPassing: true }];
        services.gameplay.isLeaderboardVisible = true;
    }
    const preciseClients: PreciseClient[] = spec.preciseShort ? [{ ipcId: 0, keys: [{ name: 'K1', isPressed: true, count: 7 }] }] : [];
    if (spec.preciseShort) services.gameplay.keyOverlay = preciseClients[0].keys;
    if (spec.tournament === 'lazer') {
        services.lazerMultiSpectating.lazerSpectatingData = { chat: [{ time: '12:34', name: 'Ref', content: 'ready' }], spectatingClients: [lazerSpectator('Red', 'red', 1000), lazerSpectator('Blue', 'blue', 2000)] };
    }
    return { ...spec, source: spec.source, status, mode, services, preciseClients };
}

function lazerSpectator(name: string, team: string, score: number): Record<string, unknown> {
    const stats = zeroStats();
    return {
        team,
        user: {
            id: score, name, countryCode: 1, accuracy: 99, rankedScore: score,
            playCount: 1, playMode: 0, rank: 2, performancePoints: 3, level: 1,
            rawBanchoStatus: 0, backgroundColour: 0, rawLoginStatus: 0, matchmaking: null
        },
        score: {
            failed: false, retries: 0, playerName: name, mods: noMods(), mode: 0,
            accuracy: 100, statistics: stats, maximumStatistics: stats, score,
            playerHP: 200, playerHPSmooth: 200, combo: 10, maxCombo: 10, pp: 5
        }
    };
}

function instanceFrom(input: InputFixture, client: number, services: Services, ipcId = 0): SourceInstance {
    return { client, isTourneyManager: false, isTourneySpectator: false, ipcId, getServices(names: string[]) { return Object.fromEntries(names.map((name) => [name, services[name]])); } };
}

function stableTournamentInstances(base: SourceInstance, services: Services): Record<string, SourceInstance> {
    const managerServices = canonicalServices();
    managerServices.tourneyManager = { ...services.tourneyManager, ipcState: 2, leftStars: 1, rightStars: 2, bestOf: 5, starsVisible: true, scoreVisible: true, firstTeamName: 'Red', secondTeamName: 'Blue', firstTeamScore: 111, secondTeamScore: 222, messages: [{ time: '12:00', name: 'BanchoBot', content: 'match' }] };
    const manager = instanceFrom({ id: '', tags: [], services: managerServices, preciseClients: [] }, 0, managerServices, 0);
    manager.isTourneyManager = true;
    const redServices = canonicalServices();
    redServices.tourneyManager = { ...redServices.tourneyManager, userName: 'Red', userCountry: 'US', userID: 1 };
    const blueServices = canonicalServices();
    blueServices.tourneyManager = { ...blueServices.tourneyManager, userName: 'Blue', userCountry: 'JP', userID: 2 };
    const red = instanceFrom({ id: '', tags: [], services: redServices, preciseClients: [] }, 0, redServices, 2);
    red.isTourneySpectator = true;
    const blue = instanceFrom({ id: '', tags: [], services: blueServices, preciseClients: [] }, 0, blueServices, 1);
    blue.isTourneySpectator = true;
    return { '10': base, '20': manager, '40': red, '30': blue };
}

async function main(): Promise<void> {
    const options = parseOptions(process.argv.slice(2));
    const scriptDir = dirname(fileURLToPath(import.meta.url));
    const lock = parseJson<Lock>(join(scriptDir, 'oracle-lock.json'));
    verifyPins(options, lock);

    registerHooks({
        resolve(specifier, context, nextResolve) {
            const resolved = nextResolve(specifier, context);
            if (resolved.url.endsWith('/packages/tosu/src/instances/lazerInstance.ts')) {
                return { url: 'oracle:lazer-instance', shortCircuit: true };
            }
            if (resolved.url.endsWith('/packages/tosu/src/instances/manager.ts')) {
                return { url: 'oracle:instance-manager', shortCircuit: true };
            }
            if (resolved.url.endsWith('/packages/tosu/src/states/beatmap.ts')) {
                return { url: 'oracle:beatmap-state', shortCircuit: true };
            }
            if (resolved.url.endsWith('/packages/tosu/src/states/gameplay.ts')) {
                return { url: 'oracle:gameplay-state', shortCircuit: true };
            }
            return resolved;
        },
        load(url, context, nextLoad) {
            if (url === 'oracle:lazer-instance') {
                return {
                    format: 'module',
                    shortCircuit: true,
                    source: 'export class LazerInstance {}'
                };
            }
            if (url === 'oracle:instance-manager') {
                return { format: 'module', shortCircuit: true, source: 'export class InstanceManager {}' };
            }
            if (url === 'oracle:beatmap-state') {
                return { format: 'module', shortCircuit: true, source: 'export class BeatmapPP {}' };
            }
            if (url === 'oracle:gameplay-state') {
                return { format: 'module', shortCircuit: true, source: 'export class Gameplay {}' };
            }
            return nextLoad(url, context);
        }
    });

    const [{ buildResult: buildV1 }, { buildResult: buildV2 }, { buildResult: buildPrecise }, { buildResult: buildSc }, { LazerInstance }] = await Promise.all([
        import(pathToFileURL(join(options.source, 'packages/tosu/src/api/utils/buildResult.ts')).href),
        import(pathToFileURL(join(options.source, 'packages/tosu/src/api/utils/buildResultV2.ts')).href),
        import(pathToFileURL(join(options.source, 'packages/tosu/src/api/utils/buildResultV2Precise.ts')).href),
        import(pathToFileURL(join(options.source, 'packages/tosu/src/api/utils/buildResultSC.ts')).href),
        import(pathToFileURL(join(options.source, 'packages/tosu/src/instances/lazerInstance.ts')).href)
    ]);

    rmSync(options.out, { recursive: true, force: true });
    mkdirSync(options.out, { recursive: true });
    const cases: InventoryEntry[] = [];
    for (const spec of scenarios) {
        const input = materialize(spec);
        const caseDir = join(options.out, spec.id);
        mkdirSync(caseDir, { recursive: true });
        writeFileSync(join(caseDir, 'input.json'), `${JSON.stringify(input, null, 2)}\n`);

        let manager: SourceManager;
        if (!spec.source) {
            manager = { focusedClient: undefined, gameFocused: false, osuInstances: {}, getInstance() { return undefined; } };
        } else {
            const client = spec.source === 'lazer' ? 1 : 0;
            const focused = instanceFrom(input, client, input.services);
            if (client === 1) Object.setPrototypeOf(focused, LazerInstance.prototype);
            const osuInstances = spec.tournament === 'stable' ? stableTournamentInstances(focused, input.services) : { '10': focused };
            manager = { focusedClient: client, gameFocused: true, osuInstances, getInstance(requested) { return requested === client ? focused : undefined; } };
        }
        // The dynamic pinned-source imports accept the structural adapter, whose
        // members above are exactly the manager surface read by all four builders.
        const outputs = { v1: buildV1(manager), v2: buildV2(manager), 'v2-precise': buildPrecise(manager), sc: buildSc(manager) };
        const expected: Record<string, string> = {};
        for (const [schema, value] of Object.entries(outputs)) {
            const relative = `${spec.id}/${schema}.json`;
            writeFileSync(join(options.out, relative), JSON.stringify(value));
            expected[schema] = relative;
        }
        cases.push({ id: spec.id, tags: spec.tags, input: `${spec.id}/input.json`, expected });
    }

    const requiredCoverage = ['no-client', 'defaults', 'menu', 'play', 'result', 'lobby', 'stable', 'lazer', 'osu', 'taiko', 'catch', 'mania', 'HD', 'FL', 'DT', 'HT', 'CL', 'DA', 'structured-speed', 'leaderboard-empty', 'leaderboard-populated', 'stable-tournament', 'lazer-tournament', 'precise-short-keys', 'multi-IPC', 'SC-equal-BPM', 'SC-range-BPM', 'known-omissions', 'placeholders'];
    const covered = new Set(cases.flatMap((entry) => entry.tags));
    const missing = requiredCoverage.filter((tag) => !covered.has(tag));
    if (missing.length > 0) throw new Error(`fixture coverage incomplete: ${missing.join(', ')}`);
    const inventory: Inventory = { format: 1, revisions: lock, requiredCoverage, cases };
    writeFileSync(join(options.out, 'inventory.json'), `${JSON.stringify(inventory, null, 2)}\n`);
    process.stdout.write(`generated ${cases.length} oracle cases at ${options.out}\n`);
}

void main().catch((error: unknown) => {
    const message = error instanceof Error ? error.stack ?? error.message : String(error);
    process.stderr.write(`${message}\n`);
    process.exitCode = 1;
});

/**
 * @fileoverview 双浏览器引擎契约（纯函数为主）
 * @description 解析 engine、Clearcote 平台/seed/启动选项与安全 userData 路径。
 *              纯逻辑可单测；seed 读写为受控磁盘 IO。
 */

import crypto from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';

export const BROWSER_ENGINES = Object.freeze(['camoufox', 'clearcote']);
export const DEFAULT_BROWSER_ENGINE = 'camoufox';
export const CLEARCOTE_SEED_FILE = '.webai2api-clearcote.json';
export const CAMOUFOX_USERDATA_PREFIX = 'camoufoxUserData';
export const CLEARCOTE_USERDATA_PREFIX = 'clearcoteUserData';
/** 单层安全标识符：ASCII 字母/数字/下划线/连字符 */
export const USERDATA_MARK_RE = /^[A-Za-z0-9_-]+$/;

const SEED_LOCK_PREFIX = '.webai2api-clearcote.lock';
const SEED_MIN_LENGTH = 16;

/**
 * 规范化 engine 值；非法值抛错
 * @param {unknown} value
 * @param {string} [fallback]
 * @returns {'camoufox'|'clearcote'}
 */
export function normalizeEngine(value, fallback = DEFAULT_BROWSER_ENGINE) {
    if (value === undefined || value === null || value === '') {
        return fallback || DEFAULT_BROWSER_ENGINE;
    }
    if (typeof value !== 'string') {
        throw new Error(`browser engine 必须是字符串，收到: ${typeof value}`);
    }
    const engine = value.trim().toLowerCase();
    if (!BROWSER_ENGINES.includes(engine)) {
        throw new Error(`未知 browser engine: ${value}（允许: camoufox | clearcote）`);
    }
    return engine;
}

/**
 * 从全局配置 + instance 配置解析 engine
 * @param {object} globalConfig
 * @param {object|null} instance
 * @returns {'camoufox'|'clearcote'}
 */
export function resolveInstanceEngine(globalConfig, instance) {
    const globalEngine = normalizeEngine(globalConfig?.browser?.engine, DEFAULT_BROWSER_ENGINE);
    if (instance?.engine === undefined || instance?.engine === null || instance?.engine === '') {
        return globalEngine;
    }
    return normalizeEngine(instance.engine, globalEngine);
}

/**
 * 校验 userDataMark：空/undefined 表示默认目录；非空必须是单层安全标识符
 * @param {unknown} mark
 * @returns {string} 规范化后的 mark（默认 ''）
 */
export function validateUserDataMark(mark) {
    if (mark === undefined || mark === null || mark === '') {
        return '';
    }
    if (typeof mark !== 'string') {
        throw new Error(`userDataMark 必须是字符串，收到: ${typeof mark}`);
    }
    const trimmed = mark.trim();
    // trim 后仍与原串不同 → 含首尾空白/控制字符风险，直接拒绝（不做静默修正）
    if (trimmed !== mark) {
        throw new Error(`userDataMark 不允许首尾空白或控制字符`);
    }
    if (!USERDATA_MARK_RE.test(mark)) {
        throw new Error(
            'userDataMark 只能包含 ASCII 字母、数字、下划线和连字符（单层标识符，不允许路径分隔符、.. 或绝对路径）'
        );
    }
    return mark;
}

/**
 * 将 baseDir 解析为 canonical 目录
 * @param {string} [baseDir]
 * @returns {string}
 */
export function resolveDataBaseDir(baseDir = path.join(process.cwd(), 'data')) {
    return path.resolve(baseDir);
}

/**
 * 断言 target 位于 baseDir 内（含 baseDir 本身的一层子路径，不允许越界）
 * @param {string} target
 * @param {string} baseDir
 */
export function assertPathInsideDataDir(target, baseDir) {
    const base = resolveDataBaseDir(baseDir);
    const resolved = path.resolve(target);
    const rel = path.relative(base, resolved);
    if (rel === '') {
        throw new Error('拒绝解析到 data/ 根目录本身');
    }
    if (rel.startsWith('..') || path.isAbsolute(rel)) {
        throw new Error(`路径越界，必须位于 data/ 之下: ${path.basename(resolved)}`);
    }
    // 禁止多级子路径逃逸命名：只允许 data/<folderName> 一层
    if (rel.includes(path.sep)) {
        throw new Error(`路径越界，仅允许 data/ 下单层目录: ${path.basename(resolved)}`);
    }
    return resolved;
}

/**
 * 解析用户数据目录（引擎隔离 + mark 校验 + 边界检查）
 * @param {string|undefined|null} userDataMark
 * @param {string} [engine]
 * @param {string} [baseDir]
 * @returns {string}
 */
export function resolveUserDataDirForEngine(userDataMark, engine = DEFAULT_BROWSER_ENGINE, baseDir = path.join(process.cwd(), 'data')) {
    const eng = normalizeEngine(engine);
    const prefix = eng === 'clearcote' ? CLEARCOTE_USERDATA_PREFIX : CAMOUFOX_USERDATA_PREFIX;
    const mark = validateUserDataMark(userDataMark);
    const name = mark ? `${prefix}_${mark}` : prefix;
    return assertPathInsideDataDir(path.join(resolveDataBaseDir(baseDir), name), baseDir);
}

/**
 * Pool 浏览器共享键：必须包含 engine + userDataDir
 * @param {string} userDataDir
 * @param {string} [engine]
 * @returns {string}
 */
export function browserShareKey(userDataDir, engine = DEFAULT_BROWSER_ENGINE) {
    return `${normalizeEngine(engine)}::${userDataDir}`;
}

/**
 * 收集配置引用的引擎集合
 * @param {object} config
 * @returns {Set<'camoufox'|'clearcote'>}
 */
export function collectReferencedEngines(config) {
    const engines = new Set();
    const instances = config?.backend?.pool?.instances || [];
    if (instances.length === 0) {
        engines.add(normalizeEngine(config?.browser?.engine, DEFAULT_BROWSER_ENGINE));
        return engines;
    }
    for (const inst of instances) {
        engines.add(resolveInstanceEngine(config, inst));
    }
    return engines;
}

/**
 * Clearcote 宿主平台/架构支持检查（官方二进制）
 * @param {string} [hostPlatform]
 * @param {string} [hostArch]
 */
export function assertClearcoteHostSupported(hostPlatform = os.platform(), hostArch = os.arch()) {
    const archOk = hostArch === 'x64' || hostArch === 'x86_64';
    if (hostPlatform === 'win32') {
        if (!archOk) {
            throw new Error(`Clearcote 官方支持 Windows x64，当前架构: ${hostArch}`);
        }
        return;
    }
    if (hostPlatform === 'linux') {
        if (!archOk) {
            throw new Error(`Clearcote 官方支持 Linux x64，当前架构: ${hostArch}`);
        }
        return;
    }
    if (hostPlatform === 'darwin') {
        throw new Error(
            'Clearcote 当前官方 Node SDK/发行版不支持 macOS（仍在 roadmap）。' +
            '请将 browser.engine 保持为 camoufox，或在 Windows/Linux x64 上部署 Clearcote。' +
            '本项目不会将 Clearcote 静默回退为普通 Chromium。'
        );
    }
    throw new Error(
        `Clearcote 不支持当前宿主平台: ${hostPlatform}。官方支持 Windows x64 / Linux x64。`
    );
}

/**
 * 解析 Clearcote 指纹 platform（auto | windows | linux）
 * @param {string} [platformSetting]
 * @param {string} [hostPlatform]
 * @returns {'windows'|'linux'}
 */
export function resolveClearcoteFingerprintPlatform(platformSetting = 'auto', hostPlatform = os.platform(), hostArch = os.arch()) {
    assertClearcoteHostSupported(hostPlatform, hostArch);
    const setting = (platformSetting || 'auto').toLowerCase();
    if (setting === 'auto') {
        return hostPlatform === 'win32' ? 'windows' : 'linux';
    }
    if (setting === 'windows' || setting === 'linux') {
        return setting;
    }
    throw new Error(`browser.clearcote.platform 非法: ${platformSetting}（允许: auto | windows | linux）`);
}

/**
 * 读取 seed 文件（不生成）
 * @param {string} userDataDir
 * @returns {{ok: true, seed: string} | {ok: false, reason: string}}
 */
export function readClearcoteSeedFile(userDataDir) {
    const metaPath = path.join(userDataDir, CLEARCOTE_SEED_FILE);
    if (!fs.existsSync(metaPath)) {
        return { ok: false, reason: 'missing' };
    }
    let raw;
    try {
        raw = fs.readFileSync(metaPath, 'utf8');
    } catch (e) {
        return { ok: false, reason: `unreadable: ${e.message}` };
    }
    let meta;
    try {
        meta = JSON.parse(raw);
    } catch {
        return { ok: false, reason: 'corrupt-json' };
    }
    if (!meta || typeof meta !== 'object' || Array.isArray(meta)) {
        return { ok: false, reason: 'invalid-type' };
    }
    if (typeof meta.seed !== 'string' || meta.seed.length < SEED_MIN_LENGTH) {
        return { ok: false, reason: 'invalid-seed-field' };
    }
    return { ok: true, seed: meta.seed };
}

/**
 * 可选：将损坏 seed 复制为时间戳 quarantine 证据（不删除原文件）
 * @param {string} metaPath
 * @returns {string|null}
 */
export function quarantineCorruptSeedCopy(metaPath) {
    const ts = new Date().toISOString().replace(/[:.]/g, '-');
    const target = `${metaPath}.corrupt-${ts}`;
    try {
        fs.copyFileSync(metaPath, target);
        return target;
    } catch {
        return null;
    }
}

/**
 * 原子写入 seed 文件（临时文件 + rename，mode 0o600）
 * @param {string} metaPath
 * @param {object} meta
 */
function writeSeedFileAtomic(metaPath, meta) {
    const tmp = `${metaPath}.${process.pid}.${crypto.randomBytes(4).toString('hex')}.tmp`;
    const payload = JSON.stringify(meta, null, 2);
    fs.writeFileSync(tmp, payload, { encoding: 'utf8', mode: 0o600 });
    try {
        fs.renameSync(tmp, metaPath);
    } catch (e) {
        try { fs.unlinkSync(tmp); } catch { /* ignore */ }
        throw e;
    }
}

/**
 * 进程内 profile 级 single-flight + 磁盘 lock，避免并发首建产生两个身份
 * @type {Map<string, string>}
 */
const seedInflight = new Map();

/**
 * 读取/生成 Clearcote profile 持久 seed
 * - 损坏/不完整：保留原文件并抛出可操作错误（可选 quarantine 副本），绝不静默覆盖
 * - 新 seed 原子写入 + 进程内/目录锁，避免并发首建双身份
 * @param {string} userDataDir
 * @returns {string}
 */
export function ensureClearcoteSeed(userDataDir) {
    if (!userDataDir) {
        throw new Error('ensureClearcoteSeed 需要 userDataDir');
    }
    const key = path.resolve(userDataDir);
    const existing = seedInflight.get(key);
    if (existing) return existing;

    const result = ensureClearcoteSeedUnlocked(userDataDir);
    seedInflight.set(key, result);
    queueMicrotask(() => seedInflight.delete(key));
    return result;
}

/**
 * @param {string} userDataDir
 * @returns {string}
 */
function ensureClearcoteSeedUnlocked(userDataDir) {
    fs.mkdirSync(userDataDir, { recursive: true });
    const metaPath = path.join(userDataDir, CLEARCOTE_SEED_FILE);
    const lockPath = path.join(userDataDir, SEED_LOCK_PREFIX);

    const found0 = readClearcoteSeedFile(userDataDir);
    if (found0.ok) return found0.seed;
    if (found0.reason !== 'missing') {
        throw new Error(
            `Clearcote seed 文件已损坏或不完整（${found0.reason}），已保留原文件。` +
            `请人工检查 ${CLEARCOTE_SEED_FILE}；确认后可将其改名为 quarantine 副本再重启生成新身份，禁止静默覆盖。`
        );
    }

    // 目录锁：首次并发创建串行化；超时拒绝无锁写入，避免双身份
    const deadline = Date.now() + 5000;
    let locked = false;
    while (Date.now() < deadline) {
        try {
            fs.mkdirSync(lockPath);
            locked = true;
            break;
        } catch {
            const found = readClearcoteSeedFile(userDataDir);
            if (found.ok) return found.seed;
            const shared = new Int32Array(new SharedArrayBuffer(4));
            Atomics.wait(shared, 0, 0, 20);
        }
    }

    if (!locked) {
        const found = readClearcoteSeedFile(userDataDir);
        if (found.ok) return found.seed;
        throw new Error(
            'Clearcote seed 创建锁超时，拒绝在无锁状态下写入（避免并发双身份）。' +
            `请清理 ${lockPath} 后重试。`
        );
    }

    try {
        const found = readClearcoteSeedFile(userDataDir);
        if (found.ok) return found.seed;
        if (found.reason !== 'missing') {
            throw new Error(
                `Clearcote seed 文件已损坏或不完整（${found.reason}），已保留原文件。` +
                `请人工检查 ${CLEARCOTE_SEED_FILE}。`
            );
        }

        const seed = `w2a-${crypto.randomBytes(24).toString('hex')}`;
        const meta = {
            version: 1,
            seed,
            createdAt: new Date().toISOString(),
            note: 'WebAI2API Clearcote persistent identity seed — do not reuse across engines'
        };
        writeSeedFileAtomic(metaPath, meta);
        return seed;
    } finally {
        try { fs.rmdirSync(lockPath); } catch { /* ignore */ }
    }
}

/**
 * 构建 Clearcote launchPersistentContext 选项
 * - fingerprintProfile 与自动 seed 互斥（计划 P1-B）
 * - sandbox 默认安全；显式关闭才注入 --no-sandbox
 * @param {object} params
 * @returns {object}
 */
export function buildClearcoteLaunchOptions(params) {
    const {
        browserConfig = {},
        userDataDir,
        headless = false,
        proxy = null,
        hostPlatform = os.platform(),
        hostArch = os.arch(),
        explicitSeed = null
    } = params;

    if (!userDataDir) {
        throw new Error('buildClearcoteLaunchOptions 需要 userDataDir');
    }

    const clearcoteCfg = browserConfig.clearcote || {};
    const fingerprintPlatform = resolveClearcoteFingerprintPlatform(clearcoteCfg.platform, hostPlatform, hostArch);
    const fingerprintProfile = normalizeFingerprintProfile(clearcoteCfg.fingerprintProfile);

    const options = {
        headless: !!headless,
        platform: fingerprintPlatform,
        brand: clearcoteCfg.brand || 'Chrome',
        geoip: clearcoteCfg.geoip !== false,
        humanize: clearcoteCfg.humanize !== false
    };

    let fingerprintSource = 'none';
    if (fingerprintProfile) {
        // 互斥：有 fingerprintProfile 时不得再传自动/持久 seed
        options.fingerprintProfile = fingerprintProfile;
        fingerprintSource = 'fingerprint-profile';
    } else {
        const seed = explicitSeed || ensureClearcoteSeed(userDataDir);
        options.fingerprint = seed;
        fingerprintSource = 'profile-seed';
    }

    if (clearcoteCfg.path) {
        options.executablePath = clearcoteCfg.path;
    }
    if (clearcoteCfg.timezone) {
        options.timezone = clearcoteCfg.timezone;
    }
    if (clearcoteCfg.acceptLanguage) {
        options.acceptLanguage = clearcoteCfg.acceptLanguage;
    }
    if (clearcoteCfg.webrtcIp) {
        options.webrtcIp = clearcoteCfg.webrtcIp;
    }

    const userArgs = sanitizeClearcoteArgs(clearcoteCfg.args);
    const sandboxDisabled = clearcoteCfg.sandbox === false;
    const args = [...userArgs];
    if (sandboxDisabled && !args.includes('--no-sandbox')) {
        args.push('--no-sandbox');
    }
    if (args.length > 0) {
        options.args = args;
    }

    if (proxy) {
        options.proxy = proxy;
    }

    options.__meta = {
        fingerprintSource,
        sandboxEnabled: !sandboxDisabled,
        sandboxExplicitlyDisabled: sandboxDisabled
    };

    // 绝不透传 Firefox/Camoufox 专属字段
    return options;
}

/**
 * 规范化 fingerprintProfile 配置（字符串路径 / 空）
 * @param {unknown} value
 * @returns {string}
 */
export function normalizeFingerprintProfile(value) {
    if (value === undefined || value === null) {
        return '';
    }
    if (typeof value !== 'string') {
        throw new Error('browser.clearcote.fingerprintProfile 必须是字符串路径');
    }
    return value.trim();
}

/** 启动参数中安全敏感、禁止用户覆盖的前缀/全名 */
const SENSITIVE_ARG_PREFIXES = [
    '--user-data-dir',
    '--proxy-server',
    '--proxy-bypass-list',
    '--fingerprint',
    '--fingerprint-profile',
    '--remote-debugging-port',
    '--remote-debugging-pipe',
    '--headless'
];

/**
 * 校验并规范化 Clearcote args（string[]）
 * @param {unknown} args
 * @returns {string[]}
 */
export function sanitizeClearcoteArgs(args) {
    if (args === undefined || args === null) return [];
    if (!Array.isArray(args)) {
        throw new Error('clearcote.args 必须是字符串数组');
    }
    const out = [];
    for (const item of args) {
        if (typeof item !== 'string') {
            throw new Error(`clearcote.args 必须全是字符串，发现: ${typeof item}`);
        }
        const trimmed = item.trim();
        if (!trimmed) continue;
        const lower = trimmed.toLowerCase();
        for (const bad of SENSITIVE_ARG_PREFIXES) {
            if (lower === bad || lower.startsWith(`${bad}=`) || lower.startsWith(`${bad} `)) {
                throw new Error(
                    `clearcote.args 不允许覆盖安全敏感参数: ${bad}（请使用专用配置项）`
                );
            }
        }
        // --no-sandbox 只能通过 sandbox: false 表达，避免 UI/配置双路径
        if (lower === '--no-sandbox' || lower.startsWith('--no-sandbox=')) {
            throw new Error('clearcote.args 不允许直接写 --no-sandbox，请设置 browser.clearcote.sandbox: false');
        }
        out.push(trimmed);
    }
    return out;
}

/**
 * 判断是否应启用项目侧 ghost-cursor
 * @param {string} engine
 * @param {object} globalConfig
 * @returns {boolean}
 */
export function shouldUseGhostCursor(engine, globalConfig) {
    const mode = globalConfig?.browser?.humanizeCursor;
    if (mode !== true) return false;
    const eng = normalizeEngine(engine);
    if (eng === 'clearcote') {
        const native = globalConfig?.browser?.clearcote?.humanize !== false;
        // Clearcote 原生 humanize 开启时不叠加 ghost-cursor
        return !native;
    }
    return true;
}

/**
 * 构建引擎 runtime 元数据
 * @param {object} params
 * @returns {object}
 */
export function buildEngineRuntime(params) {
    const {
        engine,
        version = null,
        release = null,
        hostPlatform = os.platform(),
        binarySource = 'unknown',
        capabilities = {}
    } = params;
    return {
        engine: normalizeEngine(engine),
        version: version || null,
        release: release || null,
        platform: hostPlatform,
        binarySource,
        capabilities
    };
}

/**
 * 数据目录名是否为项目管理的精确格式（拒绝 clearcoteUserDataEvil 等伪造前缀）
 * @param {string} name
 * @returns {boolean}
 */
export function isManagedUserDataFolder(name) {
    if (typeof name !== 'string' || !name) return false;
    return parseManagedUserDataFolder(name) !== null;
}

/**
 * 解析托管数据目录名 → { engine, mark }
 * @param {string} name
 * @returns {{engine: 'camoufox'|'clearcote', mark: string}|null}
 */
export function parseManagedUserDataFolder(name) {
    if (typeof name !== 'string' || !name) return null;
    const patterns = [
        { re: /^camoufoxUserData$/, engine: /** @type {const} */ ('camoufox'), mark: '' },
        { re: /^camoufoxUserData_([A-Za-z0-9_-]+)$/, engine: /** @type {const} */ ('camoufox'), mark: '$1' },
        { re: /^clearcoteUserData$/, engine: /** @type {const} */ ('clearcote'), mark: '' },
        { re: /^clearcoteUserData_([A-Za-z0-9_-]+)$/, engine: /** @type {const} */ ('clearcote'), mark: '$1' }
    ];
    for (const p of patterns) {
        const m = name.match(p.re);
        if (m) {
            return { engine: p.engine, mark: p.mark === '$1' ? m[1] : '' };
        }
    }
    return null;
}

/**
 * 将托管文件夹名解析为 data/ 内安全路径；拒绝穿越/越界/跨层
 * @param {string} name
 * @param {string} [baseDir]
 * @returns {string}
 */
export function resolveManagedUserDataPath(name, baseDir = path.join(process.cwd(), 'data')) {
    if (!isManagedUserDataFolder(name)) {
        throw new Error(`非法用户数据目录名: ${JSON.stringify(name)}`);
    }
    return assertPathInsideDataDir(path.join(resolveDataBaseDir(baseDir), name), baseDir);
}

/**
 * 脱敏 runtime/状态中的路径与代理信息（递归处理嵌套 runtime）
 * @param {object} runtime
 * @returns {object}
 */
export function sanitizeRuntimeForApi(runtime) {
    if (!runtime || typeof runtime !== 'object' || Array.isArray(runtime)) {
        return runtime;
    }
    const out = { ...runtime };
    if (out.binaryPath) {
        out.binaryPath = path.basename(String(out.binaryPath));
    }
    if (out.userDataDir) {
        out.userDataDir = path.basename(String(out.userDataDir));
    }
    if (out.runtime && typeof out.runtime === 'object') {
        out.runtime = sanitizeRuntimeForApi(out.runtime);
    }
    if (out.workers && Array.isArray(out.workers)) {
        out.workers = out.workers.map((w) => sanitizeRuntimeForApi(w));
    }
    delete out.proxyPassword;
    delete out.proxyUsername;
    delete out.__meta;
    return out;
}

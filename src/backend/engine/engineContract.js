/**
 * @fileoverview 双浏览器引擎契约（纯函数）
 * @description 解析 engine、Clearcote 平台/seed/启动选项，不触碰 IO 的部分可单测。
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
 * 解析用户数据目录（引擎隔离）
 * @param {string|undefined|null} userDataMark
 * @param {string} [engine]
 * @param {string} [baseDir]
 * @returns {string}
 */
export function resolveUserDataDirForEngine(userDataMark, engine = DEFAULT_BROWSER_ENGINE, baseDir = path.join(process.cwd(), 'data')) {
    const eng = normalizeEngine(engine);
    const prefix = eng === 'clearcote' ? CLEARCOTE_USERDATA_PREFIX : CAMOUFOX_USERDATA_PREFIX;
    if (!userDataMark) {
        return path.join(baseDir, prefix);
    }
    return path.join(baseDir, `${prefix}_${userDataMark}`);
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
 * Clearcote 宿主平台支持检查（官方二进制）
 * @param {string} [hostPlatform]
 */
export function assertClearcoteHostSupported(hostPlatform = os.platform()) {
    if (hostPlatform === 'win32' || hostPlatform === 'linux') {
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
export function resolveClearcoteFingerprintPlatform(platformSetting = 'auto', hostPlatform = os.platform()) {
    assertClearcoteHostSupported(hostPlatform);
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
 * 读取/生成 Clearcote profile 持久 seed
 * @param {string} userDataDir
 * @returns {string}
 */
export function ensureClearcoteSeed(userDataDir) {
    if (!userDataDir) {
        throw new Error('ensureClearcoteSeed 需要 userDataDir');
    }
    fs.mkdirSync(userDataDir, { recursive: true });
    const metaPath = path.join(userDataDir, CLEARCOTE_SEED_FILE);
    if (fs.existsSync(metaPath)) {
        try {
            const meta = JSON.parse(fs.readFileSync(metaPath, 'utf8'));
            if (meta && typeof meta.seed === 'string' && meta.seed.length >= 16) {
                return meta.seed;
            }
        } catch { /* regenerate below */ }
    }
    const seed = `w2a-${crypto.randomBytes(24).toString('hex')}`;
    const meta = {
        version: 1,
        seed,
        createdAt: new Date().toISOString(),
        note: 'WebAI2API Clearcote persistent identity seed — do not reuse across engines'
    };
    fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2), 'utf8');
    return seed;
}

/**
 * 构建 Clearcote launchPersistentContext 选项
 * 注意：会调用 ensureClearcoteSeed（磁盘 IO）以保证 profile 身份持久化
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
        explicitSeed = null
    } = params;

    if (!userDataDir) {
        throw new Error('buildClearcoteLaunchOptions 需要 userDataDir');
    }

    const clearcoteCfg = browserConfig.clearcote || {};
    const fingerprintPlatform = resolveClearcoteFingerprintPlatform(clearcoteCfg.platform, hostPlatform);
    const seed = explicitSeed || ensureClearcoteSeed(userDataDir);

    const options = {
        headless: !!headless,
        fingerprint: seed,
        platform: fingerprintPlatform,
        brand: clearcoteCfg.brand || 'Chrome',
        geoip: clearcoteCfg.geoip !== false,
        humanize: clearcoteCfg.humanize !== false
    };

    if (clearcoteCfg.path) {
        options.executablePath = clearcoteCfg.path;
    }
    if (clearcoteCfg.fingerprintProfile) {
        options.fingerprintProfile = clearcoteCfg.fingerprintProfile;
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
    if (Array.isArray(clearcoteCfg.args) && clearcoteCfg.args.length > 0) {
        options.args = [...clearcoteCfg.args];
    }
    if (proxy) {
        options.proxy = proxy;
    }

    // 绝不透传 Firefox/Camoufox 专属字段
    return options;
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
 * 数据目录前缀是否允许管理
 * @param {string} name
 * @returns {boolean}
 */
export function isManagedUserDataFolder(name) {
    return typeof name === 'string'
        && (name.startsWith(CAMOUFOX_USERDATA_PREFIX) || name.startsWith(CLEARCOTE_USERDATA_PREFIX));
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
    delete out.proxyPassword;
    delete out.proxyUsername;
    return out;
}

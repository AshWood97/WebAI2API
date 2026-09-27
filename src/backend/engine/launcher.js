/**
 * @fileoverview 浏览器启动与生命周期管理
 * @description 负责启动 Camoufox（Playwright 内核）、注入指纹与代理，并在进程退出时做资源清理。
 *              导航和预热行为由工作池负责，本模块只负责启动浏览器。
 *
 * 约定：
 * - 登录模式会尽量保留 Profile（用户数据目录）
 * - 清理采用三级退出：Playwright close -> SIGTERM -> SIGKILL
 */

// 必须先设置 CAMOUFOX_INSTALL_DIR，再加载 camoufox-js
import './camoufoxEnv.js';
import { Camoufox } from 'camoufox-js';
import { sampleWebGL } from 'camoufox-js/dist/webgl/sample.js';
import { FingerprintGenerator } from 'fingerprint-generator';
import fs from 'fs';
import path from 'path';
import os from 'os';
import { createCursor } from 'ghost-cursor-playwright-port';
import { getRealViewport, clamp, random, sleep } from './utils.js';
import { logger } from '../../utils/logger.js';
import { getBrowserProxy, acquireClearcoteProxy, releaseProxyHandle, cleanupProxy } from '../../utils/proxy.js';
import { PROJECT_CAMOUFOX_DIR } from './camoufoxEnv.js';
import {
    readCamoufoxVersion,
    rewriteFingerprintUserAgent,
    buildCamoufoxCapabilityOptions,
    camoufoxConfigKeySupported
} from './camoufoxMeta.js';
import {
    normalizeEngine,
    buildClearcoteLaunchOptions,
    buildEngineRuntime,
    shouldUseGhostCursor
} from './engineContract.js';
import {
    importClearcoteSdk,
    buildClearcoteRuntimeMeta,
    readClearcoteSdkVersion,
    resolveLicenseBoundary,
    readClearcoteReleaseInfo
} from './clearcoteMeta.js';

// 全局状态：跟踪全部活动 context（两种引擎）
const activeContexts = new Set();
let globalBrowserProcess = null;
let globalContext = null; // 最近一次启动的 context（兼容旧逻辑）

/** 显式生命周期：stopping 后禁止 close handler 自动恢复 */
const lifecycle = {
    stopping: false,
    stopped: false,
    // 用户手动关闭了浏览器窗口（仅可能发生在有头模式）：不再自动重建
    browserUserStopped: false,
    browserUserStoppedAt: null
};

/**
 * 是否处于关闭/已关闭状态（Worker 据此禁止 _reinit）
 * @returns {boolean}
 */
export function isShuttingDown() {
    return lifecycle.stopping || lifecycle.stopped;
}

/**
 * 用户是否手动关闭了浏览器（有头模式关窗 / Cmd+Q / kill camoufox）
 * @returns {boolean}
 */
export function isBrowserUserStopped() {
    return lifecycle.browserUserStopped;
}

/**
 * 标记浏览器为用户手动关闭：此后 close 事件不再触发自动重建
 * 无头启动的上下文不经过这里——无头下没有用户能关窗口，close 一律视为崩溃
 * @returns {boolean} 是否发生了状态变更
 */
export function markBrowserUserStopped() {
    if (lifecycle.browserUserStopped) return false;
    lifecycle.browserUserStopped = true;
    lifecycle.browserUserStoppedAt = new Date().toISOString();
    return true;
}

/**
 * 清除手动关闭标记（管理员主动恢复浏览器时调用）
 * @returns {boolean} 是否发生了状态变更
 */
export function resetBrowserStopped() {
    const was = lifecycle.browserUserStopped;
    lifecycle.browserUserStopped = false;
    lifecycle.browserUserStoppedAt = null;
    return was;
}

/**
 * 本次上下文关闭是否应自动重建浏览器
 * 规则：服务关闭中 / 用户已手动关闭 → 不重建；其余（无头崩溃等）→ 重建
 * @param {object} [context] - 触发 close 的 context（用于判断是否无头启动）
 * @returns {boolean}
 */
export function shouldAutoRestartOnClose(context) {
    if (isShuttingDown()) return false;
    if (lifecycle.browserUserStopped) return false;
    // 无头启动的上下文不存在"用户手动关闭"，一律按崩溃自愈处理
    return context?.__webaiHeadless === true;
}

/**
 * 清理浏览器资源和进程
 * 实现三级退出机制: Playwright close -> SIGTERM -> SIGKILL
 * 幂等；开始后禁止 close handler 重建 context
 * @returns {Promise<void>}
 */
export async function cleanup() {
    lifecycle.stopping = true;
    if (lifecycle.cleanupPromise) {
        return lifecycle.cleanupPromise;
    }
    if (lifecycle.stopped && activeContexts.size === 0 && !globalBrowserProcess) {
        return;
    }
    lifecycle.cleanupPromise = doCleanup().finally(() => {
        lifecycle.cleanupPromise = null;
        lifecycle.stopped = true;
        lifecycle.stopping = true;
    });
    return lifecycle.cleanupPromise;
}

/**
 * @returns {Promise<void>}
 */
async function doCleanup() {
    lifecycle.stopping = true;

    // Level 1: 通过 Playwright 协议优雅关闭全部 Context（Camoufox + Clearcote）
    const contexts = [...activeContexts];
    activeContexts.clear();
    for (const context of contexts) {
        try {
            logger.debug('浏览器', '正在关闭浏览器上下文并保存 Profile...');
            await context.close();
            logger.debug('浏览器', '已关闭浏览器上下文');
        } catch (e) {
            logger.warn('浏览器', `关闭上下文失败: ${e.message}`);
        }
    }
    globalContext = null;

    // Level 2 & 3: 处理残留进程 (主要用于登录模式)
    if (globalBrowserProcess && !globalBrowserProcess.killed) {
        logger.info('浏览器', '正在终止浏览器进程...');
        try {
            // Level 2: 发送 SIGTERM (软杀)
            globalBrowserProcess.kill('SIGTERM');

            // 等待进程退出
            const start = Date.now();
            while (Date.now() - start < 2000) {
                try {
                    process.kill(globalBrowserProcess.pid, 0);
                    await new Promise(r => setTimeout(r, 200));
                } catch (e) {
                    break;
                }
            }
        } catch (e) { }

        // Level 3: 强制查杀 (SIGKILL)
        try {
            process.kill(globalBrowserProcess.pid, 0);
            logger.debug('浏览器', '浏览器进程无响应，执行强制终止 (SIGKILL)...');
            process.kill(-globalBrowserProcess.pid, 'SIGKILL');
        } catch (e) { }

        globalBrowserProcess = null;
        logger.info('浏览器', '浏览器进程已终止');
    }

    // 清理代理（幂等；单步失败不阻塞）
    try {
        await cleanupProxy();
    } catch (e) {
        logger.warn('浏览器', `清理代理失败: ${e.message}`);
    }

    lifecycle.stopped = true;
    lifecycle.stopping = true;
}

// 防止重复注册
let signalHandlersRegistered = false;

/**
 * 注册进程退出信号处理
 * @private
 */
function registerCleanupHandlers() {
    if (signalHandlersRegistered) return;

    process.on('exit', () => {
        if (globalBrowserProcess) globalBrowserProcess.kill();
    });

    process.on('SIGINT', async () => {
        await cleanup();
        process.exit();
    });

    process.on('SIGTERM', async () => {
        await cleanup();
        process.exit();
    });

    signalHandlersRegistered = true;
}

/**
 * 获取当前操作系统名称
 * 将 Node.js 的 platform 转换为 Camoufox/FingerprintGenerator 支持的格式
 */
function getCurrentOS() {
    const platform = os.platform();
    if (platform === 'win32') return 'windows';
    if (platform === 'darwin') return 'macos';
    // 其他情况默认为 linux
    return 'linux';
}

/**
 * 获取 WebGL 平台标识
 * 将操作系统名称转换为 sampleWebGL 支持的格式
 */
function getWebGLPlatform(osName) {
    if (osName === 'windows') return 'win';
    if (osName === 'macos') return 'mac';
    return 'lin';
}

/**
 * 获取或生成持久化指纹 (含 WebGL 配置校验与 UA 主版本迁移)
 * @param {string} filePath - JSON文件保存路径
 * @param {number|null} targetMajor - 已安装 Camoufox 的 Firefox 主版本
 */
async function getPersistentFingerprint(filePath, targetMajor = null) {
    // 确保 data 目录存在
    const dir = path.dirname(filePath);
    if (!fs.existsSync(dir)) {
        fs.mkdirSync(dir, { recursive: true });
    }

    let fingerprintData = null;
    let webglPair = null;
    let shouldSave = false;
    const currentOS = getCurrentOS();
    const targetWebGLOS = getWebGLPlatform(currentOS);

    // 1. 尝试读取现有指纹
    if (fs.existsSync(filePath)) {
        try {
            const fileContent = fs.readFileSync(filePath, 'utf8');
            fingerprintData = JSON.parse(fileContent);
        } catch (e) {
            logger.warn('浏览器', `指纹文件损坏: ${e.message}`);
        }
    }

    // 2. 校验 WebGL 配置的有效性 (从 videoCard 读取)
    if (fingerprintData?.videoCard?.['webGl:vendor'] && fingerprintData?.videoCard?.['webGl:renderer']) {
        const savedVendor = fingerprintData.videoCard['webGl:vendor'];
        const savedRenderer = fingerprintData.videoCard['webGl:renderer'];
        try {
            // 拿着保存的配置，去数据库里"试探"一下是否存在
            await sampleWebGL(targetWebGLOS, savedVendor, savedRenderer);

            // 如果没报错，说明配置有效，保留使用
            webglPair = [savedVendor, savedRenderer];
            logger.debug('浏览器', `加载 WebGL 配置成功: ${savedRenderer}`);
        } catch (e) {
            // 数据库里没找到 -> 配置失效
            logger.warn('浏览器', `保存的 WebGL 配置与当前系统(${targetWebGLOS})不匹配，将重新生成`);
            webglPair = null;
            shouldSave = true;
        }
    }

    // 3. 如果指纹完全不存在，生成新的基础指纹
    if (!fingerprintData) {
        logger.info('浏览器', `正在为系统 [${currentOS}] 生成新指纹...`);
        const generatorOptions = {
            browsers: ['firefox'],
            operatingSystems: [currentOS],
            devices: ['desktop'],
            locales: ['en-US'],
            screen: { minWidth: 1280, maxWidth: 1366, minHeight: 720, maxHeight: 768 }
        };
        const generator = new FingerprintGenerator(generatorOptions);
        fingerprintData = generator.getFingerprint().fingerprint;

        // 清洗插件数据
        if (fingerprintData.pluginsData) {
            fingerprintData.pluginsData.plugins = [];
            fingerprintData.pluginsData.mimeTypes = [];
        }

        shouldSave = true;
    }

    // 3b. UA 主版本对齐已安装 Camoufox（迁移 FF135 等旧指纹）
    if (fingerprintData && Number.isFinite(targetMajor)) {
        const { changed } = rewriteFingerprintUserAgent(fingerprintData, targetMajor);
        if (changed) {
            logger.info('浏览器', `指纹 UA 已迁移至 Firefox/${targetMajor}.0`);
            shouldSave = true;
        }
    }

    // 4. 如果 WebGL 配置为空，重新生成
    if (!webglPair) {
        try {
            logger.info('浏览器', `正在生成新的 WebGL 配置 (${targetWebGLOS})...`);
            const webglData = await sampleWebGL(targetWebGLOS);
            webglPair = [webglData['webGl:vendor'], webglData['webGl:renderer']];

            // 覆盖 videoCard
            fingerprintData.videoCard = {
                'webGl:vendor': webglPair[0],
                'webGl:renderer': webglPair[1]
            };

            shouldSave = true;
        } catch (e) {
            logger.error('浏览器', `致命错误：无法生成 WebGL 配置: ${e.message}`);
        }
    }

    // 5. 如果 Canvas 噪点不存在，生成新的
    if (fingerprintData.canvasOffset === undefined) {
        const offset = Math.floor(Math.random() * 41) - 20;
        fingerprintData.canvasOffset = offset;
        logger.info('浏览器', `已生成 Canvas 噪点偏移: ${offset}`);
        shouldSave = true;
    }

    // 6. 如果有变动，保存回文件
    if (shouldSave) {
        fs.writeFileSync(filePath, JSON.stringify(fingerprintData, null, 2));
        logger.info('浏览器', `指纹已更新并保存至: ${filePath}`);
    }

    return fingerprintData;
}

/**
 * 启动浏览器实例 (仅负责启动，不负责导航和预热)
 *
 * 导航到目标页面、注册导航处理器、预热行为由工作池 (pool.js) 负责。
 *
 * @param {object} config - 全局配置对象
 * @param {object} options - 启动选项
 * @param {string} options.userDataDir - 用户数据目录路径
 * @param {string} [options.engine] - camoufox | clearcote
 * @param {object} [options.proxyConfig] - Worker 级代理配置
 * @returns {Promise<{context, page, engine, runtime}>}
 */
export async function initBrowserBase(config, options = {}) {
    const {
        userDataDir,
        instanceName = null,
        proxyConfig = null
    } = options;

    const markLabel = instanceName || '默认';
    const engine = normalizeEngine(options.engine || config?.browser?.engine);

    const isLoginMode = process.argv.some(arg => arg.startsWith('-login'));
    const isXvfbMode = process.env.XVFB_RUNNING === 'true';
    const headlessMode = config?.browser?.headless && !isLoginMode && !isXvfbMode;

    if (config?.browser?.headless && !headlessMode) {
        const reasons = [];
        if (isLoginMode) reasons.push('登录模式');
        if (isXvfbMode) reasons.push('Xvfb 模式');
        logger.info('浏览器', `[${markLabel}] 无头模式已被禁用 (${reasons.join(' + ')})`);
    }

    logger.info('浏览器', `[${markLabel}] 启动浏览器实例 (engine=${engine})...`);

    if (engine === 'clearcote') {
        return await launchClearcoteBase({
            config,
            userDataDir,
            proxyConfig,
            markLabel,
            headlessMode,
            engine
        });
    }

    return await launchCamoufoxBase({
        config,
        userDataDir,
        proxyConfig,
        markLabel,
        headlessMode,
        engine
    });
}

/**
 * 在 context 上附加启动元数据
 * __webaiHeadless 供 shouldAutoRestartOnClose 区分"用户关窗"与"无头崩溃"
 * @private
 */
function attachContextMeta(context, headlessMode) {
    if (!context) return;
    try {
        context.__webaiHeadless = headlessMode === true;
    } catch { /* frozen object: 降级为未知，按崩溃处理 */ }
}

/**
 * 尽力获取 Playwright context 背后的浏览器进程句柄
 * persistent context 由库内部 spawn，Node 侧拿不到 ChildProcess；
 * 仅 Camoufox 的 launchPersistentContext 返回值可能暴露 _browserProcess
 * @returns {import('child_process').ChildProcess|null}
 * @private
 */
function resolveBrowserProcess(context) {
    try {
        return context?._browserProcess || context?.browser?.()?._browserProcess || null;
    } catch {
        return null;
    }
}

/**
 * 注册活动 context 并绑定 close 清理
 * @private
 */
function trackContext(context, markLabel) {
    activeContexts.add(context);
    globalContext = context;
    const browserProcess = resolveBrowserProcess(context);
    if (browserProcess) {
        globalBrowserProcess = browserProcess;
    }
    context.on('close', async () => {
        logger.warn('浏览器', `[${markLabel}] 浏览器已断开连接`);
        activeContexts.delete(context);
        if (globalContext === context) {
            globalContext = null;
        }
        globalBrowserProcess = null;
    });
}

/**
 * @private
 */
async function resolveInitialPage(context) {
    const existingPages = context.pages();
    if (existingPages.length > 0) {
        return existingPages[0];
    }
    return await context.newPage();
}

/**
 * @private
 */
async function maybeInjectCss(context, browserConfig, markLabel) {
    const cssInjectConfig = browserConfig.cssInject || {};
    const cssToInject = [];

    if (cssInjectConfig.animation) {
        cssToInject.push(`
            *, *::before, *::after {
                transition: none !important;
                animation: none !important;
                transition-property: none !important;
                scroll-behavior: auto !important;
            }
            *:not(dummy-selector) {
                transition-duration: 0s !important;
                animation-duration: 0s !important;
                transition-delay: 0s !important;
                animation-delay: 0s !important;
            }
        `);
    }

    if (cssInjectConfig.filter) {
        cssToInject.push(`
            *, *::before, *::after {
                filter: none !important;
                backdrop-filter: none !important;
                box-shadow: none !important;
                text-shadow: none !important;
                mix-blend-mode: normal !important;
            }
        `);
    }

    if (cssInjectConfig.font) {
        cssToInject.push(`
            html, body {
                text-rendering: optimizeSpeed !important;
            }
        `);
    }

    if (cssToInject.length === 0) return;

    const cssString = cssToInject.join('\n');
    await context.addInitScript(`
            (function() {
                const style = document.createElement('style');
                style.textContent = ${JSON.stringify(cssString)};
                if (document.head) {
                    document.head.appendChild(style);
                } else {
                    document.addEventListener('DOMContentLoaded', () => {
                        document.head.appendChild(style);
                    });
                }
            })();
        `);
    const enabledFeatures = [];
    if (cssInjectConfig.animation) enabledFeatures.push('动画禁用');
    if (cssInjectConfig.filter) enabledFeatures.push('滤镜禁用');
    if (cssInjectConfig.font) enabledFeatures.push('字体优化');
    logger.info('浏览器', `[${markLabel}] CSS 注入已启用: ${enabledFeatures.join(', ')}`);
}

/**
 * Camoufox 启动链（保持原有行为与 import 顺序）
 * @private
 */
async function launchCamoufoxBase({ config, userDataDir, proxyConfig, markLabel, headlessMode, engine }) {
    const browserConfig = config?.browser || {};

    const camoufoxVer = readCamoufoxVersion(PROJECT_CAMOUFOX_DIR)
        || readCamoufoxVersion(process.env.CAMOUFOX_INSTALL_DIR || PROJECT_CAMOUFOX_DIR);
    if (camoufoxVer) {
        logger.info('浏览器', `[${markLabel}] Camoufox ${camoufoxVer.full} (Firefox ${camoufoxVer.major})`);
    } else {
        logger.warn('浏览器', `[${markLabel}] 未找到 camoufox/version.json，UA 迁移跳过`);
    }

    const fingerprintPath = path.join(userDataDir, 'fingerprint.json');
    const myFingerprint = await getPersistentFingerprint(fingerprintPath, camoufoxVer?.major ?? null);

    const currentOS = getCurrentOS();
    const capability = buildCamoufoxCapabilityOptions(browserConfig, camoufoxVer, myFingerprint);

    const camoufoxConfig = {
        forceScopeAccess: true,
        'canvas:aaOffset': myFingerprint.canvasOffset ?? 0,
        'canvas:aaCapOffset': true
    };

    const firefoxUserPrefs = {
        'layout.css.backdrop-filter.enabled': false,
        'ui.prefersReducedMotion': 1,
        ...(browserConfig.fission === false ? { 'fission.autostart': false } : {})
    };

    if (capability._disableInstantAnimations) {
        const propsPath = path.join(
            PROJECT_CAMOUFOX_DIR,
            'Camoufox.app', 'Contents', 'MacOS', 'properties.json'
        );
        const linuxPropsPath = path.join(PROJECT_CAMOUFOX_DIR, 'properties.json');
        if (camoufoxConfigKeySupported(propsPath, 'disableInstantAnimations')
            || camoufoxConfigKeySupported(linuxPropsPath, 'disableInstantAnimations')) {
            camoufoxConfig.disableInstantAnimations = true;
        }
        firefoxUserPrefs['ui.prefersReducedMotion'] = 1;
    }

    const camoufoxLaunchOptions = {
        executable_path: browserConfig.path || undefined,
        headless: headlessMode,
        user_data_dir: userDataDir,
        fingerprint: myFingerprint,
        os: currentOS,
        i_know_what_im_doing: true,
        webgl_config: myFingerprint.videoCard ? [myFingerprint.videoCard['webGl:vendor'], myFingerprint.videoCard['webGl:renderer']] : undefined,
        exclude_addons: ['UBO'],
        humanize: capability.humanize === undefined
            ? (browserConfig.humanizeCursor === 'camou')
            : capability.humanize,
        config: camoufoxConfig,
        firefox_user_prefs: firefoxUserPrefs,
        ...(capability.ff_version !== undefined ? { ff_version: capability.ff_version } : {}),
        ...(capability.main_world_eval !== undefined ? { main_world_eval: capability.main_world_eval } : {}),
        ...(capability.enable_cache !== undefined ? { enable_cache: capability.enable_cache } : {}),
        ...(capability.window ? { window: capability.window } : {}),
        ...(capability.locale !== undefined ? { locale: capability.locale } : {}),
        ...(capability.certificates !== undefined ? { certificates: capability.certificates } : {}),
        ...(capability.certificatePaths !== undefined ? { certificatePaths: capability.certificatePaths } : {}),
        block_webrtc: capability.block_webrtc !== false,
        geoip: capability.geoip !== false
    };

    const proxyObj = await getBrowserProxy(proxyConfig);
    if (proxyObj) {
        camoufoxLaunchOptions.proxy = proxyObj;
    }

    if (isShuttingDown() || isBrowserUserStopped()) {
        throw new Error('服务正在关闭，取消启动浏览器');
    }
    const context = await Camoufox(camoufoxLaunchOptions);
    attachContextMeta(context, headlessMode);
    trackContext(context, markLabel);

    const statusParts = [];
    statusParts.push(`无头模式: ${headlessMode ? '是' : '否'}`);
    if (camoufoxVer) statusParts.push(`Camoufox: ${camoufoxVer.full}`);
    if (proxyObj) statusParts.push('代理: 已配置');
    if (camoufoxLaunchOptions.humanize) statusParts.push('内核拟人轨迹: 开');
    logger.info('浏览器', `[${markLabel}] 浏览器已启动 (${statusParts.join(', ')})`);

    registerCleanupHandlers();

    const page = await resolveInitialPage(context);

    try {
        const vp = page.viewportSize();
        const screenW = myFingerprint.screen?.availWidth || myFingerprint.screen?.width;
        const screenH = myFingerprint.screen?.availHeight || myFingerprint.screen?.height;
        logger.debug('浏览器', `[${markLabel}] 视口: ${vp?.width || 'null'}x${vp?.height || 'null'}，指纹屏幕: ${screenW}x${screenH}`);
    } catch { /* ignore */ }

    await maybeInjectCss(context, browserConfig, markLabel);

    const runtime = buildEngineRuntime({
        engine,
        version: camoufoxVer?.full || null,
        release: camoufoxVer ? `Firefox ${camoufoxVer.major}` : null,
        hostPlatform: os.platform(),
        binarySource: browserConfig.path ? 'explicit-path' : 'project-camoufox',
        capabilities: {
            nativeHumanize: camoufoxLaunchOptions.humanize === true,
            geoip: camoufoxLaunchOptions.geoip !== false,
            blockWebRtc: camoufoxLaunchOptions.block_webrtc !== false
        }
    });
    runtime.userDataDir = userDataDir;

    return { context, page, engine, runtime };
}

/**
 * Clearcote 启动链（lazy-import SDK；不透传 Firefox 字段）
 * @private
 */
async function launchClearcoteBase({ config, userDataDir, proxyConfig, markLabel, headlessMode, engine }) {
    if (isShuttingDown()) {
        throw new Error('服务正在关闭，取消启动 Clearcote');
    }
    const browserConfig = config?.browser || {};
    const clearcoteCfg = browserConfig.clearcote || {};
    const clearcoteSdk = await importClearcoteSdk();
    if (typeof clearcoteSdk.launchPersistentContext !== 'function') {
        throw new Error('clearcote SDK 未导出 launchPersistentContext，请安装已核验版本 0.30.0');
    }

    // 免费/PRO 边界：检测到 license 且未显式允许则拒绝
    const licenseBoundary = resolveLicenseBoundary(clearcoteCfg);

    const proxyHandle = await acquireClearcoteProxy(proxyConfig);
    let context = null;
    try {
        const launchOptions = buildClearcoteLaunchOptions({
            browserConfig,
            userDataDir,
            headless: headlessMode,
            proxy: proxyHandle?.proxy || null,
            hostPlatform: os.platform(),
            hostArch: os.arch()
        });
        // 从最终 options 剥离内部 meta（SDK 不识别）
        const { __meta: launchMeta, ...sdkOptions } = launchOptions;

        context = await clearcoteSdk.launchPersistentContext(userDataDir, sdkOptions);
        attachContextMeta(context, headlessMode);
        trackContext(context, markLabel);
        if (proxyHandle) {
            bindProxyReleaseToContext(context, proxyHandle);
        }

        let browserVersion = null;
        try {
            const browser = context.browser?.() || null;
            if (browser && typeof browser.version === 'function') {
                browserVersion = browser.version();
            }
        } catch { /* external binary may not expose version */ }

        const statusParts = [];
        statusParts.push(`无头模式: ${headlessMode ? '是' : '否'}`);
        statusParts.push(`Clearcote SDK: ${readClearcoteSdkVersion() || 'unknown'}`);
        if (browserVersion) statusParts.push(`browser: ${browserVersion}`);
        statusParts.push(`platform: ${sdkOptions.platform}`);
        statusParts.push(sdkOptions.executablePath ? 'binary: explicit' : 'binary: sdk-resolve');
        statusParts.push(`sandbox: ${launchMeta?.sandboxEnabled === false ? 'DISABLED' : 'on'}`);
        statusParts.push(`license: ${licenseBoundary.status}`);
        if (proxyHandle?.proxy) statusParts.push('代理: 已配置');
        if (sdkOptions.humanize) statusParts.push('内核拟人轨迹: 开');
        logger.info('浏览器', `[${markLabel}] Clearcote 浏览器已启动 (${statusParts.join(', ')})`);
        if (launchMeta?.sandboxExplicitlyDisabled) {
            logger.warn('浏览器', `[${markLabel}] sandbox 已显式关闭（降低安全性）`);
        }

        registerCleanupHandlers();

        const page = await resolveInitialPage(context);
        await maybeInjectCss(context, browserConfig, markLabel);

        const runtime = buildClearcoteRuntimeMeta({
            hostPlatform: os.platform(),
            executablePath: sdkOptions.executablePath || null,
            fingerprintPlatform: sdkOptions.platform,
            fingerprintSource: launchMeta?.fingerprintSource || 'none',
            browserVersion: browserVersion || null,
            releaseInfo: readClearcoteReleaseInfo(clearcoteSdk),
            licenseStatus: licenseBoundary.status,
            licenseSource: licenseBoundary.source,
            sandboxEnabled: launchMeta?.sandboxEnabled !== false,
            sdkModule: clearcoteSdk
        });
        runtime.userDataDir = userDataDir;
        runtime.capabilities = {
            ...runtime.capabilities,
            nativeHumanize: sdkOptions.humanize === true,
            geoip: sdkOptions.geoip !== false,
            sandboxEnabled: launchMeta?.sandboxEnabled !== false
        };

        return { context, page, engine, runtime };
    } catch (e) {
        // 启动失败立即释放 relay
        if (proxyHandle) {
            try { await releaseProxyHandle(proxyHandle); } catch { /* ignore */ }
        }
        if (context) {
            try { await context.close(); } catch { /* ignore */ }
        }
        throw e;
    }
}

/**
 * 将 proxy relay 生命周期绑定到 context close
 * @param {object} context
 * @param {object} proxyHandle
 */
function bindProxyReleaseToContext(context, proxyHandle) {
    if (!context || typeof context.on !== 'function') return;
    const release = () => {
        releaseProxyHandle(proxyHandle).catch((e) => {
            logger.warn('代理器', `释放代理 relay 失败: ${e.message}`);
        });
    };
    context.on('close', release);
}

// 导出工具函数供 pool.js 使用
export { createCursor, getRealViewport, clamp, random, sleep, shouldUseGhostCursor };

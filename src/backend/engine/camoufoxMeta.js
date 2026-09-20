/**
 * @fileoverview Camoufox 版本与指纹辅助（纯函数，便于测试）
 */

import fs from 'fs';
import path from 'path';

/**
 * 解析已安装 Camoufox 的 version.json
 * @param {string} installDir
 * @returns {{version: string, release: string, major: number, full: string}|null}
 */
export function readCamoufoxVersion(installDir) {
    const file = path.join(installDir, 'version.json');
    if (!fs.existsSync(file)) return null;
    try {
        const data = JSON.parse(fs.readFileSync(file, 'utf8'));
        const version = String(data.version || '');
        const release = String(data.release || '');
        const major = parseInt(version.split('.')[0], 10);
        if (!Number.isFinite(major)) return null;
        return {
            version,
            release,
            major,
            full: release ? `${version}-${release}` : version
        };
    } catch {
        return null;
    }
}

/**
 * 构建 Camoufox GitHub 下载 URL
 * @param {string} version 完整 tag 版本，如 152.0.4-beta.30
 * @param {string} platform process.platform
 * @param {string} arch process.arch
 * @returns {string}
 */
export function buildCamoufoxDownloadUrl(version, platform, arch) {
    const platformMap = {
        win32: 'win',
        darwin: 'mac',
        linux: 'lin'
    };
    const archMap = {
        x64: 'x86_64',
        arm64: 'arm64'
    };
    const platformName = platformMap[platform];
    const archName = archMap[arch];
    if (!platformName || !archName) {
        throw new Error(`Unsupported platform/arch: ${platform}/${arch}`);
    }
    return `https://github.com/daijro/camoufox/releases/download/v${version}/camoufox-${version}-${platformName}.${archName}.zip`;
}

/**
 * 将指纹 UA 中的 Firefox/`rv` 主版本改写为目标版本
 * @param {object} fingerprint
 * @param {number} targetMajor
 * @returns {{fingerprint: object, changed: boolean}}
 */
export function rewriteFingerprintUserAgent(fingerprint, targetMajor) {
    if (!fingerprint || !fingerprint.navigator?.userAgent || !Number.isFinite(targetMajor)) {
        return { fingerprint, changed: false };
    }
    const ua = fingerprint.navigator.userAgent;
    const target = `${targetMajor}.0`;
    const next = ua
        .replace(/rv:[\d.]+/g, `rv:${target}`)
        .replace(/Firefox\/[\d.]+/g, `Firefox/${target}`);
    if (next === ua) {
        return { fingerprint, changed: false };
    }
    fingerprint.navigator.userAgent = next;
    return { fingerprint, changed: true };
}

/**
 * 从 fingerprint.json 规范化 UA 主版本
 * @param {string} filePath
 * @param {number} targetMajor
 * @returns {{ok: boolean, changed: boolean, major: number|null, error?: string}}
 */
export function migrateFingerprintFile(filePath, targetMajor) {
    if (!fs.existsSync(filePath)) {
        return { ok: false, changed: false, major: null, error: 'missing' };
    }
    try {
        const fingerprint = JSON.parse(fs.readFileSync(filePath, 'utf8'));
        const { changed } = rewriteFingerprintUserAgent(fingerprint, targetMajor);
        if (changed) {
            fs.writeFileSync(filePath, JSON.stringify(fingerprint, null, 2));
        }
        return { ok: true, changed, major: targetMajor };
    } catch (e) {
        return { ok: false, changed: false, major: null, error: e.message };
    }
}

/**
 * 组装 Camoufox launch 中与新版能力相关的可选项
 * @param {object} browserConfig config.browser
 * @param {{major?: number}|null} camoufoxVer
 * @param {object} fingerprint
 * @returns {object} 附加到 Camoufox() 的字段
 */
export function buildCamoufoxCapabilityOptions(browserConfig = {}, camoufoxVer = null, fingerprint = {}) {
    const camou = browserConfig.camoufox || {};
    const options = {};

    // ff_version：默认省略，跟随已安装内核；显式配置才 spoof
    if (browserConfig.ffVersion !== undefined && browserConfig.ffVersion !== null && browserConfig.ffVersion !== '') {
        options.ff_version = Number(browserConfig.ffVersion);
        options.i_know_what_im_doing = true;
    }

    // humanize
    const humanizeMode = browserConfig.humanizeCursor;
    if (humanizeMode === 'camou') {
        const maxTime = camou.humanizeMaxTime;
        options.humanize = (typeof maxTime === 'number' && maxTime > 0) ? maxTime : true;
    }

    if (camou.mainWorldEval === true) {
        options.main_world_eval = true;
    }
    if (camou.enableCache === true) {
        options.enable_cache = true;
    }

    // WebRTC：默认阻断；geoip 仍用于地理位置一致性（152 修复代理 IP 泄漏）
    options.block_webrtc = camou.blockWebRtc !== false;
    options.geoip = camou.geoip !== false;

    // 窗口尺寸仅在 camoufox-js 自行生成 fingerprint 时生效；
    // 本项目始终传入持久化 fingerprint，screen/window spoof 由 fromBrowserforge(fingerprint) 提供。
    // 这里仍回传 window 便于日志与未来无 fingerprint 启动场景。
    const screen = fingerprint.screen || {};
    const w = screen.availWidth || screen.width;
    const h = screen.availHeight || screen.height;
    if (Number.isFinite(w) && Number.isFinite(h) && w > 0 && h > 0) {
        options.window = [w, h];
    }

    options._camoufoxVersion = camoufoxVer || null;
    options._disableInstantAnimations = camou.disableInstantAnimations === true;
    return options;
}

/**
 * 读取 FF152 properties.json 中是否支持某 camoufox config 键
 * @param {string} propertiesPath
 * @param {string} key
 * @returns {boolean}
 */
export function camoufoxConfigKeySupported(propertiesPath, key) {
    try {
        if (!fs.existsSync(propertiesPath)) return false;
        const data = JSON.parse(fs.readFileSync(propertiesPath, 'utf8'));
        return data.some((item) => (item?.property ?? item) === key);
    } catch {
        return false;
    }
}

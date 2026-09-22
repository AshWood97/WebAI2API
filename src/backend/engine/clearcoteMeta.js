/**
 * @fileoverview Clearcote 元数据与 SDK 懒加载
 */

import fs from 'fs';
import os from 'os';
import path from 'path';
import { assertClearcoteHostSupported } from './engineContract.js';

/** 项目锁定的 clearcote npm 版本（与 package.json 精确 pin 一致） */
export const CLEARCOTE_SDK_PINNED = '0.30.0';

/**
 * 读取已安装 clearcote 包版本
 * @returns {string|null}
 */
export function readClearcoteSdkVersion() {
    try {
        const pkgPath = path.join(process.cwd(), 'node_modules', 'clearcote', 'package.json');
        if (!fs.existsSync(pkgPath)) return null;
        const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
        return pkg.version || null;
    } catch {
        return null;
    }
}

/**
 * 懒加载 clearcote SDK（仅 engine=clearcote 时调用）
 * @returns {Promise<object>}
 */
export async function importClearcoteSdk() {
    try {
        return await import('clearcote');
    } catch (e) {
        throw new Error(
            `无法导入 clearcote SDK: ${e.message}。请确认已执行 pnpm install，且 clearcote@${CLEARCOTE_SDK_PINNED} 已安装。`
        );
    }
}

/**
 * 检测 license 来源（不读取、不返回 key 内容）
 * @returns {{source: 'option'|'env'|'file'|'none'|'unknown', detected: boolean}}
 */
export function detectClearcoteLicenseSource() {
    // 环境变量只判断存在与非空，不打印值
    const env = process.env.CLEARCOTE_LICENSE_KEY;
    if (typeof env === 'string' && env.trim()) {
        return { source: 'env', detected: true };
    }
    try {
        const p = path.join(os.homedir(), '.clearcote', 'license.key');
        if (fs.existsSync(p)) {
            const v = fs.readFileSync(p, 'utf8').trim();
            if (v) return { source: 'file', detected: true };
        }
    } catch {
        return { source: 'unknown', detected: true };
    }
    return { source: 'none', detected: false };
}

/**
 * 免费模式边界：检测到 license 来源时拒绝，除非显式允许
 * @param {object} [clearcoteCfg]
 * @returns {{status: 'free-requested'|'license-detected'|'unknown', source: string}}
 */
export function resolveLicenseBoundary(clearcoteCfg = {}) {
    const detected = detectClearcoteLicenseSource();
    if (!detected.detected) {
        return { status: 'free-requested', source: 'none' };
    }
    if (clearcoteCfg.allowDetectedLicense === true) {
        return { status: 'license-detected', source: detected.source };
    }
    throw new Error(
        '检测到 Clearcote license 来源（' + detected.source + '）。' +
        '本项目免费模式不得自动使用 PRO/license。' +
        '请清除 CLEARCOTE_LICENSE_KEY 与 ~/.clearcote/license.key 后重试，' +
        '或设置 browser.clearcote.allowDetectedLicense: true 显式接受（不会读取/打印 key）。'
    );
}

/**
 * 从 SDK RELEASE 常量取浏览器版本（不得把 npm 版本当浏览器版本）
 * @param {object} [sdk]
 * @returns {{browserVersion: string|null, releaseTag: string|null, releaseAsset: string|null}}
 */
export function readClearcoteReleaseInfo(sdk = null) {
    const release = sdk?.RELEASE || null;
    if (release && typeof release === 'object') {
        return {
            browserVersion: release.version ? String(release.version) : null,
            releaseTag: release.tag ? String(release.tag) : null,
            releaseAsset: release.asset ? String(release.asset) : null
        };
    }
    return { browserVersion: null, releaseTag: null, releaseAsset: null };
}

/**
 * 构建 Clearcote runtime 诊断信息
 * @param {object} params
 * @returns {object}
 */
export function buildClearcoteRuntimeMeta(params = {}) {
    const {
        hostPlatform = process.platform,
        binarySource = params.executablePath ? 'explicit-path' : 'sdk-resolve',
        executablePath = null,
        fingerprintPlatform = null,
        fingerprintSource = 'none',
        seedSource = null,
        browserVersion = null,
        releaseInfo = null,
        licenseStatus = 'unknown',
        licenseSource = 'unknown',
        sandboxEnabled = true,
        sdkModule = null
    } = params;

    const sdkVersion = readClearcoteSdkVersion() || CLEARCOTE_SDK_PINNED;
    const rel = releaseInfo || readClearcoteReleaseInfo(sdkModule);
    // 显式外部 executable 且无 browser.version() 时不得把 SDK/RELEASE 冒充为已验证浏览器版本
    let version = browserVersion || rel.browserVersion || null;
    let versionVerified = Boolean(browserVersion);
    if (!browserVersion && binarySource === 'explicit-path' && !rel.browserVersion) {
        version = null;
    }

    return {
        engine: 'clearcote',
        sdk: sdkVersion,
        sdkVersion,
        // version 表示浏览器版本；与 sdkVersion 分离
        version: version || null,
        versionVerified,
        release: rel.releaseTag
            ? `clearcote@${rel.releaseTag}`
            : `clearcote-sdk@${sdkVersion}`,
        browserVersion: version,
        platform: hostPlatform,
        fingerprintPlatform,
        fingerprintSource: fingerprintSource || seedSource || 'none',
        binarySource,
        binaryPath: executablePath,
        seedSource: fingerprintSource || seedSource || 'none',
        license: {
            status: licenseStatus,
            source: licenseSource
        },
        capabilities: {
            launchPersistentContext: true,
            nativeHumanize: true,
            geoip: true,
            // 不硬编码 proLicense；由 license.status 表达
            proLicense: licenseStatus === 'license-detected',
            sandboxEnabled: sandboxEnabled !== false,
            hostSupported: hostPlatform === 'win32' || hostPlatform === 'linux'
        }
    };
}

/**
 * 预检 Clearcote：平台/架构 + SDK 可导入 + 显式 path 为 regular file + license 边界
 * @param {object} clearcoteCfg
 * @param {string} [hostPlatform]
 * @param {string} [hostArch]
 * @returns {string[]} errors
 */
export function preflightClearcote(clearcoteCfg = {}, hostPlatform = process.platform, hostArch = process.arch) {
    const errors = [];

    try {
        assertClearcoteHostSupported(hostPlatform, hostArch);
    } catch (e) {
        errors.push(e.message);
        return errors;
    }

    const platformSetting = (clearcoteCfg.platform || 'auto').toLowerCase();
    if (!['auto', 'windows', 'linux'].includes(platformSetting)) {
        errors.push(`browser.clearcote.platform 非法: ${clearcoteCfg.platform}`);
    }

    if (clearcoteCfg.sandbox !== undefined && typeof clearcoteCfg.sandbox !== 'boolean') {
        errors.push('browser.clearcote.sandbox 必须是布尔值');
    }

    if (clearcoteCfg.path) {
        if (typeof clearcoteCfg.path !== 'string') {
            errors.push('browser.clearcote.path 必须是字符串');
        } else {
            try {
                const st = fs.statSync(clearcoteCfg.path);
                if (!st.isFile()) {
                    errors.push(`browser.clearcote.path 必须是可访问的普通文件: ${clearcoteCfg.path}`);
                }
            } catch {
                errors.push(`browser.clearcote.path 不存在或不可访问: ${clearcoteCfg.path}`);
            }
        }
    }

    if (clearcoteCfg.fingerprintProfile) {
        if (typeof clearcoteCfg.fingerprintProfile !== 'string') {
            errors.push('browser.clearcote.fingerprintProfile 必须是字符串路径');
        } else if (clearcoteCfg.fingerprintProfile.trim()) {
            try {
                const st = fs.statSync(clearcoteCfg.fingerprintProfile);
                if (!st.isFile()) {
                    errors.push('browser.clearcote.fingerprintProfile 必须是可读普通文件');
                } else {
                    fs.accessSync(clearcoteCfg.fingerprintProfile, fs.constants.R_OK);
                }
            } catch {
                errors.push('browser.clearcote.fingerprintProfile 不可读（不回显路径内容）');
            }
        }
    }

    // 免费/PRO 边界
    if (clearcoteCfg.allowDetectedLicense !== undefined && typeof clearcoteCfg.allowDetectedLicense !== 'boolean') {
        errors.push('browser.clearcote.allowDetectedLicense 必须是布尔值');
    } else {
        try {
            resolveLicenseBoundary(clearcoteCfg);
        } catch (e) {
            errors.push(e.message);
        }
    }

    // root + linux + 默认 sandbox：给出可操作提示
    if (hostPlatform === 'linux' && typeof process.getuid === 'function' && process.getuid() === 0
        && clearcoteCfg.sandbox !== false) {
        errors.push(
            '在 root 下启用安全 sandbox 可能失败。请为 chrome-sandbox 设置 setuid（chown root:root && chmod 4755），' +
            '或显式设置 browser.clearcote.sandbox: false（降低安全性）并知晓风险。'
        );
    }

    // clearcote 为 ESM-only，exports 未导出 ./package.json，
    // 因此不能用 require.resolve('clearcote/package.json')；与 readClearcoteSdkVersion 一致走磁盘路径。
    const pkgPath = path.join(process.cwd(), 'node_modules', 'clearcote', 'package.json');
    if (!fs.existsSync(pkgPath)) {
        errors.push('未找到 clearcote npm 包，请运行: pnpm install（需 clearcote@0.30.0）');
    } else {
        try {
            const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
            if (pkg.version && pkg.version !== CLEARCOTE_SDK_PINNED) {
                errors.push(
                    `clearcote 版本与项目锁定不一致: 安装 ${pkg.version}，期望 ${CLEARCOTE_SDK_PINNED}`
                );
            }
        } catch (e) {
            errors.push(`clearcote package.json 无法解析: ${e.message}`);
        }
    }

    return errors;
}

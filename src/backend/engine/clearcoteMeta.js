/**
 * @fileoverview Clearcote 元数据与 SDK 懒加载
 */

import fs from 'fs';
import path from 'path';

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
        seedSource = null
    } = params;

    return {
        engine: 'clearcote',
        sdk: readClearcoteSdkVersion() || CLEARCOTE_SDK_PINNED,
        version: readClearcoteSdkVersion() || CLEARCOTE_SDK_PINNED,
        release: `clearcote-sdk@${readClearcoteSdkVersion() || CLEARCOTE_SDK_PINNED}`,
        platform: hostPlatform,
        fingerprintPlatform,
        binarySource,
        binaryPath: executablePath,
        seedSource,
        capabilities: {
            launchPersistentContext: true,
            nativeHumanize: true,
            geoip: true,
            proLicense: false,
            hostSupported: hostPlatform === 'win32' || hostPlatform === 'linux'
        }
    };
}

/**
 * 预检 Clearcote：平台 + SDK 可导入 + 显式 path
 * @param {object} clearcoteCfg
 * @param {string} [hostPlatform]
 * @returns {string[]} errors
 */
export function preflightClearcote(clearcoteCfg = {}, hostPlatform = process.platform) {
    const errors = [];

    if (hostPlatform === 'darwin') {
        errors.push(
            'Clearcote 不支持 macOS（官方仍在 roadmap）。请改用 browser.engine=camoufox，或在 Windows/Linux x64 部署。'
        );
        return errors;
    }
    if (hostPlatform !== 'win32' && hostPlatform !== 'linux') {
        errors.push(`Clearcote 不支持宿主平台 ${hostPlatform}`);
        return errors;
    }

    const platformSetting = (clearcoteCfg.platform || 'auto').toLowerCase();
    if (!['auto', 'windows', 'linux'].includes(platformSetting)) {
        errors.push(`browser.clearcote.platform 非法: ${clearcoteCfg.platform}`);
    }

    if (clearcoteCfg.path) {
        if (!fs.existsSync(clearcoteCfg.path)) {
            errors.push(`browser.clearcote.path 不存在: ${clearcoteCfg.path}`);
        }
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

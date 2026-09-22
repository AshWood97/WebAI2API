/**
 * @fileoverview 系统信息工具模块
 * @description 提供系统状态、数据文件夹管理等功能
 */

import os from 'os';
import fs from 'fs';
import path from 'path';
import { logger } from './logger.js';
import {
    isManagedUserDataFolder,
    parseManagedUserDataFolder,
    resolveManagedUserDataPath
} from '../backend/engine/engineContract.js';

// 服务启动时间
const startTime = Date.now();

// 版本信息（从 package.json 读取）
let version = '1.0.0';
try {
    const pkgPath = path.join(process.cwd(), 'package.json');
    if (fs.existsSync(pkgPath)) {
        const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
        version = pkg.version || '1.0.0';
    }
} catch (e) { /* ignore */ }

// CPU 使用率采样数据
let lastCpuInfo = null;

/**
 * 获取 CPU 使用率（跨平台）
 * @returns {number} CPU 使用率百分比
 */
function getCpuUsage() {
    const cpus = os.cpus();
    if (cpus.length === 0) return 0;

    let totalIdle = 0;
    let totalTick = 0;

    for (const cpu of cpus) {
        for (const type in cpu.times) {
            totalTick += cpu.times[type];
        }
        totalIdle += cpu.times.idle;
    }

    const currentInfo = { idle: totalIdle, total: totalTick };

    if (!lastCpuInfo) {
        lastCpuInfo = currentInfo;
        return 0; // 第一次调用无法计算
    }

    const idleDiff = currentInfo.idle - lastCpuInfo.idle;
    const totalDiff = currentInfo.total - lastCpuInfo.total;
    lastCpuInfo = currentInfo;

    if (totalDiff === 0) return 0;
    const usage = 100 - (idleDiff / totalDiff * 100);
    return Math.round(usage * 10) / 10;
}

/**
 * 获取系统状态
 * @returns {object} 系统状态信息
 */
export function getSystemStatus() {
    const totalMem = os.totalmem();
    const freeMem = os.freemem();
    const usedMem = totalMem - freeMem;

    // 检测运行模式
    const isXvfb = !!process.env.XVFB_RUNNING;
    const isHeadless = process.env.HEADLESS === 'true';

    return {
        status: isXvfb ? 'xvfb' : (isHeadless ? 'headless' : 'normal'),
        version,
        systemVersion: `${os.type()} ${os.release()}`,
        uptime: Math.floor((Date.now() - startTime) / 1000),
        cpuUsage: getCpuUsage(),
        memoryUsage: {
            total: Math.round(totalMem / 1024 / 1024),
            used: Math.round(usedMem / 1024 / 1024),
            free: Math.round(freeMem / 1024 / 1024)
        }
    };
}

/**
 * 获取数据文件夹列表
 * @param {object[]} workers - 当前 Worker 配置列表
 * @returns {object[]} 数据文件夹信息
 */
export function getDataFolders(workers = []) {
    const dataDir = path.join(process.cwd(), 'data');

    if (!fs.existsSync(dataDir)) {
        return [];
    }

    const folders = [];
    const entries = fs.readdirSync(dataDir, { withFileTypes: true });

    // 构建 userDataDir -> workerName 映射
    const workerMap = new Map();
    for (const w of workers) {
        if (w.userDataDir) {
            workerMap.set(path.basename(w.userDataDir), w.name);
        }
    }

    for (const entry of entries) {
        if (entry.isDirectory() && isManagedUserDataFolder(entry.name)) {
            const folderPath = path.join(dataDir, entry.name);
            let size = 0;

            // 计算文件夹大小（递归，但限制深度避免性能问题）
            try {
                size = getFolderSize(folderPath, 3);
            } catch (e) { /* ignore */ }

            const parsed = parseManagedUserDataFolder(entry.name);
            const engine = parsed?.engine || (entry.name.startsWith('clearcoteUserData') ? 'clearcote' : 'camoufox');
            folders.push({
                name: entry.name,
                path: `data/${entry.name}`,
                size: formatSize(size),
                sizeBytes: size,
                engine,
                instance: workerMap.get(entry.name) || null
            });
        }
    }

    return folders;
}

/**
 * 删除指定的数据文件夹
 * @param {string[]} folderNames - 要删除的文件夹名称列表
 * @param {object[]} workers - 当前 Worker 配置列表（用于检查是否正在使用）
 * @returns {{success: boolean, deleted: string[], errors: string[]}}
 */
export function deleteDataFolders(folderNames, workers = []) {
    const dataDir = path.join(process.cwd(), 'data');
    const deleted = [];
    const errors = [];

    // 构建正在使用的文件夹集合
    const inUse = new Set();
    for (const w of workers) {
        if (w.userDataDir) {
            inUse.add(path.basename(w.userDataDir));
        }
    }

    for (const name of folderNames) {
        // 安全检查：精确托管目录名 + canonical 路径边界（拒绝穿越/伪造前缀/越界 symlink）
        if (!isManagedUserDataFolder(name)) {
            errors.push(`${name}: 不允许删除非用户数据文件夹`);
            continue;
        }

        let folderPath;
        try {
            folderPath = resolveManagedUserDataPath(name, dataDir);
        } catch (e) {
            errors.push(`${name}: ${e.message}`);
            continue;
        }

        // 检查是否正在使用
        if (inUse.has(name)) {
            errors.push(`${name}: 文件夹正在被 Worker 使用`);
            continue;
        }

        // 检查是否存在；拒绝指向 data/ 外的符号链接
        try {
            const st = fs.lstatSync(folderPath);
            if (st.isSymbolicLink()) {
                let real;
                try {
                    real = fs.realpathSync(folderPath);
                } catch {
                    errors.push(`${name}: 符号链接无法解析，拒绝删除`);
                    continue;
                }
                const baseReal = fs.realpathSync(dataDir);
                const rel = path.relative(baseReal, real);
                if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) {
                    errors.push(`${name}: 符号链接指向 data/ 之外，拒绝删除`);
                    continue;
                }
            } else if (!st.isDirectory()) {
                errors.push(`${name}: 不是目录`);
                continue;
            }
        } catch {
            errors.push(`${name}: 文件夹不存在`);
            continue;
        }

        // 删除文件夹（仅 canonical 路径；force 不跳出 data/）
        try {
            fs.rmSync(folderPath, { recursive: true, force: true });
            deleted.push(name);
            logger.info('系统', `已删除数据文件夹: ${name}`);
        } catch (e) {
            errors.push(`${name}: ${e.message}`);
        }
    }

    return {
        success: errors.length === 0,
        deleted,
        errors
    };
}

/**
 * 清理临时文件
 * @param {string} tempDir - 临时目录路径
 * @returns {{success: boolean, cleaned: number}}
 */
export function clearTempFiles(tempDir) {
    if (!tempDir || !fs.existsSync(tempDir)) {
        return { success: true, cleaned: 0 };
    }

    let cleaned = 0;
    try {
        const files = fs.readdirSync(tempDir);
        for (const file of files) {
            try {
                fs.unlinkSync(path.join(tempDir, file));
                cleaned++;
            } catch (e) { /* ignore */ }
        }
        logger.info('系统', `已清理 ${cleaned} 个临时文件`);
    } catch (e) {
        logger.warn('系统', `清理临时文件失败: ${e.message}`);
    }

    return { success: true, cleaned };
}

// ==================== 辅助函数 ====================

/**
 * 计算文件夹大小
 * @param {string} dirPath - 目录路径
 * @param {number} maxDepth - 最大深度
 * @returns {number} 字节数
 */
function getFolderSize(dirPath, maxDepth) {
    if (maxDepth <= 0) return 0;

    let size = 0;
    const entries = fs.readdirSync(dirPath, { withFileTypes: true });

    for (const entry of entries) {
        const fullPath = path.join(dirPath, entry.name);
        if (entry.isFile()) {
            try {
                size += fs.statSync(fullPath).size;
            } catch (e) { /* ignore */ }
        } else if (entry.isDirectory()) {
            size += getFolderSize(fullPath, maxDepth - 1);
        }
    }

    return size;
}

/**
 * 格式化文件大小
 * @param {number} bytes - 字节数
 * @returns {string} 格式化后的字符串
 */
function formatSize(bytes) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
    return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

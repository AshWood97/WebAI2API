/**
 * @fileoverview 运行环境初始化脚本（CLI）
 * @description 用于下载/准备运行所需依赖（如 Camoufox、better-sqlite3 等）。
 *
 * 用法：
 *   npm run init                     # 自动初始化（无代理）
 *   npm run init -- -proxy           # 自动初始化（交互式输入代理）
 *   npm run init -- -proxy=http://127.0.0.1:7890
 *   npm run init -- -proxy=socks5://user:pass@127.0.0.1:1080
 *   npm run init -- -custom          # 自定义模式
 */

import fs from 'fs';
import path from 'path';
import os from 'os';
import https from 'https';
import http from 'http';
import { fileURLToPath } from 'url';
import compressing from 'compressing';
import { logger } from '../src/utils/logger.js';
import { select, input } from '@inquirer/prompts';
import { SocksProxyAgent } from 'socks-proxy-agent';
import { HttpsProxyAgent } from 'https-proxy-agent';
import { buildCamoufoxDownloadUrl } from '../src/backend/engine/camoufoxMeta.js';
import { CAMOUFOX_RELEASE, CAMOUFOX_VERSION_JSON } from './camoufoxRelease.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = path.join(__dirname, '..');
const TEMP_DIR = path.join(PROJECT_ROOT, 'data', 'temp');

// 与 camoufox-js@0.12 对齐：便携安装目录（与 PROJECT_ROOT 一致，避免 cwd 漂移）
process.env.CAMOUFOX_INSTALL_DIR = process.env.CAMOUFOX_INSTALL_DIR
    || path.join(PROJECT_ROOT, 'camoufox');

export { CAMOUFOX_RELEASE, CAMOUFOX_VERSION_JSON };

/**
 * 解析命令行代理参数
 * @returns {Promise<string|null>} 代理 URL
 */
async function parseProxyArg() {
    // 查找 -proxy 或 -proxy=xxx 参数
    const proxyArg = process.argv.find(arg => arg.startsWith('-proxy'));

    if (!proxyArg) {
        return null;
    }

    // -proxy=http://... 格式
    if (proxyArg.includes('=')) {
        const proxyUrl = proxyArg.split('=')[1];
        if (proxyUrl) {
            logger.info('初始化', `使用代理: ${proxyUrl}`);
            return proxyUrl;
        }
    }

    // -proxy 不带参数，交互式输入
    logger.info('初始化', '请输入代理配置...');

    const proxyType = await select({
        message: '代理类型',
        choices: [
            { name: 'HTTP', value: 'http' },
            { name: 'SOCKS5', value: 'socks5' }
        ]
    });

    const host = await input({
        message: '代理服务器地址',
        default: '127.0.0.1',
        validate: (val) => val.trim().length > 0 || '地址不能为空'
    });

    const port = await input({
        message: '代理端口',
        default: '7890',
        validate: (val) => {
            const num = parseInt(val, 10);
            return (num > 0 && num <= 65535) || '端口必须是 1-65535 的数字';
        }
    });

    const username = await input({
        message: '用户名 (可选，回车跳过)',
    });

    const password = await input({
        message: '密码 (可选，回车跳过)',
    });

    // 构建代理 URL
    let proxyUrl = `${proxyType}://`;
    if (username && password) {
        proxyUrl += `${encodeURIComponent(username)}:${encodeURIComponent(password)}@`;
    } else if (username) {
        proxyUrl += `${encodeURIComponent(username)}@`;
    }
    proxyUrl += `${host}:${port}`;

    logger.info('初始化', `使用代理: ${proxyUrl}`);
    return proxyUrl;
}

// 确保临时目录存在
if (!fs.existsSync(TEMP_DIR)) {
    fs.mkdirSync(TEMP_DIR, { recursive: true });
}

/**
 * 获取 Node.js ABI 版本
 */
function getNodeABI() {
    return process.versions.modules;
}

/**
 * 获取平台信息
 */
function getPlatformInfo() {
    const platform = os.platform();
    const arch = os.arch();
    const nodeVersion = process.version;
    const abi = getNodeABI();

    return { platform, arch, nodeVersion, abi };
}

/**
 * 验证平台支持
 */
function validatePlatform(platform, arch) {
    const supported = {
        'win32': ['x64'],
        'darwin': ['x64', 'arm64'],
        'linux': ['x64', 'arm64']
    };

    if (!supported[platform] || !supported[platform].includes(arch)) {
        return false;
    }

    return true;
}

/**
 * 验证 Node.js ABI 版本支持
 */
function validateABI(abi) {
    const supportedABIs = [115, 121, 123, 125, 127, 128, 130, 131, 132, 133, 135, 136, 137, 139, 140, 141];
    return supportedABIs.includes(parseInt(abi, 10));
}

/**
 * 下载文件（带进度，流式，支持重试）
 * @param {string} url - 下载地址
 * @param {string} destPath - 目标文件路径
 * @param {string|null} proxyUrl - 代理 URL（支持 http:// 和 socks5://）
 * @param {number} maxRetries - 最大重试次数
 */
async function downloadFile(url, destPath, proxyUrl = null, maxRetries = 3) {
    if (proxyUrl) {
        const proxyType = proxyUrl.startsWith('socks') ? 'SOCKS5' : 'HTTP';
        logger.info('初始化', `使用 ${proxyType} 代理: ${proxyUrl}`);
    }

    for (let attempt = 1; attempt <= maxRetries; attempt++) {
        try {
            if (attempt > 1) {
                logger.info('初始化', `第 ${attempt}/${maxRetries} 次尝试下载...`);
                // 删除之前失败的文件
                try {
                    if (fs.existsSync(destPath)) {
                        fs.unlinkSync(destPath);
                    }
                } catch (e) { }
            } else {
                logger.info('初始化', `开始下载: ${url}`);
            }

            await downloadFileOnce(url, destPath, proxyUrl);
            return destPath;
        } catch (error) {
            logger.error('初始化', `下载失败 (尝试 ${attempt}/${maxRetries}): ${error.message}`);

            if (attempt === maxRetries) {
                throw error;
            }

            // 等待后重试（递增延迟）
            const delay = attempt * 2000;
            logger.info('初始化', `${delay / 1000} 秒后重试...`);
            await new Promise(resolve => setTimeout(resolve, delay));
        }
    }
}

/**
 * 单次下载尝试（内部函数）
 * 使用 Node.js 原生 http/https 模块，支持 SOCKS5 和 HTTP 代理
 */
async function downloadFileOnce(url, destPath, proxyUrl = null) {
    const IDLE_TIMEOUT = 180000; // 3 分钟无数据传输才超时

    return new Promise((resolve, reject) => {
        const urlObj = new URL(url);
        const isHttps = urlObj.protocol === 'https:';

        let requestOptions = {
            hostname: urlObj.hostname,
            port: urlObj.port || (isHttps ? 443 : 80),
            path: urlObj.pathname + urlObj.search,
            method: 'GET',
            headers: {
                'User-Agent': 'Wget/1.21.4 (linux-gnu)',
                'Accept': '*/*',
                'Accept-Encoding': 'identity',
                'Connection': 'keep-alive'
            }
        };

        // 创建代理 agent
        let httpModule = isHttps ? https : http;
        let agent = null;

        if (proxyUrl) {
            if (proxyUrl.startsWith('socks')) {
                // SOCKS5 代理，使用 socks-proxy-agent
                logger.debug('初始化', `使用 SOCKS5 代理: ${proxyUrl}`);
                agent = new SocksProxyAgent(proxyUrl);
            } else if (proxyUrl.startsWith('http')) {
                // HTTP 代理，使用 https-proxy-agent
                logger.debug('初始化', `使用 HTTP 代理: ${proxyUrl}`);
                agent = new HttpsProxyAgent(proxyUrl);
            }
        }

        // 添加 agent 到请求选项
        if (agent) {
            requestOptions.agent = agent;
        }

        const fileStream = fs.createWriteStream(destPath);
        let downloadedSize = 0;
        let totalSize = 0;
        let lastLogTime = Date.now();
        let finished = false;
        let idleTimer = null;
        let req = null;

        const resetIdleTimer = () => {
            if (idleTimer) clearTimeout(idleTimer);
            idleTimer = setTimeout(() => {
                if (!finished) {
                    const error = new Error(`下载超时: ${IDLE_TIMEOUT / 1000} 秒内没有收到任何数据`);
                    cleanup();
                    reject(error);
                }
            }, IDLE_TIMEOUT);
        };

        const cleanup = () => {
            finished = true;
            if (idleTimer) clearTimeout(idleTimer);
            if (req) {
                try { req.destroy(); } catch (e) { }
            }
            fileStream.close();
        };

        const handleResponse = (res) => {
            resetIdleTimer();

            // 处理重定向
            if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
                cleanup();
                try { fs.unlinkSync(destPath); } catch (e) { }
                logger.info('初始化', `重定向到: ${res.headers.location}`);
                // 递归调用处理重定向
                downloadFileOnce(res.headers.location, destPath, proxyUrl)
                    .then(resolve)
                    .catch(reject);
                return;
            }

            if (res.statusCode !== 200) {
                cleanup();
                try { fs.unlinkSync(destPath); } catch (e) { }
                reject(new Error(`HTTP 错误: ${res.statusCode}`));
                return;
            }

            totalSize = parseInt(res.headers['content-length'] || '0', 10);
            if (totalSize > 0) {
                logger.info('初始化', `文件大小: ${(totalSize / 1024 / 1024).toFixed(2)} MB`);
            }

            res.on('data', (chunk) => {
                resetIdleTimer();
                downloadedSize += chunk.length;

                const now = Date.now();
                if (totalSize > 0 && now - lastLogTime > 100) {  // 100ms 更新一次，更流畅
                    const percent = ((downloadedSize / totalSize) * 100).toFixed(1);
                    const downloadedMB = (downloadedSize / 1024 / 1024).toFixed(2);
                    const totalMB = (totalSize / 1024 / 1024).toFixed(2);
                    // 使用 \r 回到行首，实现单行刷新
                    process.stdout.write(`\r下载进度: ${percent}% (${downloadedMB}MB / ${totalMB}MB)    `);
                    lastLogTime = now;
                }
            });

            res.on('error', (error) => {
                if (finished) return;
                cleanup();
                try { fs.unlinkSync(destPath); } catch (e) { }
                reject(error);
            });

            res.pipe(fileStream);

            fileStream.on('error', (error) => {
                if (finished) return;
                cleanup();
                reject(error);
            });

            fileStream.on('finish', () => {
                if (finished) return;
                finished = true;
                if (idleTimer) clearTimeout(idleTimer);

                const finalSize = (downloadedSize / 1024 / 1024).toFixed(2);

                if (totalSize > 0 && downloadedSize !== totalSize) {
                    process.stdout.write('\n');  // 换行，避免与进度条混在一起
                    const errorMsg = `下载不完整: 预期 ${(totalSize / 1024 / 1024).toFixed(2)} MB, 实际 ${finalSize} MB`;
                    logger.error('初始化', errorMsg);
                    try { fs.unlinkSync(destPath); } catch (e) { }
                    reject(new Error(errorMsg));
                    return;
                }

                process.stdout.write('\n');  // 换行，结束进度条
                logger.info('初始化', `下载完成: ${finalSize} MB`);
                resolve(destPath);
            });
        };

        resetIdleTimer();

        // 统一使用 httpModule.request 发起请求（agent 会自动处理代理）
        req = httpModule.request(requestOptions, handleResponse);
        req.on('error', (error) => {
            if (finished) return;
            cleanup();
            try { fs.unlinkSync(destPath); } catch (e) { }
            reject(error);
        });
        req.end();
    });
}

/**
 * 构建 better-sqlite3 下载 URL
 */
function getBetterSqlite3Url(platform, arch, abi) {
    const version = '12.5.0';
    const platformMap = {
        'win32': 'win32',
        'darwin': 'darwin',
        'linux': 'linux'
    };

    const platformName = platformMap[platform];
    const archName = arch; // x64 或 arm64

    return `https://github.com/WiseLibs/better-sqlite3/releases/download/v${version}/better-sqlite3-v${version}-node-v${abi}-${platformName}-${archName}.tar.gz`;
}

/**
 * 下载并安装 better-sqlite3
 */
async function installBetterSqlite3(platform, arch, abi, proxyUrl) {
    logger.info('初始化', '开始安装 better-sqlite3...');

    const url = getBetterSqlite3Url(platform, arch, abi);
    const downloadPath = path.join(TEMP_DIR, 'better-sqlite3.tar.gz');

    // 下载
    await downloadFile(url, downloadPath, proxyUrl);

    // 解压 .tar.gz 文件
    logger.info('初始化', '正在解压 better-sqlite3...');
    await compressing.tgz.uncompress(downloadPath, TEMP_DIR);

    // 查找 better_sqlite3.node
    const files = fs.readdirSync(TEMP_DIR, { recursive: true });
    const nodeFile = files.find(f => f.endsWith('better_sqlite3.node'));
    if (!nodeFile) {
        throw new Error('未找到 better_sqlite3.node 文件');
    }

    // 复制到 node_modules
    const buildDir = path.join(PROJECT_ROOT, 'node_modules', 'better-sqlite3', 'build', 'Release');
    if (!fs.existsSync(buildDir)) {
        fs.mkdirSync(buildDir, { recursive: true });
    }

    const sourcePath = path.join(TEMP_DIR, nodeFile);
    const destPath = path.join(buildDir, 'better_sqlite3.node');
    fs.copyFileSync(sourcePath, destPath);

    logger.info('初始化', `better-sqlite3 安装成功: ${destPath}`);

    // 清理
    fs.unlinkSync(downloadPath);
    // 清理解压后的所有文件
    files.forEach(f => {
        const filePath = path.join(TEMP_DIR, f);
        try {
            if (fs.existsSync(filePath)) {
                const stat = fs.statSync(filePath);
                if (stat.isDirectory()) {
                    fs.rmSync(filePath, { recursive: true, force: true });
                } else {
                    fs.unlinkSync(filePath);
                }
            }
        } catch (e) { }
    });
}

/**
 * 构建 Camoufox 下载 URL
 */
function getCamoufoxUrl(platform, arch) {
    return buildCamoufoxDownloadUrl(CAMOUFOX_RELEASE, platform, arch);
}

/**
 * 下载并安装 Camoufox
 */
async function installCamoufox(platform, arch, proxyUrl) {
    logger.info('初始化', '开始安装 Camoufox 浏览器...');

    const url = getCamoufoxUrl(platform, arch);
    const downloadPath = path.join(TEMP_DIR, 'camoufox.zip');

    // 下载
    await downloadFile(url, downloadPath, proxyUrl);

    // 解压 .zip 文件到 camoufox 目录
    logger.info('初始化', '正在解压 Camoufox...');
    const camoufoxDir = path.join(PROJECT_ROOT, 'camoufox');
    if (!fs.existsSync(camoufoxDir)) {
        fs.mkdirSync(camoufoxDir, { recursive: true });
    }

    await compressing.zip.uncompress(downloadPath, camoufoxDir);

    // macOS 专用：复制 properties.json 到 MacOS 目录
    if (platform === 'darwin') {
        const resourcesPath = path.join(camoufoxDir, 'Camoufox.app', 'Contents', 'Resources', 'properties.json');
        const macOSDir = path.join(camoufoxDir, 'Camoufox.app', 'Contents', 'MacOS');
        const macOSPath = path.join(macOSDir, 'properties.json');

        if (fs.existsSync(resourcesPath)) {
            // 确保目标目录存在
            if (!fs.existsSync(macOSDir)) {
                fs.mkdirSync(macOSDir, { recursive: true });
            }
            fs.copyFileSync(resourcesPath, macOSPath);
            logger.info('初始化', `已复制 properties.json 到 MacOS 目录`);
        } else {
            logger.warn('初始化', `未找到 properties.json: ${resourcesPath}`);
        }
    }

    logger.info('初始化', `Camoufox 安装成功: ${camoufoxDir}`);

    // 创建 version.json
    const versionJsonPath = path.join(camoufoxDir, 'version.json');
    const versionData = { ...CAMOUFOX_VERSION_JSON };
    fs.writeFileSync(versionJsonPath, JSON.stringify(versionData, null, 2), 'utf8');
    logger.info('初始化', `已生成 version.json: ${versionJsonPath}`);

    // 清理
    fs.unlinkSync(downloadPath);
}


/**
 * 主流程
 */
import { fileURLToPath as _initFileUrl } from 'url';
const _isInitMain = process.argv[1] === _initFileUrl(import.meta.url);

async function main() {
    try {
        logger.info('初始化', '========================================');
        logger.info('初始化', '依赖初始化脚本启动');
        logger.info('初始化', '========================================');

        // 代理使用提示
        if (!process.argv.some(arg => arg.startsWith('-proxy'))) {
            logger.warn('初始化', '该脚本需连接 GitHub 下载资源。若网络受限，请使用代理：');
            logger.warn('初始化', ' - 用法: npm run init -- -proxy 可交互式填写代理信息');
            logger.warn('初始化', ' - 同时支持直接传入参数或者使用带鉴权的代理 (支持HTTP和SOCKS5)');
            logger.warn('初始化', ' - 示例: npm run init -- -proxy=http://username:passwd@127.0.0.1:7890');
        }

        // 显示系统信息
        const { platform, arch, nodeVersion, abi } = getPlatformInfo();
        logger.info('初始化', `操作系统: ${platform}`);
        logger.info('初始化', `芯片架构: ${arch}`);
        logger.info('初始化', `Node.js 版本: ${nodeVersion}`);
        logger.info('初始化', `Node.js ABI 版本: ${abi}`);

        // 验证平台支持
        if (!validatePlatform(platform, arch)) {
            logger.error('初始化', '不支持的平台！');
            logger.error('初始化', `因该项目使用了 Camoufox 浏览器，没有您设备可用的预编译版本`);
            logger.error('初始化', `支持的平台: Windows x64, macOS x64/arm64, Linux x64/arm64`);
            process.exit(1);
        }

        logger.info('初始化', '平台支持检查通过');

        // 验证 ABI 版本支持
        if (!validateABI(abi)) {
            logger.error('初始化', '不支持的 Node.js ABI 版本！');
            logger.error('初始化', `当前 ABI 版本: ${abi}`);
            logger.error('初始化', `支持的 ABI 版本: 115, 121, 123, 125, 127, 128, 130, 131, 132, 133, 135, 136, 137, 139, 140, 141`);
            logger.error('初始化', `建议使用 Node.js 20.10.0 或更高版本`);
            process.exit(1);
        }

        logger.info('初始化', 'ABI 版本检查通过');

        // 解析代理参数
        const proxyUrl = await parseProxyArg();

        // 检查是否为自定义模式
        const isCustomMode = process.argv.includes('-custom');

        if (isCustomMode) {
            // 自定义模式：交互式选择步骤
            const action = await select({
                message: '请选择要执行的操作:',
                choices: [
                    { name: '安装 better-sqlite3 预编译文件', value: 'sqlite' },
                    { name: '安装 Camoufox 浏览器', value: 'camoufox' },
                    { name: '安装 GeoLite2-City.mmdb 数据库', value: 'geolite' },
                    { name: '修复 macOS 环境下的 properties.json', value: 'macos_fix' },
                    { name: '修复 version.json 缺失', value: 'version_fix' },
                    { name: '退出', value: 'exit' }
                ]
            });

            switch (action) {
                case 'sqlite':
                    await installBetterSqlite3(platform, arch, abi, proxyUrl);
                    break;
                case 'camoufox':
                    await installCamoufox(platform, arch, proxyUrl);
                    break;
                case 'geolite':
                    await downloadGeoLiteDb(proxyUrl, true); // 强制下载
                    break;
                case 'macos_fix':
                    fixMacOSProperties();
                    break;
                case 'version_fix':
                    fixVersionJson();
                    break;
                case 'exit':
                    logger.info('初始化', '已退出');
                    break;
            }
        } else {
            // 正常模式：执行所有步骤
            await installBetterSqlite3(platform, arch, abi, proxyUrl);
            await installCamoufox(platform, arch, proxyUrl);
            await downloadGeoLiteDb(proxyUrl);
        }

        logger.info('初始化', '========================================');
        logger.info('初始化', '操作完成！');
        logger.info('初始化', '========================================');
        process.exit(0);

    } catch (err) {
        logger.error('初始化', '初始化失败', { error: err.message });
        process.exit(1);
    }
}

if (_isInitMain) {
    main();
}


/**
 * 下载 GeoLite2-City.mmdb 到 camoufox 目录
 * @param {string|null} proxyUrl - 代理 URL
 * @param {boolean} [force=false] - 是否强制下载（忽略已存在检查）
 */
async function downloadGeoLiteDb(proxyUrl, force = false) {
    const camoufoxDir = path.join(PROJECT_ROOT, 'camoufox');
    const destPath = path.join(camoufoxDir, 'GeoLite2-City.mmdb');

    // 确保目录存在
    if (!fs.existsSync(camoufoxDir)) {
        fs.mkdirSync(camoufoxDir, { recursive: true });
    }

    // 如果已存在且非强制模式，跳过下载
    if (!force && fs.existsSync(destPath)) {
        logger.info('初始化', 'GeoLite2-City.mmdb 已存在，跳过下载');
        return;
    }

    logger.info('初始化', '开始下载 GeoLite2-City.mmdb...');
    const url = 'https://github.com/P3TERX/GeoLite.mmdb/releases/latest/download/GeoLite2-City.mmdb';
    await downloadFile(url, destPath, proxyUrl);
    logger.info('初始化', `GeoLite2-City.mmdb 下载完成: ${destPath}`);
}

/**
 * 修复 macOS 环境下的 properties.json
 */
function fixMacOSProperties() {
    const platform = os.platform();
    if (platform !== 'darwin') {
        logger.warn('初始化', '此操作仅适用于 macOS 系统');
        return;
    }

    const camoufoxDir = path.join(PROJECT_ROOT, 'camoufox');
    const resourcesPath = path.join(camoufoxDir, 'Camoufox.app', 'Contents', 'Resources', 'properties.json');
    const macOSDir = path.join(camoufoxDir, 'Camoufox.app', 'Contents', 'MacOS');
    const macOSPath = path.join(macOSDir, 'properties.json');

    if (!fs.existsSync(resourcesPath)) {
        logger.error('初始化', `源文件不存在: ${resourcesPath}`);
        logger.error('初始化', '请先安装 Camoufox 浏览器');
        return;
    }

    if (!fs.existsSync(macOSDir)) {
        fs.mkdirSync(macOSDir, { recursive: true });
    }

    fs.copyFileSync(resourcesPath, macOSPath);
    logger.info('初始化', `已复制 properties.json 到 MacOS 目录: ${macOSPath}`);
}

/**
 * 修复 version.json 缺失
 */
function fixVersionJson() {
    const camoufoxDir = path.join(PROJECT_ROOT, 'camoufox');
    const versionJsonPath = path.join(camoufoxDir, 'version.json');

    if (!fs.existsSync(camoufoxDir)) {
        logger.error('初始化', `camoufox 目录不存在: ${camoufoxDir}`);
        logger.error('初始化', '请先安装 Camoufox 浏览器');
        return;
    }

    const versionData = { ...CAMOUFOX_VERSION_JSON };

    fs.writeFileSync(versionJsonPath, JSON.stringify(versionData, null, 2), 'utf8');
    logger.info('初始化', `已生成 version.json: ${versionJsonPath}`);
}

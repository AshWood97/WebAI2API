/**
 * @fileoverview 代理适配模块
 * @description 将配置中的 HTTP/SOCKS5 代理转换为 Playwright 可用的代理配置，并在需要时通过 proxy-chain 搭建本地 HTTP 代理桥接。
 */

import { anonymizeProxy, closeAnonymizedProxy } from 'proxy-chain';
import { logger } from './logger.js';

// 全局代理状态：支持多个 proxy-chain relay（多实例 SOCKS5 认证）
const proxyState = {
    anonymizedProxies: new Set()
};

/**
 * 构建代理 URL
 * @param {object} proxyConfig - 代理配置对象
 * @param {string} proxyConfig.type - 代理类型 ('http' 或 'socks5')
 * @param {string} proxyConfig.host - 代理主机地址
 * @param {number} proxyConfig.port - 代理端口
 * @param {string} [proxyConfig.user] - 可选的用户名
 * @param {string} [proxyConfig.passwd] - 可选的密码
 * @returns {string} - 代理 URL
 */
export function buildProxyUrl(proxyConfig) {
    const { type, host, port, user, passwd } = proxyConfig;

    // 构建带认证的代理 URL
    if (user && passwd) {
        return `${type}://${user}:${passwd}@${host}:${port}`;
    }

    // 构建不带认证的代理 URL
    if (type === 'socks5') {
        return `socks5://${host}:${port}`;
    }

    return `http://${host}:${port}`;
}

/**
 * 将代理转换为 HTTP 代理
 * - HTTP 代理：直接返回
 * - SOCKS5 代理：使用 proxy-chain 转换为本地 HTTP 代理
 * @param {object} proxyConfig - 代理配置对象
 * @returns {Promise<string|null>} - 转换后的 HTTP 代理 URL，如果无需代理则返回 null
 */
export async function getHttpProxy(proxyConfig) {
    if (!proxyConfig || !proxyConfig.enable) {
        return null;
    }

    const { type, host, port } = proxyConfig;
    const originalUrl = buildProxyUrl(proxyConfig);

    // 如果是 HTTP 代理，直接返回
    if (type === 'http') {
        logger.debug('代理器', `使用 HTTP 代理: ${host}:${port}`);
        return originalUrl;
    }

    // 如果是 SOCKS5 代理，需要转换为 HTTP 代理
    if (type === 'socks5') {
        try {
            logger.info('代理器', `检测到 SOCKS5 代理，正在转换为 HTTP 代理: ${host}:${port}`);
            const httpProxyUrl = await anonymizeProxy(originalUrl);

            // 记录全部 relay，便于退出时统一清理
            proxyState.anonymizedProxies.add(httpProxyUrl);

            logger.info('代理器', `SOCKS5 代理已转换为 HTTP 代理: ${httpProxyUrl}`);
            return httpProxyUrl;
        } catch (error) {
            logger.error('代理器', `SOCKS5 代理转换失败: ${error.message}`);
            throw error;
        }
    }

    logger.warn('代理器', `不支持的代理类型: ${type}`);
    return null;
}

/**
 * 获取用于浏览器的代理配置
 * 返回 Playwright 可以使用的代理对象
 * @param {object} proxyConfig - 代理配置对象
 * @returns {Promise<object|null>} - Playwright 代理配置对象
 */
export async function getBrowserProxy(proxyConfig) {
    if (!proxyConfig || !proxyConfig.enable) {
        return null;
    }

    const { type, host, port, user, passwd } = proxyConfig;

    // 构建代理 URL 字符串
    // 注意：Camoufox 对 socks5:// 协议使用 new URL().origin 会返回 null
    // 因此我们直接返回完整的 URL 字符串，让 Camoufox 使用 new URL().href
    let proxyUrl;
    if (user && passwd) {
        // 带认证的代理格式: protocol://user:passwd@host:port
        const protocol = type === 'socks5' ? 'socks5' : 'http';
        proxyUrl = `${protocol}://${encodeURIComponent(user)}:${encodeURIComponent(passwd)}@${host}:${port}`;
    } else {
        // 不带认证的代理格式: protocol://host:port
        if (type === 'socks5') {
            proxyUrl = `socks5://${host}:${port}`;
        } else {
            proxyUrl = `http://${host}:${port}`;
        }
    }

    logger.info('代理器', `代理配置: ${type}://${host}:${port}${user ? ' (带认证)' : ''}`);

    // 直接返回字符串格式，Camoufox 会正确解析
    return proxyUrl;
}

/**
 * 获取 Clearcote/Playwright 标准代理对象
 * - HTTP：拆出 server/username/password
 * - SOCKS5 无认证：直接 socks5://
 * - SOCKS5 带认证：复用 proxy-chain relay（Chromium 无法原生认证 SOCKS5，禁止静默丢密码）
 * @param {object} proxyConfig
 * @returns {Promise<object|null>}
 */
export async function getClearcoteProxy(proxyConfig) {
    if (!proxyConfig || !proxyConfig.enable) {
        return null;
    }

    const { type, host, port, user, passwd } = proxyConfig;
    const hasAuth = !!(user || passwd);

    if (type === 'socks5' && hasAuth) {
        logger.info('代理器', 'Clearcote: SOCKS5 带认证，通过本地 relay 转换为 HTTP 代理（不会静默丢弃密码）');
        const httpProxyUrl = await getHttpProxy(proxyConfig);
        if (!httpProxyUrl) {
            throw new Error('Clearcote SOCKS5 认证代理 relay 创建失败');
        }
        return { server: httpProxyUrl };
    }

    if (type === 'socks5') {
        return { server: `socks5://${host}:${port}` };
    }

    const server = `http://${host}:${port}`;
    if (hasAuth) {
        return { server, username: user, password: passwd };
    }
    return { server };
}

/**
 * 清理代理资源
 * 关闭由 proxy-chain 创建的本地代理服务器
 */
export async function cleanupProxy() {
    if (proxyState.anonymizedProxies.size === 0) {
        return;
    }
    const urls = [...proxyState.anonymizedProxies];
    proxyState.anonymizedProxies.clear();
    for (const url of urls) {
        try {
            logger.debug('代理器', '正在关闭本地代理桥接...');
            await closeAnonymizedProxy(url, true);
            logger.debug('代理器', '本地代理桥接已关闭');
        } catch (error) {
            logger.error('代理器', `关闭本地代理桥接失败: ${error.message}`);
        }
    }
}

/**
 * 从配置文件读取代理配置
 * @param {object} config - 配置对象
 * @returns {object|null} - 代理配置对象或 null
 */
export function getProxyConfig(config) {
    if (config?.browser?.proxy?.enable) {
        return config.browser.proxy;
    }
    return null;
}

/**
 * 浏览器关闭语义回归测试
 * 运行: node scripts/test-browser-close-semantics.mjs
 *
 * 覆盖"关闭 Camoufox 窗口后是否会自动重启"的语义：
 * - 有头 context close  = 用户手动关窗 → 不再自动重建
 * - 无头 context close  = 崩溃 → 保持自愈重建
 * - 服务 shutdown 中 close → 不重建（防回归）
 * - 手动关闭后请求 → 明确报错且不拉起浏览器
 * - POST /admin/browser/restart 语义（PoolManager.restartBrowser）
 * - /admin/stop 路径调用 cleanup（防孤儿 Firefox 进程）
 */
import path from 'path';
import { pathToFileURL } from 'url';

import {
    isShuttingDown,
    isBrowserUserStopped,
    markBrowserUserStopped,
    resetBrowserStopped,
    shouldAutoRestartOnClose
} from '../src/backend/engine/launcher.js';
import { Worker } from '../src/backend/pool/Worker.js';
import { PoolManager } from '../src/backend/pool/PoolManager.js';

let passed = 0;
let failed = 0;
function assert(name, cond, detail = '') {
    if (cond) {
        passed++;
        console.log(`  ✓ ${name}`);
    } else {
        failed++;
        console.error(`  ✗ ${name}${detail ? ' — ' + detail : ''}`);
    }
}

/** 简易 EventEmitter context/page 替身 */
function makeContext({ headless = false } = {}) {
    const handlers = new Map();
    const ctx = {
        __webaiHeadless: headless,
        isClosed: false,
        on(event, fn) {
            if (!handlers.has(event)) handlers.set(event, []);
            handlers.get(event).push(fn);
        },
        emit(event) {
            for (const fn of handlers.get(event) || []) fn();
        },
        async newPage() { return makePage(); },
        async close() { ctx.isClosed = true; }
    };
    return ctx;
}

function makePage() {
    let closed = false;
    return {
        get closed() { return closed; },
        isClosed() { return closed; },
        async close() { closed = true; },
        on() { },
        authState: null
    };
}

/** 构造一个不依赖真实浏览器的 Worker：直接注入 _initNewBrowser 替身 */
function makeWorker({ headless = false, launchCount = { n: 0 } }) {
    const worker = Object.create(Worker.prototype);
    worker.name = 'test-worker';
    worker.type = 'test';
    worker.instanceName = null;
    worker.engine = 'camoufox';
    worker.userDataDir = '/tmp/webai2api-test-userdata';
    worker.globalConfig = { browser: { engine: 'camoufox' } };
    worker.workerConfig = { name: 'test-worker' };
    worker.mergeTypes = [];
    worker.browser = null;
    worker.page = null;
    worker.busyCount = 0;
    worker.initialized = false;
    worker._isBrowserOwner = true;
    worker._browserOwner = null;
    worker._sharedWorkers = [];
    // 用替身替换真实启动：每次调用都"拉起"一个新 context
    worker._initNewBrowser = async (targetUrl) => {
        launchCount.n++;
        const context = makeContext({ headless });
        const page = makePage();
        worker.browser = context;
        worker.page = page;
        // 复刻真实实现的 close 注册（含本次修复后的分支）
        const browserInstance = context;
        browserInstance.on('close', async () => {
            if (worker.browser !== browserInstance) return;
            if (isShuttingDown()) {
                worker.initialized = false;
                worker.browser = null;
                worker.page = null;
                return;
            }
            if (!shouldAutoRestartOnClose(browserInstance)) {
                markBrowserUserStopped();
                worker.initialized = false;
                worker.browser = null;
                worker.page = null;
                for (const sw of worker._sharedWorkers) {
                    sw.initialized = false;
                    sw.browser = null;
                    sw.page = null;
                }
                return;
            }
            worker.initialized = false;
            worker.browser = null;
            worker.page = null;
            await worker._reinit();
        });
        worker._targetUrl = targetUrl || 'about:blank';
        worker.initialized = true;
    };
    return worker;
}

console.log('\n[1] 有头模式：用户手动关窗不再自动重启');
{
    resetBrowserStopped();
    const launches = { n: 0 };
    const worker = makeWorker({ headless: false, launchCount: launches });
    await worker._initNewBrowser('about:blank');
    assert('初始启动一次', launches.n === 1);

    // 用户关窗 → context close
    worker.browser.emit('close');
    await new Promise(r => setTimeout(r, 30));

    assert('标记为用户手动关闭', isBrowserUserStopped() === true);
    assert('未重新拉起浏览器', launches.n === 1, `launches=${launches.n}`);
    assert('Worker 被标记为未初始化', worker.initialized === false);
    assert('浏览器引用已释放', worker.browser === null && worker.page === null);

    // 关闭后收到请求：明确报错，不再拉起
    const result = await worker._executeAdapter({}, 'test', 'm1', 'p', [], {});
    assert('请求返回明确错误', !!result.error && result.error.includes('已被手动关闭'), result.error);
    assert('请求未拉起浏览器', launches.n === 1, `launches=${launches.n}`);

    // _doReinit 直接跳过
    await worker._doReinit();
    assert('_doReinit 不重建', launches.n === 1, `launches=${launches.n}`);
}

console.log('\n[2] 恢复后可以重新拉起');
{
    const launches = { n: 0 };
    const worker = makeWorker({ headless: false, launchCount: launches });
    assert('重置前处于手动关闭态', isBrowserUserStopped() === true);
    assert('resetBrowserStopped 返回 true', resetBrowserStopped() === true);
    assert('重置后不再处于手动关闭态', isBrowserUserStopped() === false);
    assert('重复 reset 返回 false', resetBrowserStopped() === false);

    await worker._initNewBrowser('about:blank');
    assert('恢复后可重新拉起', launches.n === 1);
    assert('Worker 重新初始化', worker.initialized === true);
}

console.log('\n[3] 无头模式：崩溃仍然自愈（不回归 Docker 场景）');
{
    resetBrowserStopped();
    const launches = { n: 0 };
    const worker = makeWorker({ headless: true, launchCount: launches });
    await worker._initNewBrowser('about:blank');
    assert('初始启动一次', launches.n === 1);

    worker.browser.emit('close');
    await new Promise(r => setTimeout(r, 50));

    assert('未标记为用户手动关闭', isBrowserUserStopped() === false);
    assert('崩溃后自动重建', launches.n === 2, `launches=${launches.n}`);
    assert('Worker 恢复为已初始化', worker.initialized === true);
}

console.log('\n[4] shouldAutoRestartOnClose 判定表');
{
    resetBrowserStopped();
    assert('无头 close 应自愈', shouldAutoRestartOnClose({ __webaiHeadless: true }) === true);
    assert('有头 close 不视为崩溃', shouldAutoRestartOnClose({ __webaiHeadless: false }) === false);
    assert('未知 context 不视为崩溃', shouldAutoRestartOnClose({}) === false);
    markBrowserUserStopped();
    assert('手动关闭后即使无头也不重建', shouldAutoRestartOnClose({ __webaiHeadless: true }) === false);
    resetBrowserStopped();
    assert('reset 后无头恢复自愈', shouldAutoRestartOnClose({ __webaiHeadless: true }) === true);
}

console.log('\n[5] PoolManager.restartBrowser 守卫');
{
    resetBrowserStopped();
    const pm = Object.create(PoolManager.prototype);
    pm.workers = [];
    const notStopped = await pm.restartBrowser();
    assert('未手动关闭时拒绝恢复', notStopped.success === false && /无需恢复/.test(notStopped.message), notStopped.message);

    markBrowserUserStopped();
    const launches = { n: 0 };
    const owner = makeWorker({ headless: false, launchCount: launches });
    owner._sharedWorkers = [];
    pm.workers = [owner];
    const restored = await pm.restartBrowser();
    assert('恢复成功', restored.success === true, restored.message);
    assert('恢复后清除了手动关闭标记', isBrowserUserStopped() === false);
    assert('恢复时拉起了浏览器', launches.n === 1, `launches=${launches.n}`);
    assert('isBrowserStopped 反映状态', pm.isBrowserStopped() === false);

    // 所有者恢复失败时如实报告
    markBrowserUserStopped();
    const failing = Object.create(Worker.prototype);
    failing.name = 'bad-owner';
    failing._isBrowserOwner = true;
    failing._browserOwner = null;
    failing._sharedWorkers = [];
    failing._reinit = async () => { throw new Error('launch failed'); };
    pm.workers = [failing];
    const failedResult = await pm.restartBrowser();
    assert('失败时 success=false', failedResult.success === false);
    assert('失败信息包含 worker 名', /bad-owner/.test(failedResult.message), failedResult.message);
    resetBrowserStopped();
}

console.log('\n[6] 共享浏览器：手动关闭时同步释放共享者');
{
    resetBrowserStopped();
    const launches = { n: 0 };
    const owner = makeWorker({ headless: false, launchCount: launches });
    const shared = makeWorker({ headless: false, launchCount: launches });
    shared._browserOwner = owner;
    owner._sharedWorkers = [shared];
    await owner._initNewBrowser('about:blank');

    owner.browser.emit('close');
    await new Promise(r => setTimeout(r, 30));

    assert('共享者被标记为未初始化', shared.initialized === false);
    assert('共享者引用已释放', shared.browser === null && shared.page === null);
    assert('共享者不触发额外启动', launches.n === 1, `launches=${launches.n}`);
}

console.log('\n[7] /admin/stop 路径会调用 cleanup（源码级断言）');
{
    const fs = await import('fs');
    const routesPath = path.join(process.cwd(), 'src/server/api/admin/routes.js');
    const src = fs.readFileSync(routesPath, 'utf8');
    const stopBlock = src.match(/\/admin\/stop[\s\S]{0,700}?process\.exit\(0\)/);
    assert('存在 /admin/stop 处理块', !!stopBlock);
    assert('stop 前调用 cleanupBrowsers', !!stopBlock && stopBlock[0].includes('cleanupBrowsers()'));
    assert('stop 不再直接裸 exit', !!stopBlock && !/setTimeout\(\(\) => process\.exit\(0\), 1000\)/.test(stopBlock[0]));
    assert('browser/restart 端点已注册', src.includes("pathname === '/browser/restart'"));
}

console.log('\n[8] Worker 关闭处理器使用 shouldAutoRestartOnClose（源码级断言）');
{
    const fs = await import('fs');
    const workerSrc = fs.readFileSync(path.join(process.cwd(), 'src/backend/pool/Worker.js'), 'utf8');
    assert('导入 shouldAutoRestartOnClose', workerSrc.includes('shouldAutoRestartOnClose'));
    assert('close 处理器引用该守卫', /shouldAutoRestartOnClose\(browserInstance\)/.test(workerSrc));
    assert('_executeAdapter 有手动关闭守卫', /isBrowserUserStopped\(\)/.test(workerSrc));
    assert('_doReinit 有手动关闭守卫', /isShuttingDown\(\) \|\| isBrowserUserStopped\(\)/.test(workerSrc));
}

// 注意：cleanup() 会把进程级 lifecycle.stopping/stopped 永久置位（真实场景下
// 收到信号即退出，不存在复位路径），因此 shutdown 相关断言放在最后执行。
console.log('\n[9] 服务 shutdown 中 close 不重建（防回归）');
{
    resetBrowserStopped();
    const launches = { n: 0 };
    const worker = makeWorker({ headless: true, launchCount: launches });
    await worker._initNewBrowser('about:blank');
    assert('初始启动一次', launches.n === 1);

    // 通过 cleanup 置位 shutting down（doCleanup 会尝试 close 全部 context）
    const { cleanup } = await import('../src/backend/engine/launcher.js');
    await cleanup();
    assert('isShuttingDown 为 true', isShuttingDown() === true);

    worker.browser.emit('close');
    await new Promise(r => setTimeout(r, 50));
    assert('shutdown 中不重建', launches.n === 1, `launches=${launches.n}`);
    assert('shutdown 中 restartBrowser 被拒绝', (await Object.create(PoolManager.prototype).restartBrowser()).success === false);
}

console.log(`\n结果: ${passed} passed, ${failed} failed\n`);
process.exit(failed > 0 ? 1 : 0);

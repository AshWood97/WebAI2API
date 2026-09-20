/** Camoufox 浏览器发行版钉选版本（init / 测试共用） */

/** GitHub release tag，去掉 v 前缀 */
export const CAMOUFOX_RELEASE = '152.0.4-beta.30';

/** 写入 camoufox/version.json 的字段 */
export const CAMOUFOX_VERSION_JSON = Object.freeze({
    version: '152.0.4',
    release: 'beta.30'
});

/** 自检告警阈值：低于该 Firefox 主版本视为过旧 */
export const CAMOUFOX_MIN_RECOMMENDED_MAJOR = 146;

<script setup>
import { onMounted, reactive } from 'vue';
import { useSettingsStore } from '@/stores/settings';

const settingsStore = useSettingsStore();

// 表单数据
const formData = reactive({
    path: '',
    engine: 'camoufox',
    headless: false,
    fission: true,
    humanizeCursor: 'camou', // false | true | 'camou'
    ffVersion: null,
    camoufox: {
        mainWorldEval: false,
        enableCache: false,
        disableInstantAnimations: false,
        humanizeMaxTime: 1.5,
        blockWebRtc: true,
        geoip: true,
        locale: '',
        certificatePathsText: ''
    },
    clearcote: {
        path: '',
        platform: 'auto',
        brand: 'Chrome',
        fingerprintProfile: '',
        timezone: '',
        acceptLanguage: '',
        geoip: true,
        humanize: true,
        webrtcIp: '',
        sandbox: true,
        allowDetectedLicense: false,
        argsText: ''
    },
    // CSS 性能优化
    cssAnimation: false,
    cssFilter: false,
    cssFont: false,
    // 全局代理
    proxyEnable: false,
    proxyType: 'http',
    proxyHost: '127.0.0.1',
    proxyPort: 7890,
    proxyAuth: false,
    proxyUser: '',
    proxyPasswd: ''
});

const parsePathList = (text) => (text || '')
    .split('\n')
    .map(s => s.trim())
    .filter(Boolean);

onMounted(async () => {
    await settingsStore.fetchBrowserConfig();
    const cfg = settingsStore.browserConfig || {};
    formData.path = cfg.path || '';
    formData.engine = cfg.engine || 'camoufox';
    formData.headless = cfg.headless || false;
    formData.fission = cfg.fission !== false;
    formData.humanizeCursor = cfg.humanizeCursor ?? 'camou';
    formData.ffVersion = cfg.ffVersion ?? null;

    const c = cfg.camoufox || {};
    formData.camoufox.mainWorldEval = c.mainWorldEval ?? false;
    formData.camoufox.enableCache = c.enableCache ?? false;
    formData.camoufox.disableInstantAnimations = c.disableInstantAnimations ?? false;
    formData.camoufox.humanizeMaxTime = c.humanizeMaxTime ?? 1.5;
    formData.camoufox.blockWebRtc = c.blockWebRtc !== false;
    formData.camoufox.geoip = c.geoip !== false;
    formData.camoufox.locale = c.locale || '';
    formData.camoufox.certificatePathsText = (c.certificatePaths || []).join('\n');

    const cc = cfg.clearcote || {};
    formData.clearcote.path = cc.path || '';
    formData.clearcote.platform = cc.platform || 'auto';
    formData.clearcote.brand = cc.brand || 'Chrome';
    formData.clearcote.fingerprintProfile = cc.fingerprintProfile || '';
    formData.clearcote.timezone = cc.timezone || '';
    formData.clearcote.acceptLanguage = cc.acceptLanguage || '';
    formData.clearcote.geoip = cc.geoip !== false;
    formData.clearcote.humanize = cc.humanize !== false;
    formData.clearcote.webrtcIp = cc.webrtcIp || '';
    formData.clearcote.sandbox = cc.sandbox !== false;
    formData.clearcote.allowDetectedLicense = cc.allowDetectedLicense === true;
    formData.clearcote.argsText = (cc.args || []).join('\n');

    if (cfg.cssInject) {
        formData.cssAnimation = cfg.cssInject.animation || false;
        formData.cssFilter = cfg.cssInject.filter || false;
        formData.cssFont = cfg.cssInject.font || false;
    }

    if (cfg.proxy) {
        formData.proxyEnable = cfg.proxy.enable || false;
        formData.proxyType = cfg.proxy.type || 'http';
        formData.proxyHost = cfg.proxy.host || '';
        formData.proxyPort = cfg.proxy.port || 7890;
        formData.proxyAuth = cfg.proxy.auth || false;
        formData.proxyUser = cfg.proxy.username || '';
        formData.proxyPasswd = cfg.proxy.password || '';
    }
});

// 保存设置
const handleSave = async () => {
    const certificatePaths = parsePathList(formData.camoufox.certificatePathsText);
    const clearcoteArgs = parsePathList(formData.clearcote.argsText);
    const config = {
        path: formData.path,
        engine: formData.engine,
        headless: formData.headless,
        cssInject: {
            animation: formData.cssAnimation,
            filter: formData.cssFilter,
            font: formData.cssFont
        },
        fission: formData.fission,
        humanizeCursor: formData.humanizeCursor,
        ffVersion: formData.ffVersion === '' || formData.ffVersion === null
            ? null
            : Number(formData.ffVersion),
        camoufox: {
            mainWorldEval: formData.camoufox.mainWorldEval,
            enableCache: formData.camoufox.enableCache,
            disableInstantAnimations: formData.camoufox.disableInstantAnimations,
            humanizeMaxTime: Number(formData.camoufox.humanizeMaxTime) || 1.5,
            blockWebRtc: formData.camoufox.blockWebRtc,
            geoip: formData.camoufox.geoip,
            locale: formData.camoufox.locale || null,
            certificatePaths
            // 不发送 certificates：避免 WebUI 保存时清空 YAML 中的原始 PEM；
            // 且 camoufox-js@0.12 尚未消费 certificates/certificatePaths（仅预留）
        },
        clearcote: {
            path: formData.clearcote.path || '',
            platform: formData.clearcote.platform || 'auto',
            brand: formData.clearcote.brand || 'Chrome',
            fingerprintProfile: formData.clearcote.fingerprintProfile || '',
            timezone: formData.clearcote.timezone || '',
            acceptLanguage: formData.clearcote.acceptLanguage || '',
            geoip: formData.clearcote.geoip !== false,
            humanize: formData.clearcote.humanize !== false,
            webrtcIp: formData.clearcote.webrtcIp || '',
            sandbox: formData.clearcote.sandbox !== false,
            allowDetectedLicense: formData.clearcote.allowDetectedLicense === true,
            args: clearcoteArgs
        },
        proxy: {
            enable: formData.proxyEnable,
            type: formData.proxyType,
            host: formData.proxyHost,
            port: formData.proxyPort,
            auth: formData.proxyAuth,
            username: formData.proxyUser,
            password: formData.proxyPasswd
        }
    };
    await settingsStore.saveBrowserConfig(config);
};
</script>

<template>
    <div class="browser-settings">
        <a-card title="基础" :bordered="false" class="sec">
            <a-form layout="vertical">
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="全局浏览器基座 engine">
                            <a-select v-model:value="formData.engine">
                                <a-select-option value="camoufox">camoufox（默认 / Firefox）</a-select-option>
                                <a-select-option value="clearcote">clearcote（Chromium；Windows/Linux x64）</a-select-option>
                            </a-select>
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="Camoufox 可执行文件路径（留空使用默认）">
                            <a-input v-model:value="formData.path" placeholder="/path/to/camoufox" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="无头模式">
                            <a-switch v-model:checked="formData.headless" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="站点隔离 (fission)">
                            <a-switch v-model:checked="formData.fission" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="拟人鼠标轨迹">
                            <a-select v-model:value="formData.humanizeCursor">
                                <a-select-option :value="'camou'">Camoufox 内核（推荐）</a-select-option>
                                <a-select-option :value="true">ghost-cursor</a-select-option>
                                <a-select-option :value="false">关闭</a-select-option>
                            </a-select>
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="Firefox 主版本 spoof（空=跟随已安装）">
                            <a-input-number v-model:value="formData.ffVersion" :min="100" :max="200" style="width:100%"
                                placeholder="例如 152" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="Locale（空=跟指纹）">
                            <a-input v-model:value="formData.camoufox.locale" placeholder="zh-CN / en-US" />
                        </a-form-item>
                    </a-col>
                </a-row>
            </a-form>
        </a-card>

        <a-card title="Camoufox FF152 能力" :bordered="false" class="sec">
            <a-form layout="vertical">
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="main_world_eval (mw:)">
                            <a-switch v-model:checked="formData.camoufox.mainWorldEval" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="启用页面缓存">
                            <a-switch v-model:checked="formData.camoufox.enableCache" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="禁用瞬时动画">
                            <a-switch v-model:checked="formData.camoufox.disableInstantAnimations" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="拟人轨迹最大时长（秒）">
                            <a-input-number v-model:value="formData.camoufox.humanizeMaxTime" :min="0.3" :max="5"
                                :step="0.1" style="width:100%" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="阻断 WebRTC">
                            <a-switch v-model:checked="formData.camoufox.blockWebRtc" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="GeoIP 伪造">
                            <a-switch v-model:checked="formData.camoufox.geoip" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-form-item label="自定义 CA 证书路径（每行一个）· 预留：camoufox-js 0.12 暂未消费">
                    <a-textarea v-model:value="formData.camoufox.certificatePathsText" :rows="3"
                        placeholder="/etc/ssl/corp-ca.pem" />
                </a-form-item>
            </a-form>
        </a-card>

        <a-card title="Clearcote（Chromium 基座）" :bordered="false" class="sec">
            <a-alert type="info" show-icon style="margin-bottom:12px"
                description="Clearcote 官方支持 Windows x64 / Linux x64；macOS 仍在 roadmap。profile 使用 data/clearcoteUserData*，与 Camoufox 目录隔离。切换 engine 需重启服务。免费 GitHub 构建默认同时仅 1 个 Clearcote 浏览器。容器内如需 --no-sandbox，请使用 sandbox 开关（args 禁止手写 --no-sandbox）。" />
            <a-form layout="vertical">
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="Clearcote 可执行文件（空=SDK 校验缓存）">
                            <a-input v-model:value="formData.clearcote.path" placeholder="留空使用 SDK 解析" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="指纹 platform">
                            <a-select v-model:value="formData.clearcote.platform">
                                <a-select-option value="auto">auto（按宿主机）</a-select-option>
                                <a-select-option value="windows">windows</a-select-option>
                                <a-select-option value="linux">linux</a-select-option>
                            </a-select>
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="Brand">
                            <a-select v-model:value="formData.clearcote.brand">
                                <a-select-option value="Chrome">Chrome</a-select-option>
                                <a-select-option value="Edge">Edge</a-select-option>
                                <a-select-option value="Opera">Opera</a-select-option>
                                <a-select-option value="Vivaldi">Vivaldi</a-select-option>
                            </a-select>
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="timezone（空=不强制）">
                            <a-input v-model:value="formData.clearcote.timezone" placeholder="Asia/Shanghai" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="acceptLanguage（空=不强制）">
                            <a-input v-model:value="formData.clearcote.acceptLanguage" placeholder="zh-CN,zh" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="webrtcIp（空=不强制）">
                            <a-input v-model:value="formData.clearcote.webrtcIp" placeholder="203.0.113.10" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="fingerprintProfile 文件路径（空=用持久 seed；配置后与 seed 互斥）">
                            <a-input v-model:value="formData.clearcote.fingerprintProfile" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="GeoIP 对齐">
                            <a-switch v-model:checked="formData.clearcote.geoip" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="原生 humanize">
                            <a-switch v-model:checked="formData.clearcote.humanize" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="sandbox（关闭降安全）">
                            <a-switch v-model:checked="formData.clearcote.sandbox" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="允许检测到的 license">
                            <a-switch v-model:checked="formData.clearcote.allowDetectedLicense" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-alert type="warning" show-icon style="margin-bottom: 12px"
                    description="sandbox 关闭后仅通过开关注入 --no-sandbox，请勿在 args 手写。免费模式默认拒绝自动 PRO/license；fingerprintProfile 与持久 seed 互斥。args 请每行一个参数，逗号属于参数本身（如 --disable-features=A,B）。" />
                <a-form-item label="Clearcote/Chromium 额外 args（每行一个；不继承 Firefox 参数）">
                    <a-textarea v-model:value="formData.clearcote.argsText" :rows="3"
                        placeholder="--disable-gpu" />
                </a-form-item>
            </a-form>
        </a-card>

        <a-card title="CSS 性能注入" :bordered="false" class="sec">
            <a-row :gutter="16">
                <a-col :xs="24" :sm="12" :md="8">
                    <a-form-item label="禁用动画">
                        <a-switch v-model:checked="formData.cssAnimation" />
                    </a-form-item>
                </a-col>
                <a-col :xs="24" :sm="12" :md="8">
                    <a-form-item label="禁用滤镜/阴影">
                        <a-switch v-model:checked="formData.cssFilter" />
                    </a-form-item>
                </a-col>
                <a-col :xs="24" :sm="12" :md="8">
                    <a-form-item label="字体极速渲染（高指纹风险）">
                        <a-switch v-model:checked="formData.cssFont" />
                    </a-form-item>
                </a-col>
            </a-row>
        </a-card>

        <a-card title="全局代理" :bordered="false" class="sec">
            <a-form layout="vertical">
                <a-row :gutter="16">
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="启用">
                            <a-switch v-model:checked="formData.proxyEnable" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="类型">
                            <a-select v-model:value="formData.proxyType">
                                <a-select-option value="http">http</a-select-option>
                                <a-select-option value="socks5">socks5</a-select-option>
                            </a-select>
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="8">
                        <a-form-item label="主机">
                            <a-input v-model:value="formData.proxyHost" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="端口">
                            <a-input-number v-model:value="formData.proxyPort" :min="1" :max="65535" style="width:100%" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="4">
                        <a-form-item label="需要认证">
                            <a-switch v-model:checked="formData.proxyAuth" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row v-if="formData.proxyAuth" :gutter="16">
                    <a-col :xs="24" :sm="12" :md="12">
                        <a-form-item label="用户名">
                            <a-input v-model:value="formData.proxyUser" />
                        </a-form-item>
                    </a-col>
                    <a-col :xs="24" :sm="12" :md="12">
                        <a-form-item label="密码">
                            <a-input-password v-model:value="formData.proxyPasswd" />
                        </a-form-item>
                    </a-col>
                </a-row>
            </a-form>
        </a-card>

        <div class="actions">
            <a-button type="primary" @click="handleSave">保存配置</a-button>
        </div>
    </div>
</template>

<style scoped>
.browser-settings {
    max-width: 960px;
}

.sec {
    margin-bottom: 12px;
}

.actions {
    margin-top: 8px;
}
</style>

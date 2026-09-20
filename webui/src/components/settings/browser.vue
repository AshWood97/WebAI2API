<script setup>
import { onMounted, reactive } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { message } from 'ant-design-vue';

const settingsStore = useSettingsStore();

// 表单数据
const formData = reactive({
    path: '',
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
    .split(/\n|,/)
    .map(s => s.trim())
    .filter(Boolean);

onMounted(async () => {
    await settingsStore.fetchBrowserConfig();
    const cfg = settingsStore.browserConfig || {};
    formData.path = cfg.path || '';
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
    const config = {
        path: formData.path,
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
            certificatePaths,
            certificates: []
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
    message.success('浏览器配置已保存（部分项需重启服务生效）');
};
</script>

<template>
    <div class="browser-settings">
        <a-card title="基础" :bordered="false" class="sec">
            <a-form layout="vertical">
                <a-row :gutter="16">
                    <a-col :span="12">
                        <a-form-item label="浏览器可执行文件路径（留空使用默认）">
                            <a-input v-model:value="formData.path" placeholder="/path/to/camoufox" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="6">
                        <a-form-item label="无头模式">
                            <a-switch v-model:checked="formData.headless" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="6">
                        <a-form-item label="站点隔离 (fission)">
                            <a-switch v-model:checked="formData.fission" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row :gutter="16">
                    <a-col :span="8">
                        <a-form-item label="拟人鼠标轨迹">
                            <a-select v-model:value="formData.humanizeCursor">
                                <a-select-option :value="'camou'">Camoufox 内核（推荐）</a-select-option>
                                <a-select-option :value="true">ghost-cursor</a-select-option>
                                <a-select-option :value="false">关闭</a-select-option>
                            </a-select>
                        </a-form-item>
                    </a-col>
                    <a-col :span="8">
                        <a-form-item label="Firefox 主版本 spoof（空=跟随已安装）">
                            <a-input-number v-model:value="formData.ffVersion" :min="100" :max="200" style="width:100%"
                                placeholder="例如 152" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="8">
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
                    <a-col :span="8">
                        <a-form-item label="main_world_eval (mw:)">
                            <a-switch v-model:checked="formData.camoufox.mainWorldEval" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="8">
                        <a-form-item label="启用页面缓存">
                            <a-switch v-model:checked="formData.camoufox.enableCache" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="8">
                        <a-form-item label="禁用瞬时动画">
                            <a-switch v-model:checked="formData.camoufox.disableInstantAnimations" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row :gutter="16">
                    <a-col :span="8">
                        <a-form-item label="拟人轨迹最大时长（秒）">
                            <a-input-number v-model:value="formData.camoufox.humanizeMaxTime" :min="0.3" :max="5"
                                :step="0.1" style="width:100%" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="8">
                        <a-form-item label="阻断 WebRTC">
                            <a-switch v-model:checked="formData.camoufox.blockWebRtc" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="8">
                        <a-form-item label="GeoIP 伪造">
                            <a-switch v-model:checked="formData.camoufox.geoip" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-form-item label="自定义 CA 证书路径（每行一个，企业 MITM 代理用）">
                    <a-textarea v-model:value="formData.camoufox.certificatePathsText" :rows="3"
                        placeholder="/etc/ssl/corp-ca.pem" />
                </a-form-item>
            </a-form>
        </a-card>

        <a-card title="CSS 性能注入" :bordered="false" class="sec">
            <a-row :gutter="16">
                <a-col :span="8">
                    <a-form-item label="禁用动画">
                        <a-switch v-model:checked="formData.cssAnimation" />
                    </a-form-item>
                </a-col>
                <a-col :span="8">
                    <a-form-item label="禁用滤镜/阴影">
                        <a-switch v-model:checked="formData.cssFilter" />
                    </a-form-item>
                </a-col>
                <a-col :span="8">
                    <a-form-item label="字体极速渲染（高指纹风险）">
                        <a-switch v-model:checked="formData.cssFont" />
                    </a-form-item>
                </a-col>
            </a-row>
        </a-card>

        <a-card title="全局代理" :bordered="false" class="sec">
            <a-form layout="vertical">
                <a-row :gutter="16">
                    <a-col :span="4">
                        <a-form-item label="启用">
                            <a-switch v-model:checked="formData.proxyEnable" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="4">
                        <a-form-item label="类型">
                            <a-select v-model:value="formData.proxyType">
                                <a-select-option value="http">http</a-select-option>
                                <a-select-option value="socks5">socks5</a-select-option>
                            </a-select>
                        </a-form-item>
                    </a-col>
                    <a-col :span="8">
                        <a-form-item label="主机">
                            <a-input v-model:value="formData.proxyHost" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="4">
                        <a-form-item label="端口">
                            <a-input-number v-model:value="formData.proxyPort" :min="1" :max="65535" style="width:100%" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="4">
                        <a-form-item label="需要认证">
                            <a-switch v-model:checked="formData.proxyAuth" />
                        </a-form-item>
                    </a-col>
                </a-row>
                <a-row v-if="formData.proxyAuth" :gutter="16">
                    <a-col :span="12">
                        <a-form-item label="用户名">
                            <a-input v-model:value="formData.proxyUser" />
                        </a-form-item>
                    </a-col>
                    <a-col :span="12">
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

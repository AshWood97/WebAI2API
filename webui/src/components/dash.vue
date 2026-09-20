<script setup>
import { ref, onMounted, onUnmounted } from 'vue';
import { useSystemStore } from '@/stores/system';
import { useSettingsStore } from '@/stores/settings';
import { message } from 'ant-design-vue';
import {
    DesktopOutlined,
    PieChartOutlined,
    ChromeOutlined,
    FieldTimeOutlined,
    LineChartOutlined,
    SyncOutlined,
    ExclamationCircleOutlined,
    CheckCircleOutlined,
    CloseCircleOutlined,
    ApiOutlined,
    CopyOutlined,
    BookOutlined
} from '@ant-design/icons-vue';

const systemStore = useSystemStore();
const queueData = ref([]);
const timer = ref(null);
const queueStats = ref({ processing: 0, waiting: 0, total: 0 });
const providers = ref([]);
const runtimeInfo = ref(null);
const baseUrl = ref('');

const refreshData = async () => {
    const settingsStore = useSettingsStore(); // 获取store
    baseUrl.value = `${location.origin}/v1`;
    try {
        const res = await fetch('/admin/queue', { headers: settingsStore.getHeaders() });
        if (res.ok) {
            const data = await res.json();

            // 更新统计信息
            queueStats.value = {
                processing: data.processing || 0,
                waiting: data.waiting || 0,
                total: data.total || 0
            };

            const processing = (data.processingTasks || []).map(t => ({ ...t, status: 'processing' }));
            const waiting = (data.waitingTasks || []).map(t => ({ ...t, status: 'waiting' }));
            queueData.value = [...processing, ...waiting];
        }
    } catch (e) {
        console.error('Fetch queue failed', e);
    }

    // Provider 健康卡片（借鉴 WebModel）
    try {
        const settingsStore = useSettingsStore();
        const res = await fetch('/v1/providers', { headers: settingsStore.getHeaders() });
        if (res.ok) {
            const data = await res.json();
            providers.value = data.data || [];
        }
    } catch (e) { /* providers optional */ }

    try {
        const settingsStore = useSettingsStore();
        const res = await fetch('/v1/runtime/status', { headers: settingsStore.getHeaders() });
        if (res.ok) {
            runtimeInfo.value = await res.json();
        }
    } catch (e) { /* runtime optional */ }

    await Promise.all([
        systemStore.fetchStatus(),
        systemStore.fetchStats()
    ]);
};

const copyText = async (text, tip = '已复制') => {
    try {
        await navigator.clipboard.writeText(text);
        message.success(tip);
    } catch {
        message.error('复制失败');
    }
};

const copyBaseUrl = () => copyText(baseUrl.value, 'Base URL 已复制');
const copySampleCurl = () => {
    const token = useSettingsStore().token || 'YOUR_API_KEY';
    const curl = `curl -sS ${baseUrl.value}/chat/completions -H 'Authorization: Bearer ${token}' -H 'Content-Type: application/json' -d '{"model":"your-model","messages":[{"role":"user","content":"hi"}],"stream":true}'`;
    copyText(curl, 'curl 示例已复制');
};

const providerStatusColor = (status) => {
    if (status === 'active') return 'green';
    if (status === 'registered') return 'blue';
    return 'default';
};

const formatUptime = (seconds) => {
    const d = Math.floor(seconds / (3600 * 24));
    const h = Math.floor((seconds % (3600 * 24)) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    if (d > 0) return `${d}天 ${h}小时 ${m}分`;
    if (h > 0) return `${h}小时 ${m}分`;
    return `${m}分`;
};

const formatMemory = (mb) => {
    if (!mb || mb === 0) return '0 MB';
    if (mb > 1024) {
        return parseFloat((mb / 1024).toFixed(2)) + ' GB';
    }
    return parseFloat(Number(mb).toFixed(2)) + ' MB';
};

const getLoadColor = (usage) => {
    if (usage < 50) return '#52c41a'; // 绿色
    if (usage < 80) return '#faad14'; // 橙色
    return '#f5222d'; // 红色
};

// 状态映射
const getStatusConfig = (status) => {
    const map = {
        'normal': { color: 'green', text: '正常模式 (Normal)' },
        'headless': { color: 'blue', text: '无头模式 (Headless)' },
        'xvfb': { color: 'purple', text: '虚拟显示 (Xvfb)' }
    };
    return map[status] || { color: 'red', text: '未运行' };
};

onMounted(() => {
    refreshData();
    timer.value = setInterval(refreshData, 5000); // 每5秒轮询
});

onUnmounted(() => {
    if (timer.value) clearInterval(timer.value);
});
</script>

<template>
    <a-layout style="width: 100%; background: transparent;">
        <!-- 安全模式告警横幅 -->
        <a-alert v-if="systemStore.safeMode?.enabled" type="error" show-icon style="margin-bottom: 16px;" closable>
            <template #message>
                <span style="font-weight: 600;">⚠️ 安全模式</span>
            </template>
            <template #description>
                <div>
                    <p style="margin-bottom: 8px;">
                        服务因初始化失败进入安全模式，OpenAI API 不可用。
                    </p>
                    <p style="margin-bottom: 8px; color: #cf1322;">
                        <b>原因：</b>{{ systemStore.safeMode.reason }}
                    </p>
                    <p style="margin: 0;">
                        请前往「系统设置」修改正确的配置后重启服务。
                    </p>
                </div>
            </template>
        </a-alert>

        <!-- 响应式布局：手机竖向，电脑横向 -->
        <a-row :gutter="[16, 16]" style="margin-bottom: 24px">
            <!-- 系统信息卡片 -->
            <a-col :xs="24" :md="12">
                <a-card title="系统状态" :bordered="false" style="height: 100%">
                    <a-space direction="vertical" style="width: 100%" size="middle">
                        <div style="display: flex; justify-content: space-between;">
                            <span>
                                <DesktopOutlined /> 系统版本:
                            </span>
                            <b>{{ systemStore.systemVersion }}</b>
                        </div>
                        <div style="display: flex; justify-content: space-between;">
                            <span>
                                <FieldTimeOutlined /> 运行时间:
                            </span>
                            <b>{{ formatUptime(systemStore.uptime) }}</b>
                        </div>
                        <div style="display: flex; justify-content: space-between;">
                            <span>
                                <ChromeOutlined /> 状态:
                            </span>
                            <a-tag :color="getStatusConfig(systemStore.status).color">
                                {{ getStatusConfig(systemStore.status).text }}
                            </a-tag>
                        </div>

                        <div>
                            <div style="display: flex; justify-content: space-between; margin-bottom: 4px;">
                                <span>
                                    <LineChartOutlined /> CPU 使用率:
                                </span>
                                <span>{{ systemStore.cpuUsage }}%</span>
                            </div>
                            <a-progress :percent="systemStore.cpuUsage"
                                :stroke-color="getLoadColor(systemStore.cpuUsage)" :show-info="false" />
                        </div>

                        <div>
                            <div style="display: flex; justify-content: space-between; margin-bottom: 4px;">
                                <span>
                                    <PieChartOutlined /> 内存使用:
                                </span>
                                <span>{{ formatMemory(systemStore.memoryUsage.used) }} / {{
                                    formatMemory(systemStore.memoryUsage.total) }}</span>
                            </div>
                            <a-progress
                                :percent="Math.round((systemStore.memoryUsage.used / systemStore.memoryUsage.total) * 100) || 0"
                                :stroke-color="getLoadColor((systemStore.memoryUsage.used / systemStore.memoryUsage.total) * 100)"
                                :show-info="false" />
                        </div>
                    </a-space>
                </a-card>
            </a-col>

            <!-- 统计数据卡片 -->
            <a-col :xs="24" :md="12">
                <a-card title="业务统计" :bordered="false" style="height: 100%">
                    <a-row :gutter="16" style="margin-bottom: 24px">
                        <a-col :span="12">
                            <a-statistic title="窗口数量" :value="systemStore.stats.workers || 0">
                                <template #suffix>
                                    <span style="font-size: 14px; color: #8c8c8c;">个</span>
                                </template>
                            </a-statistic>
                        </a-col>
                        <a-col :span="12">
                            <a-statistic title="实例数量" :value="systemStore.stats.instances || 0">
                                <template #suffix>
                                    <span style=" font-size: 14px; color: #8c8c8c;">个</span>
                                </template>
                            </a-statistic>
                        </a-col>
                    </a-row>
                    <a-row :gutter="16">
                        <a-col :span="12">
                            <a-statistic title="正在进行" :value="queueStats.processing">
                                <template #suffix>
                                    <span style="font-size: 14px; color: #8c8c8c;">/ {{ queueStats.total }}</span>
                                </template>
                            </a-statistic>
                        </a-col>
                        <a-col :span="12">
                            <a-statistic title="等待排队" :value="queueStats.waiting">
                                <template #suffix>
                                    <span style="font-size: 14px; color: #8c8c8c;">/ {{ queueStats.total }}</span>
                                </template>
                            </a-statistic>
                        </a-col>
                    </a-row>
                    <a-row :gutter="16" style="margin-top: 16px">
                        <a-col :span="12">
                            <a-statistic title="今日成功" :value="systemStore.stats.success || 0">
                                <template #prefix>
                                    <CheckCircleOutlined style="color: #52c41a" />
                                </template>
                            </a-statistic>
                        </a-col>
                        <a-col :span="12">
                            <a-statistic title="今日失败" :value="systemStore.stats.failed || 0">
                                <template #prefix>
                                    <CloseCircleOutlined style="color: #ff4d4f" />
                                </template>
                            </a-statistic>
                        </a-col>
                    </a-row>
                </a-card>
            </a-col>
        </a-row>

        <!-- 接入与文档 -->
        <a-card title="API 接入" :bordered="false" style="margin-bottom: 16px">
            <a-space direction="vertical" style="width: 100%" size="small">
                <div style="display: flex; flex-wrap: wrap; gap: 8px; align-items: center;">
                    <a-input :value="baseUrl" readonly style="max-width: 420px; font-family: monospace;" />
                    <a-button size="small" @click="copyBaseUrl">
                        <CopyOutlined /> 复制 Base URL
                    </a-button>
                    <a-button size="small" @click="copySampleCurl">
                        <CopyOutlined /> 复制 curl
                    </a-button>
                    <a-button size="small" type="link" href="/docs" target="_blank">
                        <BookOutlined /> API 文档
                    </a-button>
                </div>
                <div v-if="runtimeInfo?.camoufox" style="font-size: 12px; color: #8c8c8c;">
                    Camoufox 内核：{{ runtimeInfo.camoufox.full || runtimeInfo.camoufox.version }}
                    · 状态 {{ runtimeInfo.status }} · 模型 {{ runtimeInfo.models?.count ?? 0 }}
                </div>
            </a-space>
        </a-card>

        <!-- Provider 健康卡片 -->
        <a-card v-if="providers.length" title="Provider 状态" :bordered="false" style="margin-bottom: 16px">
            <a-row :gutter="[12, 12]">
                <a-col v-for="p in providers" :key="p.id" :xs="24" :sm="12" :md="8" :lg="6">
                    <div style="padding: 12px; box-shadow: 0 0 0 1px #f0f0f0; border-radius: 8px;">
                        <div style="display: flex; justify-content: space-between; align-items: center;">
                            <b><ApiOutlined /> {{ p.name || p.id }}</b>
                            <a-tag :color="providerStatusColor(p.status)">{{ p.status }}</a-tag>
                        </div>
                        <div style="margin-top: 8px; font-size: 12px; color: #8c8c8c;">
                            模型 {{ p.modelCount || 0 }} · 文本 {{ p.textCount || 0 }} · 图像 {{ p.imageCount || 0 }}
                        </div>
                        <div style="margin-top: 4px; font-size: 12px; color: #8c8c8c;">
                            Worker {{ p.runningWorkers || 0 }}
                            <span v-for="w in (p.workers || []).slice(0, 3)" :key="w.name">
                                · {{ w.name }}{{ w.busy ? '(忙)' : '' }}
                            </span>
                        </div>
                    </div>
                </a-col>
            </a-row>
        </a-card>

        <!-- 任务队列列表 -->
        <a-card title="任务队列实时监控" :bordered="false" style="width: 100%" :bodyStyle="{ padding: '0 24px' }">
            <template #extra>
                <div style="color: #8c8c8c; font-size: 12px;">
                    <SyncOutlined :spin="true" style="margin-right: 4px" /> 实时刷新中
                </div>
            </template>
            <a-list item-layout="horizontal" :data-source="queueData">
                <template #renderItem="{ item }">
                    <a-list-item>
                        <a-list-item-meta :description="`ID: ${item.id}`">
                            <template #title>
                                <span style="font-weight: 500; margin-right: 8px;">{{ item.model }}</span>
                                <a-tag v-if="item.worker" color="blue">{{ item.worker }}</a-tag>
                            </template>
                        </a-list-item-meta>

                        <div>
                            <a-tag v-if="item.status === 'processing'" color="processing">
                                <template #icon>
                                    <SyncOutlined :spin="true" />
                                </template>
                                进行中
                            </a-tag>
                            <a-tag v-else-if="item.status === 'waiting'" color="warning">
                                <template #icon>
                                    <ExclamationCircleOutlined />
                                </template>
                                等待中
                            </a-tag>
                            <a-tag v-else-if="item.status === 'success'" color="success">
                                <template #icon>
                                    <CheckCircleOutlined />
                                </template>
                                已完成
                            </a-tag>
                        </div>
                    </a-list-item>
                </template>
                <div v-if="queueData.length === 0" style="text-align: center; padding: 24px; color: #8c8c8c;">
                    暂无任务
                </div>
            </a-list>
        </a-card>
    </a-layout>
</template>
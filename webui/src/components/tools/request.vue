<script setup>
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue';
import { useRouter } from 'vue-router';
import { useSettingsStore } from '@/stores/settings';
import {
    buildRequestAccountOptions,
    buildRequestProviderOptions,
    countRequestProviderAccounts,
    nativeProviderFor,
    providerVisual,
} from '@/lib/request-provider-options';
import {
    ReloadOutlined,
    DeleteOutlined,
    DownloadOutlined,
    EyeOutlined,
    CheckCircleOutlined,
    CloseCircleOutlined,
    ClockCircleOutlined,
    PictureOutlined,
    PlayCircleOutlined,
    FileTextOutlined,
    RocketOutlined,
    RedoOutlined,
    InboxOutlined,
    LoadingOutlined,
    CopyOutlined,
    AppstoreOutlined,
    UserOutlined,
    AudioOutlined,
    UpOutlined,
    DownOutlined
} from '@ant-design/icons-vue';
import { message, Modal } from 'ant-design-vue';

const settingsStore = useSettingsStore();
const router = useRouter();

// 数据状态
const loading = ref(false);
const records = ref([]);
const total = ref(0);
const page = ref(1);
const pageSize = ref(50);

// 筛选状态
const dateRange = ref([]);
const statusFilter = ref('all');
const modelFilter = ref('');
const searchText = ref('');
const modelOptions = ref([]);

// 多选状态
const selectedRowKeys = ref([]);
const selectedRows = ref([]);

// 统计摘要
const stats = ref({ total: 0, success: 0, failed: 0, avgDuration: 0 });

// 详情抽屉
const drawerVisible = ref(false);
const currentRecord = ref(null);
const detailLoading = ref(false);

// 快速预览弹窗
const previewModalVisible = ref(false);
const previewContent = ref('');
const previewMediaType = ref('text'); // text, image, video
const previewMediaUrl = ref('');
const previewTitle = ref('快速预览');

// 媒体数据缓存 (blob URLs)
const mediaCache = ref({});

// 发送请求相关
const sendModelList = ref([]);
const nativeProviders = ref([]);
const nativeProviderAccounts = ref({});
const sendProvider = ref('doubao');
const sendAccount = ref('');
const sendModel = ref('');
const sendPrompt = ref('');
const sendImageList = ref([]);
const sendAudioFile = ref(null);
const sendStreamMode = ref(false);
const sendReasoningMode = ref(true);
const sending = ref(false);

const modelProviderId = (model) => model?.provider || model?.owned_by || 'other';
const modelAccountId = (model) => model?.account_id || model?.owned_by || 'default';
const isScopedModel = (model) => Boolean(model?.adapter && model.id?.startsWith(`${model.adapter}/`));

const sendProviderOptions = computed(() => {
    return buildRequestProviderOptions(sendModelList.value, nativeProviders.value);
});

const providerModels = computed(() => sendModelList.value.filter(model => (
    isScopedModel(model) && modelProviderId(model) === sendProvider.value
)));

const sendAccountOptions = computed(() => {
    return buildRequestAccountOptions(
        providerModels.value,
        sendProvider.value,
        nativeProviderAccounts.value[sendProvider.value] || []
    );
});

const accountModels = computed(() => providerModels.value.filter(model => modelAccountId(model) === sendAccount.value));
const selectedNativeProvider = computed(() => nativeProviderFor(nativeProviders.value, sendProvider.value));
const selectedProviderName = computed(() => selectedNativeProvider.value?.name
    || sendProviderOptions.value.find(provider => provider.id === sendProvider.value)?.name
    || sendProvider.value);
const selectedNativeProviderNeedsAccount = computed(() => Boolean(selectedNativeProvider.value) && sendAccountOptions.value.length === 0);
const selectedNativeProviderPending = computed(() => selectedNativeProvider.value?.protocol_ready === false);
const platformExpanded = ref(false);
const platformGrid = ref(null);
const hiddenPlatformCount = ref(0);
const failedPlatformIcons = ref(new Set());
const failPlatformIcon = (id) => {
    const next = new Set(failedPlatformIcons.value);
    next.add(id);
    failedPlatformIcons.value = next;
};
const platformTiles = computed(() => sendProviderOptions.value.map(provider => {
    const visual = providerVisual(provider.id, provider.name);
    const native = nativeProviderFor(nativeProviders.value, provider.id);
    return {
        id: provider.id,
        name: provider.name,
        mark: visual.mark,
        color: visual.color,
        short: visual.short,
        icon: failedPlatformIcons.value.has(provider.id) ? '' : visual.icon,
        count: countRequestProviderAccounts(sendModelList.value, provider.id, nativeProviderAccounts.value[provider.id] || []),
        pending: native?.protocol_ready === false
    };
}));
const measureHiddenPlatforms = () => {
    const grid = platformGrid.value;
    if (!grid) {
        hiddenPlatformCount.value = 0;
        return;
    }
    const tiles = [...grid.querySelectorAll('.platform-tile')];
    if (!tiles.length) {
        hiddenPlatformCount.value = 0;
        return;
    }
    const firstTop = tiles[0].offsetTop;
    hiddenPlatformCount.value = tiles.filter(tile => tile.offsetTop > firstTop + 2).length;
};
const selectedSendModel = computed(() => sendModelList.value.find(model => model.id === sendModel.value) || null);
const selectedModelType = computed(() => selectedSendModel.value?.type || 'text');
const selectedModelParameters = computed(() => selectedSendModel.value?.web_parameters || []);
const selectedModelCapabilities = computed(() => selectedSendModel.value?.capabilities || []);
const currentModelIsTranscription = computed(() => selectedModelType.value === 'transcription');
const currentModelIsMedia = computed(() => ['video', 'audio'].includes(selectedModelType.value));
const currentModelUsesTextOptions = computed(() => selectedModelType.value === 'text');

const parameterSourceName = (source) => ({ web: '网页已验证', adapter: '适配器支持', api: '接口参数' }[source] || '已声明');
const modelTypeName = (type) => ({ text: '文本', image: '图片', video: '视频', audio: '音乐', transcription: '转写' }[type] || type);

const selectFirstAvailableModel = () => {
    if (accountModels.value.some(model => model.id === sendModel.value)) return;
    sendModel.value = accountModels.value[0]?.id || '';
};

const initializeRequestPath = () => {
    const providers = sendProviderOptions.value;
    if (!providers.length) return;
    if (!providers.some(provider => provider.id === sendProvider.value)) {
        sendProvider.value = providers.find(provider => provider.id === 'doubao')?.id || providers[0].id;
    }
    const accounts = sendAccountOptions.value;
    if (!accounts.some(account => account.id === sendAccount.value)) {
        sendAccount.value = accounts[0]?.id || '';
    }
    selectFirstAvailableModel();
};

async function fetchNativeProviderAccounts(providerId) {
    if (!nativeProviderFor(nativeProviders.value, providerId)) return;
    try {
        const response = await fetch(`/admin/providers/${encodeURIComponent(providerId)}/accounts`, {
            headers: settingsStore.getHeaders()
        });
        if (!response.ok) return;
        const payload = await response.json();
        nativeProviderAccounts.value = {
            ...nativeProviderAccounts.value,
            [providerId]: Array.isArray(payload.data) ? payload.data : []
        };
    } catch (error) {
        console.warn(`无法读取 ${providerId} 账号列表`, error);
    }
}

watch([platformTiles, platformExpanded], async () => {
    await nextTick();
    measureHiddenPlatforms();
});

watch(sendProvider, async providerId => {
    await fetchNativeProviderAccounts(providerId);
    const accounts = sendAccountOptions.value;
    if (!accounts.some(account => account.id === sendAccount.value)) {
        sendAccount.value = accounts[0]?.id || '';
    }
    selectFirstAvailableModel();
});

watch(sendAccount, selectFirstAvailableModel);

watch(sendModel, (modelId) => {
    sendImageList.value = [];
    sendAudioFile.value = null;
    const model = sendModelList.value.find(item => item.id === modelId);
    if (!model) return;
    if (modelProviderId(model) !== sendProvider.value) sendProvider.value = modelProviderId(model);
    if (modelAccountId(model) !== sendAccount.value) sendAccount.value = modelAccountId(model);
});

// 当前模型是否支持图片输入
const currentModelSupportsImage = computed(() => {
    const model = selectedSendModel.value;
    if (!model) return false;
    return model.image_policy !== 'forbidden';
});

// 自动刷新
let autoRefreshInterval = null;

// 移动端检测
const isMobile = ref(window.innerWidth <= 768);
let resizeHandler = null;

// 状态配置
const statusConfig = {
    success: { color: '#52c41a', text: '成功', icon: CheckCircleOutlined },
    failed: { color: '#ff4d4f', text: '失败', icon: CloseCircleOutlined },
    pending: { color: '#faad14', text: '处理中', icon: ClockCircleOutlined }
};

// 获取历史列表
const fetchHistory = async () => {
    loading.value = true;
    try {
        const params = new URLSearchParams({
            page: page.value,
            pageSize: pageSize.value
        });

        if (statusFilter.value && statusFilter.value !== 'all') {
            params.append('status', statusFilter.value);
        }
        if (modelFilter.value) {
            params.append('model', modelFilter.value);
        }
        if (searchText.value) {
            params.append('search', searchText.value);
        }
        if (dateRange.value && dateRange.value.length === 2) {
            params.append('startDate', dateRange.value[0].format('YYYY-MM-DD'));
            params.append('endDate', dateRange.value[1].format('YYYY-MM-DD'));
        }

        const res = await fetch(`/admin/history?${params.toString()}`, {
            headers: settingsStore.getHeaders()
        });
        if (res.ok) {
            const data = await res.json();
            records.value = data.items || [];
            total.value = data.total || 0;
            // 预加载缩略图
            preloadThumbnails();
        }
    } catch (e) {
        message.error('获取历史记录失败');
    } finally {
        loading.value = false;
    }
};

// 预加载列表中的缩略图
const preloadThumbnails = async () => {
    for (const record of records.value) {
        if (record.responseMedia && record.responseMedia.length > 0) {
            const media = record.responseMedia[0];
            if (media.localPath && media.status === 'downloaded') {
                await getMediaBlobUrl(media);
            }
        }
    }
};

// 获取统计摘要
const fetchStats = async () => {
    try {
        const params = new URLSearchParams();
        if (dateRange.value && dateRange.value.length === 2) {
            params.append('startDate', dateRange.value[0].format('YYYY-MM-DD'));
            params.append('endDate', dateRange.value[1].format('YYYY-MM-DD'));
        }

        const res = await fetch(`/admin/history/stats?${params.toString()}`, {
            headers: settingsStore.getHeaders()
        });
        if (res.ok) {
            stats.value = await res.json();
        }
    } catch (e) {
        console.error('获取统计失败', e);
    }
};

// 获取模型列表
const fetchModels = async () => {
    try {
        const res = await fetch('/admin/history/models', {
            headers: settingsStore.getHeaders()
        });
        if (res.ok) {
            modelOptions.value = await res.json();
        }
    } catch (e) {
        console.error('获取模型列表失败', e);
    }
};

// 查看详情
const viewDetail = async (record) => {
    drawerVisible.value = true;
    detailLoading.value = true;
    try {
        const res = await fetch(`/admin/history/${record.id}`, {
            headers: settingsStore.getHeaders()
        });
        if (res.ok) {
            currentRecord.value = await res.json();
            // 预加载详情中的媒体
            if (currentRecord.value.responseMedia) {
                for (const media of currentRecord.value.responseMedia) {
                    if (media.localPath && media.status === 'downloaded') {
                        await getMediaBlobUrl(media);
                    }
                }
            }
        }
    } catch (e) {
        message.error('获取详情失败');
    } finally {
        detailLoading.value = false;
    }
};

// 获取媒体 Blob URL（带认证）
const getMediaBlobUrl = async (media) => {
    if (!media.localPath) return null;

    const filename = media.localPath.split('/').pop();
    const cacheKey = filename;

    // 检查缓存
    if (mediaCache.value[cacheKey]) {
        return mediaCache.value[cacheKey];
    }

    try {
        const res = await fetch(`/admin/history/media/${filename}`, {
            headers: settingsStore.getHeaders()
        });
        if (res.ok) {
            const blob = await res.blob();
            const blobUrl = URL.createObjectURL(blob);
            mediaCache.value[cacheKey] = blobUrl;
            return blobUrl;
        }
    } catch (e) {
        console.error('获取媒体失败', e);
    }
    return null;
};

// 获取缓存的 blob URL
const getCachedMediaUrl = (media) => {
    if (!media || !media.localPath) return null;
    const filename = media.localPath.split('/').pop();
    return mediaCache.value[filename] || null;
};

// 重试下载媒体
const retryMedia = async (recordId, mediaIndex) => {
    try {
        const res = await fetch(`/admin/history/${recordId}/retry-media`, {
            method: 'POST',
            headers: {
                ...settingsStore.getHeaders(),
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({ mediaIndex })
        });

        if (res.ok) {
            message.success('下载成功');
            fetchHistory();
            if (currentRecord.value && currentRecord.value.id === recordId) {
                viewDetail(currentRecord.value);
            }
        } else {
            const data = await res.json();
            message.error(data.message || '下载失败');
        }
    } catch (e) {
        message.error('请求失败');
    }
};

// 删除记录
const deleteRecords = (ids) => {
    Modal.confirm({
        title: '确认删除',
        content: `确定要删除这 ${ids.length} 条记录吗？关联的媒体文件也会被删除。`,
        okText: '删除',
        okType: 'danger',
        cancelText: '取消',
        async onOk() {
            try {
                const res = await fetch('/admin/history', {
                    method: 'DELETE',
                    headers: {
                        ...settingsStore.getHeaders(),
                        'Content-Type': 'application/json'
                    },
                    body: JSON.stringify({ ids })
                });
                if (res.ok) {
                    message.success('删除成功');
                    clearSelection();
                    fetchHistory();
                    fetchStats();
                } else {
                    message.error('删除失败');
                }
            } catch (e) {
                message.error('请求失败');
            }
        }
    });
};

// 按日期范围删除
const deleteByDateRange = () => {
    if (!dateRange.value || dateRange.value.length !== 2) {
        message.warning('请先选择日期范围');
        return;
    }

    Modal.confirm({
        title: '确认删除',
        content: `确定要删除 ${dateRange.value[0].format('YYYY-MM-DD')} 至 ${dateRange.value[1].format('YYYY-MM-DD')} 的所有记录吗？`,
        okText: '删除',
        okType: 'danger',
        cancelText: '取消',
        async onOk() {
            try {
                const params = new URLSearchParams({
                    startDate: dateRange.value[0].format('YYYY-MM-DD'),
                    endDate: dateRange.value[1].format('YYYY-MM-DD')
                });
                const res = await fetch(`/admin/history?${params.toString()}`, {
                    method: 'DELETE',
                    headers: settingsStore.getHeaders()
                });
                if (res.ok) {
                    const data = await res.json();
                    message.success(`已删除 ${data.deleted} 条记录`);
                    clearSelection();
                    fetchHistory();
                    fetchStats();
                } else {
                    message.error('删除失败');
                }
            } catch (e) {
                message.error('请求失败');
            }
        }
    });
};

// 格式化时间
const formatTime = (timestamp) => {
    if (!timestamp) return '-';
    const date = new Date(timestamp);
    return date.toLocaleString('zh-CN', {
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit'
    });
};

// 格式化耗时
const formatDuration = (ms) => {
    if (!ms) return '-';
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
};

// 截断文本
const truncateText = (text, maxLen = 120) => {
    if (!text) return '-';
    return text.length > maxLen ? text.substring(0, maxLen) + '...' : text;
};

// 判断响应是否有媒体内容
const hasMedia = (record) => {
    return record.responseMedia && record.responseMedia.length > 0;
};

// 获取第一个媒体
const getFirstMedia = (record) => {
    if (!hasMedia(record)) return null;
    return record.responseMedia[0];
};

// 表格列定义
const columns = [
    {
        title: '状态',
        dataIndex: 'status',
        key: 'status',
        width: 70,
        align: 'center'
    },
    {
        title: 'Prompt',
        dataIndex: 'prompt',
        key: 'prompt',
        width: 200
    },
    {
        title: '模型',
        dataIndex: 'model_name',
        key: 'model_name',
        width: 150,
        ellipsis: true
    },
    {
        title: '响应',
        key: 'response',
        width: 220
    },
    {
        title: '媒体',
        key: 'media',
        width: 180,
        align: 'center'
    },
    {
        title: '时间',
        dataIndex: 'created_at',
        key: 'created_at',
        width: 100,
        customRender: ({ value }) => formatTime(value)
    },
    {
        title: '耗时',
        dataIndex: 'duration_ms',
        key: 'duration_ms',
        width: 60,
        align: 'right',
        customRender: ({ value }) => formatDuration(value)
    },
    {
        title: '',
        key: 'action',
        width: 100,
        align: 'center',
        fixed: 'right'
    }
];

// 监听筛选变化
watch([statusFilter, modelFilter, dateRange], () => {
    page.value = 1;
    fetchHistory();
    fetchStats();
});

// 搜索防抖
let searchTimeout = null;
watch(searchText, () => {
    if (searchTimeout) clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => {
        page.value = 1;
        fetchHistory();
    }, 300);
});

// 分页变化
const handleTableChange = (pagination) => {
    page.value = pagination.current;
    pageSize.value = pagination.pageSize;
    clearSelection();
    fetchHistory();
};

// 刷新
const handleRefresh = () => {
    fetchHistory();
    fetchModels();
};

// 快速预览响应内容
const previewResponse = async (record) => {
    previewModalVisible.value = true;
    previewMediaType.value = 'text';
    previewTitle.value = '响应预览';
    if (record.status === 'failed') {
        previewContent.value = record.error_message || '未知错误';
    } else {
        previewContent.value = record.response_text || '无响应';
    }
};

// 快速预览 Prompt 内容
const previewPrompt = (record) => {
    previewModalVisible.value = true;
    previewMediaType.value = 'text';
    previewTitle.value = 'Prompt 预览';
    previewContent.value = record.prompt || '无内容';
};

// 复制预览内容到剪贴板
const copyPreviewContent = async () => {
    try {
        await navigator.clipboard.writeText(previewContent.value);
        message.success('已复制到剪贴板');
    } catch (e) {
        message.error('复制失败');
    }
};

// 快速预览媒体
const previewMedia = async (record) => {
    const media = getFirstMedia(record);
    if (!media) return;

    if (media.type === 'image') {
        previewMediaType.value = 'image';
    } else if (media.type === 'video') {
        previewMediaType.value = 'video';
    } else {
        previewMediaType.value = 'text';
        previewContent.value = media.originalUrl || '无预览';
        previewModalVisible.value = true;
        return;
    }

    if (media.status === 'downloaded') {
        const url = await getMediaBlobUrl(media);
        if (url) {
            previewMediaUrl.value = url;
            previewModalVisible.value = true;
        } else {
            message.error('预览加载失败');
        }
    } else {
        previewContent.value = '媒体未下载或下载失败，请查看详情并重试下载';
        previewMediaType.value = 'text';
        previewModalVisible.value = true;
    }
};

// 关闭预览弹窗
const closePreview = () => {
    previewModalVisible.value = false;
    previewContent.value = '';
    previewMediaUrl.value = '';
    previewMediaType.value = 'text';
    previewTitle.value = '快速预览';
};

// 多选变化
const onSelectChange = (keys, rows) => {
    selectedRowKeys.value = keys;
    selectedRows.value = rows;
};

// 批量删除选中
const deleteSelected = () => {
    if (selectedRowKeys.value.length === 0) {
        message.warning('请先选择要删除的记录');
        return;
    }
    deleteRecords(selectedRowKeys.value);
};

// 清空选择
const clearSelection = () => {
    selectedRowKeys.value = [];
    selectedRows.value = [];
};

// === 发送请求功能 ===

// 获取可用模型列表
const fetchSendModelList = async () => {
    try {
        const [modelResponse, providerResponse] = await Promise.all([
            fetch('/v1/models', { headers: settingsStore.getHeaders() }),
            fetch('/admin/providers', { headers: settingsStore.getHeaders() })
        ]);
        if (modelResponse.ok) {
            const data = await modelResponse.json();
            sendModelList.value = data.data || [];
        }
        if (providerResponse.ok) {
            const data = await providerResponse.json();
            nativeProviders.value = Array.isArray(data.data) ? data.data : [];
        }
        initializeRequestPath();
        await Promise.all(nativeProviders.value.map(provider => fetchNativeProviderAccounts(provider.id)));
    } catch (e) {
        console.error('获取模型列表失败', e);
    }
};

const openSelectedProviderSettings = () => {
    if (!selectedNativeProvider.value) return;
    router.push({ path: '/settings/providers', query: { provider: selectedNativeProvider.value.id } });
};

// 图片转 base64
const fileToBase64 = (file) => {
    return new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.readAsDataURL(file);
        reader.onload = () => resolve(reader.result);
        reader.onerror = reject;
    });
};

// 图片上传前检查
const beforeUpload = (file) => {
    const allowedTypes = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
    if (!allowedTypes.includes(file.type)) {
        message.error('仅支持 PNG, JPEG, GIF, WebP 格式');
        return false;
    }
    if (sendImageList.value.length >= 10) {
        message.error('最多上传 10 张图片');
        return false;
    }
    return false;
};

// 处理图片选择
const handleSendImageChange = async (info) => {
    const file = info.file;
    if (file.status === 'removed') {
        sendImageList.value = sendImageList.value.filter(f => f.uid !== file.uid);
        return;
    }
    try {
        const base64 = await fileToBase64(file.originFileObj || file);
        sendImageList.value.push({ uid: file.uid, name: file.name, base64, file: file.originFileObj || file });
    } catch (e) {
        message.error('图片读取失败');
    }
};

const beforeAudioUpload = (file) => {
    if (!file.type?.startsWith('audio/')) {
        message.error('请选择音频文件');
        return false;
    }
    return false;
};

const handleSendAudioChange = (info) => {
    if (info.file.status === 'removed') {
        sendAudioFile.value = null;
        return;
    }
    sendAudioFile.value = info.file.originFileObj || info.file;
};

const responseError = async (res) => {
    try {
        const data = await res.json();
        return data?.error?.message || data?.message || '请求失败';
    } catch {
        return `请求失败 (${res.status})`;
    }
};

const uploadMediaReferences = async () => Promise.all(sendImageList.value.map(async (image) => {
    const form = new FormData();
    form.append('file', image.file);
    form.append('purpose', 'vision');
    const res = await fetch('/v1/files', { method: 'POST', headers: settingsStore.getHeaders(), body: form });
    if (!res.ok) throw new Error(await responseError(res));
    return (await res.json()).id;
}));

const sendChatRequest = async () => {
    const content = sendImageList.value.length > 0
        ? [
            { type: 'text', text: sendPrompt.value },
            ...sendImageList.value.map(image => ({ type: 'image_url', image_url: { url: image.base64 } }))
        ]
        : sendPrompt.value;
    const body = {
        model: sendModel.value,
        messages: [{ role: 'user', content }],
        stream: sendStreamMode.value
    };
    if (sendReasoningMode.value) body.reasoning = true;
    const res = await fetch('/v1/chat/completions', {
        method: 'POST',
        headers: { ...settingsStore.getHeaders(), 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
    });
    if (!res.ok) throw new Error(await responseError(res));
};

const sendMediaRequest = async () => {
    const type = selectedModelType.value;
    if (type === 'transcription') {
        const form = new FormData();
        form.append('model', sendModel.value);
        form.append('file', sendAudioFile.value);
        const res = await fetch('/v1/audio/transcriptions', { method: 'POST', headers: settingsStore.getHeaders(), body: form });
        if (!res.ok) throw new Error(await responseError(res));
        const data = await res.json();
        message.success(data.text ? '录音转写完成' : '录音转写已提交');
        return;
    }

    const body = { model: sendModel.value, prompt: sendPrompt.value };
    if (type === 'video' && sendImageList.value.length > 0) {
        body.input_reference = await uploadMediaReferences();
    }
    const endpoint = type === 'video' ? '/v1/videos' : '/v1/audio/generations';
    const res = await fetch(endpoint, {
        method: 'POST',
        headers: { ...settingsStore.getHeaders(), 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
    });
    if (!res.ok) throw new Error(await responseError(res));
};

// 发送请求
const sendRequest = async () => {
    if (selectedNativeProviderNeedsAccount.value) {
        message.warning(`请先配置 ${selectedProviderName.value} 账号`);
        return;
    }
    if (!sendModel.value) {
        message.warning('请选择模型');
        return;
    }
    if (currentModelIsTranscription.value && !sendAudioFile.value) {
        message.warning('请上传音频文件');
        return;
    }
    if (!currentModelIsTranscription.value && !sendPrompt.value.trim()) {
        message.warning('请输入提示词');
        return;
    }
    sending.value = true;
    try {
        if (currentModelIsMedia.value || currentModelIsTranscription.value) {
            await sendMediaRequest();
            if (!currentModelIsTranscription.value) message.success('媒体任务已提交');
        } else {
            await sendChatRequest();
            message.success('请求已发送');
            startAutoRefresh();
            setTimeout(() => {
                silentFetchHistory();
                silentFetchStats();
            }, 1000);
        }
        sendPrompt.value = '';
        sendImageList.value = [];
        sendAudioFile.value = null;
    } catch (error) {
        message.error(error.message || '请求发送失败');
    } finally {
        sending.value = false;
    }
};

// 静默删除记录（不弹确认框）
const silentDeleteRecord = async (id) => {
    try {
        await fetch('/admin/history', {
            method: 'DELETE',
            headers: {
                ...settingsStore.getHeaders(),
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({ ids: [id] })
        });
    } catch (e) { /* 静默失败 */ }
};

// 从历史记录重发
const resendFromRecord = (record) => {
    const modelId = record.model_id || record.model_name;
    if (modelId) {
        const matchingModel = sendModelList.value.find(model => model.id === modelId)
            || sendModelList.value.find(model => model.id.endsWith(`/${modelId}`));
        sendModel.value = matchingModel?.id || modelId;
    }
    if (record.prompt) {
        sendPrompt.value = record.prompt;
    }
    sendImageList.value = [];

    // 如果原记录是失败状态（没有生成回复或图片），重发后删除旧记录
    const shouldDelete = record.status === 'failed';

    sendRequest();

    if (shouldDelete) {
        silentDeleteRecord(record.id);
    }
};

// === 自动刷新 ===
const silentFetchHistory = async () => {
    try {
        const params = new URLSearchParams({ page: page.value, pageSize: pageSize.value });
        if (statusFilter.value && statusFilter.value !== 'all') params.append('status', statusFilter.value);
        if (modelFilter.value) params.append('model', modelFilter.value);
        if (searchText.value) params.append('search', searchText.value);
        if (dateRange.value && dateRange.value.length === 2) {
            params.append('startDate', dateRange.value[0].format('YYYY-MM-DD'));
            params.append('endDate', dateRange.value[1].format('YYYY-MM-DD'));
        }
        const res = await fetch(`/admin/history?${params.toString()}`, { headers: settingsStore.getHeaders() });
        if (res.ok) {
            const data = await res.json();
            records.value = data.items || [];
            total.value = data.total || 0;
            preloadThumbnails();
        }
    } catch (e) { /* 静默失败 */ }
};

const silentFetchStats = async () => {
    try {
        const params = new URLSearchParams();
        if (dateRange.value && dateRange.value.length === 2) {
            params.append('startDate', dateRange.value[0].format('YYYY-MM-DD'));
            params.append('endDate', dateRange.value[1].format('YYYY-MM-DD'));
        }
        const res = await fetch(`/admin/history/stats?${params.toString()}`, { headers: settingsStore.getHeaders() });
        if (res.ok) { stats.value = await res.json(); }
    } catch (e) { /* 静默失败 */ }
};

const startAutoRefresh = () => {
    if (autoRefreshInterval) return;
    autoRefreshInterval = setInterval(() => {
        silentFetchHistory();
        silentFetchStats();
    }, 5000);
};

const stopAutoRefresh = () => {
    if (autoRefreshInterval) {
        clearInterval(autoRefreshInterval);
        autoRefreshInterval = null;
    }
};

let platformResizeObserver = null;
onMounted(() => {
    resizeHandler = () => {
        isMobile.value = window.innerWidth <= 768;
        measureHiddenPlatforms();
    };
    window.addEventListener('resize', resizeHandler);
    if (typeof ResizeObserver === 'function') {
        platformResizeObserver = new ResizeObserver(() => measureHiddenPlatforms());
    }
    fetchHistory();
    fetchStats();
    fetchModels();
    fetchSendModelList();
    nextTick().then(() => {
        if (platformResizeObserver && platformGrid.value) {
            platformResizeObserver.observe(platformGrid.value);
        }
        measureHiddenPlatforms();
    });
});

onUnmounted(() => {
    stopAutoRefresh();
    if (resizeHandler) window.removeEventListener('resize', resizeHandler);
    platformResizeObserver?.disconnect();
});
</script>

<template>
    <!-- 发送请求 -->
    <a-card class="request-card" :bordered="false" style="margin-bottom: 24px">
        <template #title>发送请求</template>
        <div class="platform-rail" aria-label="平台">
            <div class="platform-rail-head">
                <span class="request-platform-label"><AppstoreOutlined /> 平台</span>
                <button
                    v-if="platformExpanded || hiddenPlatformCount > 0"
                    type="button"
                    class="platform-rail-toggle"
                    @click="platformExpanded = !platformExpanded"
                >
                    <template v-if="platformExpanded"><UpOutlined /> 收起</template>
                    <template v-else><DownOutlined /> 展开 {{ hiddenPlatformCount }}</template>
                </button>
            </div>
            <div ref="platformGrid" class="platform-rail-grid" :class="{ expanded: platformExpanded }">
                <button
                    v-for="tile in platformTiles"
                    :key="tile.id"
                    type="button"
                    class="platform-tile"
                    :class="{ active: tile.id === sendProvider, pending: tile.pending }"
                    :title="tile.name"
                    @click="sendProvider = tile.id"
                >
                    <span class="platform-tile-icon" :class="{ 'has-logo': tile.icon }" :style="tile.icon ? undefined : { background: tile.color }">
                        <img v-if="tile.icon" class="platform-tile-logo" :src="tile.icon" :alt="tile.short" @error="failPlatformIcon(tile.id)" />
                        <template v-else>{{ tile.mark }}</template>
                    </span>
                    <span class="platform-tile-meta">
                        <span class="platform-tile-name">{{ tile.short }}</span>
                        <strong class="platform-tile-count">{{ tile.count }}</strong>
                    </span>
                    <span v-if="tile.pending" class="platform-tile-pip" />
                </button>
            </div>
        </div>
        <div class="request-path" aria-label="模型请求路径">
            <div class="request-path-step">
                <div class="request-path-label"><UserOutlined /> 账户别名</div>
                <a-select v-model:value="sendAccount" size="small" :options="sendAccountOptions.map(account => ({ value: account.id, label: account.name }))" />
            </div>
            <div class="request-path-step">
                <div class="request-path-label"><RocketOutlined /> 模型</div>
                <a-select v-model:value="sendModel" size="small" placeholder="选择模型" show-search>
                    <a-select-option v-for="model in accountModels" :key="model.id" :value="model.id">
                        {{ model.display_name || model.id }}
                    </a-select-option>
                </a-select>
            </div>
        </div>

        <a-alert
            v-if="selectedNativeProviderPending || selectedNativeProviderNeedsAccount"
            :type="selectedNativeProviderPending ? 'info' : 'warning'"
            show-icon
            style="margin-top: 12px"
            :message="selectedNativeProviderPending
                ? `${selectedProviderName} 已吸收，网页协议尚未接通。可先登记账号，登录材料后续再配置。`
                : `${selectedProviderName} 已吸收，但尚未配置可调用账号`"
        >
            <template #action>
                <a-button size="small" type="primary" @click="openSelectedProviderSettings">配置账号</a-button>
            </template>
        </a-alert>

        <section v-if="selectedSendModel" class="selected-model-summary" aria-label="模型参数">
            <div class="selected-model-heading">
                <div>
                    <strong>{{ selectedSendModel.display_name || selectedSendModel.id }}</strong>
                    <span class="model-id">{{ selectedSendModel.id }}</span>
                </div>
                <a-space size="small" wrap>
                    <a-tag color="blue">{{ modelTypeName(selectedModelType) }}</a-tag>
                    <a-tag v-for="capability in selectedModelCapabilities" :key="capability" color="cyan">{{ capability }}</a-tag>
                </a-space>
            </div>
            <a-descriptions v-if="selectedModelParameters.length" size="small" :column="isMobile ? 1 : 2" bordered>
                <a-descriptions-item v-for="parameter in selectedModelParameters" :key="parameter.key" :label="parameter.label">
                    <div class="parameter-value-list">
                        <a-tag v-for="value in parameter.values" :key="value" :color="parameter.source === 'web' ? 'green' : 'default'">
                            {{ value }}
                        </a-tag>
                        <span v-if="parameter.note" class="parameter-note">{{ parameter.note }}</span>
                        <span class="parameter-source">{{ parameterSourceName(parameter.source) }}</span>
                    </div>
                </a-descriptions-item>
            </a-descriptions>
        </section>

        <div class="request-composer">
            <!-- 左侧：模型 + 提示词 -->
            <div class="request-prompt-column">
                <!-- 提示词 -->
                <div v-if="!currentModelIsTranscription" style="margin-bottom: 12px;">
                    <div style="font-size: 12px; color: #a1a1aa; margin-bottom: 4px;">提示词</div>
                    <a-textarea v-model:value="sendPrompt" placeholder="输入提示词" :rows="3" size="small" />
                </div>

                <div v-else class="audio-upload-wrap">
                    <div style="font-size: 12px; color: #a1a1aa; margin-bottom: 4px;">录音文件</div>
                    <a-upload-dragger :file-list="[]" :before-upload="beforeAudioUpload" @change="handleSendAudioChange" accept="audio/*" :show-upload-list="false">
                        <p style="margin: 0;"><AudioOutlined style="font-size: 20px; color: #e54d5e;" /></p>
                        <p style="font-size: 12px; margin: 2px 0 0; color: #a1a1aa;">点击或拖拽上传音频</p>
                    </a-upload-dragger>
                    <a-tag v-if="sendAudioFile" closable style="margin-top: 8px;" @close="sendAudioFile = null">
                        <AudioOutlined /> {{ sendAudioFile.name }}
                    </a-tag>
                </div>

                <!-- 选项 + 发送按钮 -->
                <div style="display: flex; align-items: center; gap: 16px; flex-wrap: wrap;">
                    <a-checkbox v-if="currentModelUsesTextOptions" v-model:checked="sendStreamMode">流式响应</a-checkbox>
                    <a-checkbox v-if="currentModelUsesTextOptions" v-model:checked="sendReasoningMode">返回思考</a-checkbox>
                    <a-button type="primary" @click="sendRequest" :loading="sending" :disabled="!sendModel || selectedNativeProviderNeedsAccount">
                        <template #icon><RocketOutlined /></template>
                        发送
                    </a-button>
                </div>
            </div>

            <!-- 右侧：图片上传（仅支持图片的模型显示） -->
            <div v-if="currentModelSupportsImage" class="send-upload-area">
                <div style="font-size: 12px; color: #a1a1aa; margin-bottom: 4px;">
                    {{ selectedModelType === 'video' ? '参考图' : '附加图片' }} ({{ sendImageList.length }}/10)
                </div>
                <a-upload-dragger :file-list="[]" :multiple="true" :before-upload="beforeUpload"
                    @change="handleSendImageChange" accept=".png,.jpg,.jpeg,.gif,.webp" :show-upload-list="false">
                    <p style="margin: 0;">
                        <InboxOutlined style="font-size: 20px; color: #e54d5e;" />
                    </p>
                    <p style="font-size: 12px; margin: 2px 0 0 0; color: #a1a1aa;">
                        点击或拖拽上传图片
                    </p>
                </a-upload-dragger>
                <div v-if="sendImageList.length > 0" style="margin-top: 8px; display: flex; flex-wrap: wrap; gap: 4px;">
                    <a-tag v-for="img in sendImageList" :key="img.uid" closable
                        @close="sendImageList = sendImageList.filter(i => i.uid !== img.uid)">
                        <PictureOutlined /> {{ img.name.slice(0, 15) }}{{ img.name.length > 15 ? '...' : '' }}
                    </a-tag>
                </div>
            </div>
        </div>
    </a-card>

    <!-- 统计摘要 -->
    <a-card title="请求记录" :bordered="false">
        <template #extra>
            <a-button type="link" danger size="small" @click="deleteByDateRange"
                :disabled="!dateRange || dateRange.length !== 2">
                <template #icon>
                    <DeleteOutlined />
                </template>
                删除所选范围
            </a-button>
        </template>

        <div class="stats-content">
            <a-range-picker v-model:value="dateRange" :format="'YYYY-MM-DD'" :placeholder="['开始日期', '结束日期']"
                size="small" class="stats-date-picker" />

            <a-divider type="vertical" style="height: 32px; margin: 0 16px" />

            <div class="stats-numbers">
                <div class="stat-item neutral">
                    <FileTextOutlined />
                    <span class="stat-value">{{ stats.total }}</span>
                    <span class="stat-label">总数</span>
                </div>
                <div class="stat-item success">
                    <CheckCircleOutlined />
                    <span class="stat-value">{{ stats.success }}</span>
                    <span class="stat-label">成功</span>
                </div>
                <div class="stat-item error">
                    <CloseCircleOutlined />
                    <span class="stat-value">{{ stats.failed }}</span>
                    <span class="stat-label">失败</span>
                </div>
                <div class="stat-item neutral">
                    <ClockCircleOutlined />
                    <span class="stat-value">{{ formatDuration(stats.avgDuration) }}</span>
                    <span class="stat-label">平均耗时</span>
                </div>
            </div>
        </div>
    </a-card>

    <!-- 历史记录表格 -->
    <a-card :bordered="false" style="margin-top: 24px">
        <!-- 筛选工具栏 -->
        <div class="toolbar">
            <div class="toolbar-row">
                <a-select v-model:value="statusFilter" class="toolbar-status-select" size="small" placeholder="状态">
                    <a-select-option value="all">全部状态</a-select-option>
                    <a-select-option value="success">成功</a-select-option>
                    <a-select-option value="failed">失败</a-select-option>
                    <a-select-option value="pending">处理中</a-select-option>
                </a-select>
                <a-select v-model:value="modelFilter" class="toolbar-model-select" size="small" placeholder="全部模型"
                    allow-clear show-search>
                    <a-select-option v-for="model in modelOptions" :key="model" :value="model">
                        {{ model }}
                    </a-select-option>
                </a-select>
                <a-button size="small" @click="handleRefresh">
                    <template #icon>
                        <ReloadOutlined />
                    </template>
                </a-button>
                <a-button v-if="selectedRowKeys.length > 0" type="primary" danger size="small" @click="deleteSelected">
                    <template #icon>
                        <DeleteOutlined />
                    </template>
                    删除选中 ({{ selectedRowKeys.length }})
                </a-button>
            </div>
            <div class="toolbar-row">
                <a-input-search v-model:value="searchText" placeholder="搜索 Prompt 或响应内容" size="small"
                    allow-clear style="width: 100%;" />
            </div>
        </div>

        <!-- 表格 -->
        <a-table
            :columns="columns"
            :data-source="records"
            :loading="loading"
            :row-selection="{
                selectedRowKeys: selectedRowKeys,
                onChange: onSelectChange,
                columnWidth: 40
            }"
            :pagination="{
                current: page,
                pageSize: pageSize,
                total: total,
                showSizeChanger: true,
                showQuickJumper: true,
                showTotal: (total) => `共 ${total} 条`,
                pageSizeOptions: ['20', '50', '100', '200']
            }"
            row-key="id"
            size="small"
            :scroll="{ x: 1000 }"
            @change="handleTableChange"
        >
            <template #bodyCell="{ column, record }">
                <!-- Prompt 列：支持多行，点击弹出预览 -->
                <template v-if="column.key === 'prompt'">
                    <div class="multiline-text clickable" @click="previewPrompt(record)" title="点击查看完整内容">
                        {{ truncateText(record.prompt, 120) }}
                    </div>
                </template>

                <!-- 响应列 -->
                <template v-else-if="column.key === 'response'">
                    <div v-if="record.status === 'failed'" class="multiline-text error-text clickable"
                        @click="previewResponse(record)" title="点击查看完整内容">
                        {{ truncateText(record.error_message, 120) || '错误' }}
                    </div>
                    <div v-else class="multiline-text response-text clickable"
                        @click="previewResponse(record)" title="点击查看完整内容">
                        {{ truncateText(record.response_text, 120) || '-' }}
                    </div>
                </template>

                <!-- 媒体列：显示缩略图 -->
                <template v-else-if="column.key === 'media'">
                    <div v-if="hasMedia(record)" class="media-thumb-cell" @click="previewMedia(record)" title="点击查看大图">
                        <template v-if="getFirstMedia(record).status === 'downloaded'">
                            <img
                                v-if="getFirstMedia(record).type === 'image'"
                                :src="getCachedMediaUrl(getFirstMedia(record))"
                                class="thumb-img"
                                loading="lazy"
                            />
                            <div v-else-if="getFirstMedia(record).type === 'video'" class="thumb-video">
                                <PlayCircleOutlined />
                            </div>
                        </template>
                        <div v-else class="thumb-placeholder">
                            <PictureOutlined v-if="getFirstMedia(record).type === 'image'" />
                            <PlayCircleOutlined v-else />
                        </div>
                        <span v-if="record.responseMedia.length > 1" class="media-count">
                            +{{ record.responseMedia.length - 1 }}
                        </span>
                    </div>
                    <span v-else class="no-media">-</span>
                </template>

                <!-- 状态列 -->
                <template v-else-if="column.key === 'status'">
                    <a-tag :color="statusConfig[record.status]?.color || '#8c8c8c'" size="small">
                        {{ statusConfig[record.status]?.text || record.status }}
                    </a-tag>
                </template>

                <!-- 操作列 -->
                <template v-else-if="column.key === 'action'">
                    <a-space :size="0">
                        <a-tooltip title="重发">
                            <a-button type="link" size="small" @click="resendFromRecord(record)">
                                <template #icon>
                                    <RedoOutlined />
                                </template>
                            </a-button>
                        </a-tooltip>
                        <a-tooltip title="详情">
                            <a-button type="link" size="small" @click="viewDetail(record)">
                                <template #icon>
                                    <EyeOutlined />
                                </template>
                            </a-button>
                        </a-tooltip>
                        <a-tooltip title="删除">
                            <a-button type="link" size="small" danger @click="deleteRecords([record.id])">
                                <template #icon>
                                    <DeleteOutlined />
                                </template>
                            </a-button>
                        </a-tooltip>
                    </a-space>
                </template>
            </template>
        </a-table>
    </a-card>

    <!-- 详情抽屉 -->
    <a-drawer v-model:open="drawerVisible" title="请求详情" placement="right" :width="isMobile ? '100%' : 700" :destroy-on-close="true">
        <a-spin :spinning="detailLoading">
            <template v-if="currentRecord">
                <!-- 基本信息 -->
                <a-descriptions :column="isMobile ? 1 : 2" size="small" bordered>
                    <a-descriptions-item label="请求 ID" :span="2">
                        <code>{{ currentRecord.id }}</code>
                    </a-descriptions-item>
                    <a-descriptions-item label="时间">
                        {{ new Date(currentRecord.created_at).toLocaleString('zh-CN') }}
                    </a-descriptions-item>
                    <a-descriptions-item label="状态">
                        <a-tag :color="statusConfig[currentRecord.status]?.color">
                            {{ statusConfig[currentRecord.status]?.text || currentRecord.status }}
                        </a-tag>
                    </a-descriptions-item>
                    <a-descriptions-item label="模型" :span="2">
                        {{ currentRecord.model_name || currentRecord.model_id || '-' }}
                    </a-descriptions-item>
                    <a-descriptions-item label="耗时">
                        {{ formatDuration(currentRecord.duration_ms) }}
                    </a-descriptions-item>
                    <a-descriptions-item label="流式">
                        {{ currentRecord.isStreaming ? '是' : '否' }}
                    </a-descriptions-item>
                </a-descriptions>

                <!-- Prompt -->
                <a-divider orientation="left">Prompt</a-divider>
                <div class="content-box">
                    {{ currentRecord.prompt || '无' }}
                </div>

                <!-- 输入图片 -->
                <template v-if="currentRecord.inputImages && currentRecord.inputImages.length > 0">
                    <a-divider orientation="left">输入图片</a-divider>
                    <div class="media-list">
                        <span v-for="(img, idx) in currentRecord.inputImages" :key="idx" class="media-item">
                            <a-tag>{{ img.split('/').pop() }}</a-tag>
                        </span>
                    </div>
                </template>

                <!-- 响应内容 -->
                <a-divider orientation="left">响应内容</a-divider>
                <div class="content-box" :class="{ 'error-box': currentRecord.status === 'failed' }">
                    <template v-if="currentRecord.status === 'failed'">
                        {{ currentRecord.error_message || '未知错误' }}
                    </template>
                    <template v-else>
                        {{ currentRecord.response_text || '无响应' }}
                    </template>
                </div>

                <!-- 思考过程 -->
                <template v-if="currentRecord.reasoning_content">
                    <a-divider orientation="left">思考过程</a-divider>
                    <div class="content-box reasoning-box">
                        {{ currentRecord.reasoning_content }}
                    </div>
                </template>

                <!-- 媒体内容 -->
                <template v-if="currentRecord.responseMedia && currentRecord.responseMedia.length > 0">
                    <a-divider orientation="left">媒体内容 ({{ currentRecord.responseMedia.length }})</a-divider>
                    <div class="media-gallery-large">
                        <div v-for="(media, idx) in currentRecord.responseMedia" :key="idx" class="media-card-large">
                            <div class="media-preview-large">
                                <template v-if="media.status === 'downloaded' && getCachedMediaUrl(media)">
                                    <img v-if="media.type === 'image'" :src="getCachedMediaUrl(media)" alt="生成图片" />
                                    <video v-else-if="media.type === 'video'" :src="getCachedMediaUrl(media)" controls />
                                </template>
                                <template v-else>
                                    <div class="media-placeholder-large">
                                        <PictureOutlined v-if="media.type === 'image'" />
                                        <PlayCircleOutlined v-else-if="media.type === 'video'" />
                                        <FileTextOutlined v-else />
                                        <div class="media-status">
                                            <a-tag v-if="media.status === 'failed'" color="red">下载失败</a-tag>
                                            <a-tag v-else-if="media.status === 'external'" color="blue">外部链接</a-tag>
                                            <a-tag v-else-if="media.status === 'pending'" color="orange">待下载</a-tag>
                                        </div>
                                        <a-button v-if="media.status === 'failed'" type="primary" size="small"
                                            @click="retryMedia(currentRecord.id, idx)">
                                            <template #icon>
                                                <ReloadOutlined />
                                            </template>
                                            重试下载
                                        </a-button>
                                    </div>
                                </template>
                            </div>
                        </div>
                    </div>
                </template>
            </template>
        </a-spin>
    </a-drawer>

    <!-- 快速预览弹窗 -->
    <a-modal
        v-model:open="previewModalVisible"
        :footer="null"
        :width="isMobile ? '95%' : (previewMediaType === 'image' || previewMediaType === 'video' ? '90%' : '70%')"
        centered
        @cancel="closePreview"
    >
        <template #title>
            <div style="display: flex; align-items: center; gap: 8px;">
                <span>{{ previewTitle }}</span>
                <a-button v-if="previewMediaType === 'text'" type="text" size="small" @click="copyPreviewContent">
                    <template #icon><CopyOutlined /></template>
                    复制全文
                </a-button>
            </div>
        </template>
        <div v-if="previewMediaType === 'text'" class="preview-text-content">
            {{ previewContent }}
        </div>
        <div v-else-if="previewMediaType === 'image'" class="preview-image-content">
            <img :src="previewMediaUrl" alt="预览图片" />
        </div>
        <div v-else-if="previewMediaType === 'video'" class="preview-video-content">
            <video :src="previewMediaUrl" controls autoplay />
        </div>
    </a-modal>
</template>

<style scoped>

.request-card :deep(.ant-card-head) {
    min-height: 56px;
}

.request-card :deep(.ant-card-head-wrapper) {
    gap: 12px;
    flex-wrap: wrap;
}

.request-card :deep(.ant-card-extra) {
    margin-inline-start: 0;
    padding: 0;
}
.platform-rail {
    margin: -4px 0 14px;
}

.platform-rail-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 8px;
}

.request-platform-label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: #a1a1aa;
    font-size: 13px;
    white-space: nowrap;
}

.platform-rail-toggle {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    border: 0;
    padding: 0;
    background: transparent;
    color: #e54d5e;
    font-size: 12px;
    cursor: pointer;
}

.platform-rail-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(132px, 1fr));
    column-gap: 8px;
    row-gap: 8px;
}

.platform-rail-grid:not(.expanded) {
    grid-template-rows: minmax(52px, auto);
    grid-auto-rows: 0;
    row-gap: 0;
    overflow: hidden;
}

.platform-tile {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    min-height: 52px;
    padding: 8px 10px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
    background: #121a2b;
    text-align: left;
    cursor: pointer;
}

.platform-tile:hover {
    border-color: rgba(229, 77, 94, 0.45);
    background: #1a2234;
}

.platform-tile.active {
    border-color: #e54d5e;
    background: rgba(229, 77, 94, 0.14);
    box-shadow: inset 0 0 0 1px #e54d5e;
}

.platform-tile-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 7px;
    color: #fff;
    font-size: 13px;
    font-weight: 700;
    flex-shrink: 0;
    overflow: hidden;
}

.platform-tile-icon.has-logo {
    background: #161b22;
    border: 1px solid rgba(255, 255, 255, 0.08);
}

.platform-tile-logo {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
}

.platform-tile-meta {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.15;
}

.platform-tile-name {
    color: #a1a1aa;
    font-size: 12px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.platform-tile-count {
    color: #e6e6ef;
    font-size: 18px;
    font-weight: 700;
    font-family: "SF Mono", Monaco, monospace;
}

.platform-tile-pip {
    position: absolute;
    top: 7px;
    right: 7px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #faad14;
}

.request-path {
    display: grid;
    grid-template-columns: repeat(2, minmax(180px, 1fr));
    gap: 12px;
    padding-bottom: 14px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.request-path-step {
    min-width: 0;
}

.request-path-step :deep(.ant-select) {
    width: 100%;
}

.request-path-label {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 5px;
    color: #a1a1aa;
    font-size: 12px;
}

.selected-model-summary {
    margin: 14px 0;
    padding: 12px;
    border: 1px solid rgba(229, 77, 94, 0.28);
    border-radius: 6px;
    background: #121a2b;
}

.selected-model-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 10px;
}

.model-id {
    display: block;
    margin-top: 2px;
    color: #a1a1aa;
    font-family: 'SF Mono', 'Monaco', monospace;
    font-size: 11px;
    overflow-wrap: anywhere;
}

.parameter-value-list {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    min-width: 0;
}

.parameter-note {
    color: #a1a1aa;
    font-size: 12px;
}

.parameter-source {
    color: #a1a1aa;
    font-size: 11px;
}

.request-composer {
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
}

.request-prompt-column {
    flex: 1;
    min-width: 280px;
}

.audio-upload-wrap :deep(.ant-upload-drag) {
    min-height: 86px;
}

/* 图片上传区域高度控制 */
.send-upload-area :deep(.ant-upload-drag) {
    height: calc(100% - 20px);
}

/* 统计内容样式 */
.stats-content {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
}

.stats-numbers {
    display: flex;
    align-items: center;
    gap: 20px;
}

.stat-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 12px;
    background: #121a2b;
    border-radius: 6px;
    transition: all 0.2s;
}

.stat-item:hover {
    background: #1a2234;
}

.stat-item.success {
    color: #52c41a;
}

.stat-item.error {
    color: #ff4d4f;
}

.stat-item.neutral {
    color: #a1a1aa;
}

.stat-value {
    font-size: 18px;
    font-weight: 600;
    font-family: 'SF Mono', 'Monaco', monospace;
}

.stat-label {
    font-size: 12px;
    color: #a1a1aa;
}

/* 工具栏样式 */
.toolbar {
    margin-bottom: 16px;
}

.toolbar-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
}

.toolbar-row:last-child {
    margin-bottom: 0;
}

/* 工具栏 select 默认宽度 */
.toolbar-status-select {
    width: 100px;
}

.toolbar-model-select {
    width: 200px;
}

@media (min-width: 768px) {
    .toolbar {
        display: flex;
        justify-content: space-between;
        align-items: center;
        gap: 12px;
    }

    .toolbar-row {
        margin-bottom: 0;
    }

    .toolbar-row:last-child {
        flex: 1;
        max-width: 300px;
    }
}

@media (max-width: 767px) {
    .platform-rail-grid {
        grid-template-columns: repeat(auto-fill, minmax(108px, 1fr));
    }

    .request-path {
        grid-template-columns: 1fr;
    }

    .selected-model-heading {
        flex-direction: column;
    }
}

/* 表格内样式 */
.error-text {
    color: #ff4d4f;
    font-size: 12px;
}

.response-text {
    font-size: 12px;
    color: #a1a1aa;
}

/* 多行文本 */
.multiline-text {
    font-size: 12px;
    line-height: 1.5;
    max-height: 54px;  /* 约 3 行 */
    overflow: hidden;
    word-break: break-all;
}

.multiline-text.clickable {
    cursor: pointer;
    padding: 4px;
    margin: -4px;
    border-radius: 4px;
    transition: background 0.2s;
}

.multiline-text.clickable:hover {
    background: #1a2234;
}

.no-media {
    color: #bfbfbf;
}

/* 表格行高度适配大缩略图 */
:deep(.ant-table-tbody > tr > td) {
    vertical-align: middle;
}

/* 列表缩略图 - 160x160 */
.media-thumb-cell {
    position: relative;
    width: 160px;
    height: 160px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    margin: 8px auto;
}

.thumb-img {
    width: 160px;
    height: 160px;
    object-fit: cover;
    border-radius: 4px;
    border: 1px solid rgba(255, 255, 255, 0.08);
}

.thumb-video {
    width: 160px;
    height: 160px;
    background: #000;
    border-radius: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #fff;
    font-size: 28px;
}

.thumb-placeholder {
    width: 160px;
    height: 160px;
    background: #121a2b;
    border: 1px dashed rgba(255, 255, 255, 0.16);
    border-radius: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #bfbfbf;
    font-size: 24px;
}

.media-count {
    position: absolute;
    bottom: 4px;
    right: 4px;
    background: rgba(0, 0, 0, 0.6);
    color: #fff;
    font-size: 11px;
    padding: 2px 6px;
    border-radius: 3px;
}

/* 内容框样式 */
.content-box {
    background: #121a2b;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 4px;
    padding: 12px;
    font-family: 'Consolas', 'Monaco', monospace;
    font-size: 13px;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 600px;
    overflow-y: auto;
}

.content-box.error-box {
    color: #ff4d4f;
    background: rgba(239, 68, 68, 0.12);
    border-color: rgba(239, 68, 68, 0.35);
}

.content-box.reasoning-box {
    background: rgba(34, 197, 94, 0.12);
    border-color: rgba(34, 197, 94, 0.35);
    color: #389e0d;
}

/* 详情页媒体样式 - 更大尺寸 */
.media-gallery-large {
    display: flex;
    flex-direction: column;
    gap: 16px;
}

.media-card-large {
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
    overflow: hidden;
    background: #121a2b;
}

.media-preview-large {
    width: 100%;
    min-height: 300px;
    max-height: 500px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #111520;
}

.media-preview-large img {
    max-width: 100%;
    max-height: 500px;
    object-fit: contain;
}

.media-preview-large video {
    max-width: 100%;
    max-height: 500px;
}

.media-placeholder-large {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    color: #bfbfbf;
    gap: 12px;
    padding: 40px;
    font-size: 48px;
}

.media-status {
    font-size: 14px;
}

/* 媒体列表 */
.media-list {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
}

/* 预览弹窗内容 */
.preview-text-content {
    background: #121a2b;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 4px;
    padding: 16px;
    font-family: 'Consolas', 'Monaco', monospace;
    font-size: 14px;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 60vh;
    overflow-y: auto;
    line-height: 1.6;
}

.preview-image-content {
    display: flex;
    justify-content: center;
    align-items: center;
    min-height: 200px;
}

.preview-image-content img {
    max-width: 100%;
    max-height: 70vh;
    object-fit: contain;
    border-radius: 4px;
}

.preview-video-content {
    display: flex;
    justify-content: center;
    align-items: center;
}

.preview-video-content video {
    max-width: 100%;
    max-height: 70vh;
    border-radius: 4px;
}

/* 图片上传区域尺寸 */
.send-upload-area {
    flex: 0 0 280px;
    min-width: 200px;
}

/* 日期选择器 */
.stats-date-picker {
    width: 240px;
}

/* 响应式 - 平板及以下 */
@media (max-width: 768px) {
    .send-upload-area {
        flex: 1 1 100% !important;
        min-width: 0 !important;
    }

    .stats-date-picker {
        width: 100%;
    }

    .media-thumb-cell {
        width: 80px;
        height: 80px;
    }

    .thumb-img {
        width: 80px;
        height: 80px;
    }

    .thumb-video {
        width: 80px;
        height: 80px;
        font-size: 20px;
    }

    .thumb-placeholder {
        width: 80px;
        height: 80px;
        font-size: 18px;
    }

    .toolbar {
        display: flex;
        align-items: center;
        gap: 6px;
        flex-wrap: wrap;
    }

    .toolbar-row {
        flex-wrap: nowrap;
        margin-bottom: 0;
        gap: 4px;
    }

    .toolbar-row:last-child {
        flex: 1;
        min-width: 100px;
    }

    .toolbar-status-select {
        width: 80px !important;
    }

    .toolbar-model-select {
        width: 100px !important;
    }

    .stat-value {
        font-size: 14px;
    }

    .stat-item {
        padding: 2px 8px;
    }

    .content-box {
        max-height: 400px;
        font-size: 12px;
        padding: 8px;
    }

    .media-preview-large {
        min-height: 200px;
        max-height: 350px;
    }
}

/* 响应式 - 手机 */
@media (max-width: 576px) {
    .stats-content {
        flex-direction: column;
        align-items: flex-start;
    }

    .stats-content .ant-divider {
        display: none;
    }

    .stats-numbers {
        margin-top: 8px;
        flex-wrap: wrap;
        gap: 8px;
    }

    .stat-item {
        padding: 2px 6px;
        gap: 4px;
    }

    .stat-value {
        font-size: 13px;
    }

    .stat-label {
        font-size: 11px;
    }
}
</style>

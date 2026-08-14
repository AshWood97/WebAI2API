import { createApp, h } from 'vue'
import { createPinia } from 'pinia'
import Antd, { ConfigProvider, theme } from 'ant-design-vue';
import { createRouter, createWebHistory } from 'vue-router'
import App from './App.vue'
import 'ant-design-vue/dist/reset.css';
import './theme.css';

const routes = [
    { path: '/', component: () => import('@/components/tools/request.vue') },
    { path: '/dash', component: () => import('@/components/dash.vue') },
    { path: '/settings/server', component: () => import('@/components/settings/server.vue') },
    { path: '/settings/workers', component: () => import('@/components/settings/workers.vue') },
    { path: '/settings/browser', component: () => import('@/components/settings/browser.vue') },
    { path: '/settings/adapters', component: () => import('@/components/settings/adapters.vue') },
    { path: '/settings/qwen', component: () => import('@/components/settings/native-providers.vue') },
    { path: '/settings/providers', component: () => import('@/components/settings/native-providers.vue') },
    { path: '/tools/display', component: () => import('@/components/tools/display.vue') },
    { path: '/tools/cache', component: () => import('@/components/tools/cache.vue') },
    { path: '/tools/logs', component: () => import('@/components/tools/logs.vue') },
    { path: '/tools/request', component: () => import('@/components/tools/request.vue') },
];

const router = createRouter({
    history: createWebHistory(location.pathname === '/webai2api' || location.pathname.startsWith('/webai2api/') ? '/webai2api/' : '/'),
    routes
})

const pinia = createPinia()
const omniTheme = {
    algorithm: theme.darkAlgorithm,
    token: {
        colorPrimary: '#e54d5e',
        colorBgBase: '#0b0e14',
        colorBgContainer: '#161b22',
        colorBgLayout: '#0b0e14',
        colorTextBase: '#e6e6ef',
        colorBorder: 'rgba(255,255,255,0.12)',
        borderRadius: 8,
    },
};
const app = createApp({
    render() {
        return h(ConfigProvider, { theme: omniTheme }, { default: () => h(App) });
    }
});
app.use(pinia)
app.use(router)
app.use(Antd)
app.mount('#app')

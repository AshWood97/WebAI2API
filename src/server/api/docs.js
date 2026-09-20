/**
 * 构建 /docs HTML
 * @param {{title?: string, version?: string}} [options]
 * @returns {string}
 */
function escapeHtml(s) {
    return String(s ?? '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

export function buildDocsHtml(options = {}) {
    const title = options.title || 'WebAI2API API 文档';
    const version = options.version || '';
    return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>${escapeHtml(title)}</title>
<style>
  :root {
    --bg:#09090b; --surface:#111113; --border:#1f1f23; --ink:#e4e4e7;
    --ink2:#a1a1aa; --ink3:#52525b; --accent:#6366f1; --ok:#22c55e; --warn:#eab308;
  }
  * { box-sizing:border-box; margin:0; padding:0; }
  body { background:var(--bg); color:var(--ink); font-family:-apple-system,BlinkMacSystemFont,"PingFang SC","Segoe UI",sans-serif; line-height:1.55; padding:24px 16px 48px; }
  .wrap { max-width:920px; margin:0 auto; }
  h1 { font-size:22px; font-weight:600; margin-bottom:6px; }
  .meta { color:var(--ink2); font-size:13px; margin-bottom:18px; }
  .meta code { font-family:ui-monospace,SFMono-Regular,Consolas,monospace; color:var(--accent); }
  .card { background:var(--surface); box-shadow:0 0 0 1px var(--border); border-radius:12px; margin-bottom:14px; overflow:hidden; }
  .card header { padding:12px 16px; border-bottom:1px solid var(--border); font-size:13px; font-weight:600; text-transform:uppercase; letter-spacing:.05em; }
  table { width:100%; border-collapse:collapse; font-size:13.5px; }
  th, td { text-align:left; padding:9px 12px; border-bottom:1px solid var(--border); vertical-align:top; }
  th { color:var(--ink3); font-size:11px; text-transform:uppercase; letter-spacing:.06em; background:var(--bg); }
  tr:last-child td { border-bottom:none; }
  .method { font-family:ui-monospace,monospace; font-size:11px; font-weight:600; padding:2px 8px; border-radius:6px; background:var(--accent); color:#fff; }
  .method.get { background:#0ea5e9; }
  .path { font-family:ui-monospace,monospace; font-size:12.5px; color:var(--ink); }
  .tag { display:inline-block; font-size:11px; color:var(--ink2); background:var(--bg); box-shadow:0 0 0 1px var(--border); border-radius:6px; padding:2px 8px; margin-right:4px; }
  .sum { color:var(--ink2); font-size:13px; }
  .err { color:#f87171; padding:16px; }
  .copybar { display:flex; flex-wrap:wrap; gap:8px; padding:14px 16px; align-items:center; }
  .copybar input { flex:1; min-width:220px; background:var(--bg); border:1px solid var(--border); color:var(--ink); border-radius:8px; padding:8px 12px; font-family:ui-monospace,monospace; font-size:12px; }
  button { background:var(--accent); color:#fff; border:none; border-radius:8px; padding:8px 14px; cursor:pointer; font-size:13px; }
  button.ghost { background:transparent; color:var(--accent); box-shadow:0 0 0 1px var(--accent); }
  .note { color:var(--ink3); font-size:12px; padding:0 16px 14px; }
</style>
</head>
<body>
<div class="wrap">
  <h1>${escapeHtml(title)}</h1>
  <p class="meta">版本 <code id="ver">${escapeHtml(version || '…')}</code> · OpenAPI: <code>/openapi.json</code> · 管理台: <code>/</code></p>

  <div class="card">
    <header>快速接入</header>
    <div class="copybar">
      <input id="baseUrl" readonly />
      <button onclick="copyVal('baseUrl')">复制 Base URL</button>
      <button class="ghost" onclick="copyVal('curlVal')">复制示例 curl</button>
    </div>
    <div class="copybar">
      <input id="curlVal" readonly />
    </div>
    <div class="note">Bearer Token 使用 config.yaml 中 server.auth 的值。生成请求请优先使用 stream:true。</div>
  </div>

  <div class="card">
    <header>端点目录</header>
    <div id="out"><div class="note">正在加载 /openapi.json …</div></div>
  </div>
</div>
<script>
function copyVal(id) {
  const el = document.getElementById(id);
  if (!el) return;
  const text = el.value || el.textContent;
  (navigator.clipboard?.writeText(text) || Promise.reject()).then(
    () => {},
    () => { el.select?.(); document.execCommand?.('copy'); }
  );
}
function methodClass(m) { return String(m||'').toLowerCase() === 'get' ? 'method get' : 'method'; }
function esc(s) {
  return String(s ?? '').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
}

const origin = location.origin;
// WebUI 使用 admin_token；兼容旧键
const token = localStorage.getItem('admin_token') || localStorage.getItem('webai2api_token') || 'YOUR_API_KEY';
document.getElementById('baseUrl').value = origin + '/v1';
document.getElementById('curlVal').value =
  "curl -sS " + origin + "/v1/chat/completions -H 'Authorization: Bearer " + token +
  "' -H 'Content-Type: application/json' -d '{\\"model\\":\\"your-model\\",\\"messages\\":[{\\"role\\":\\"user\\",\\"content\\":\\"hi\\"}],\\"stream\\":true}'";

fetch('/openapi.json')
  .then(r => r.json())
  .then(spec => {
    document.getElementById('ver').textContent = spec.info?.version || '';
    const paths = spec.paths || {};
    const rows = [];
    for (const [p, ops] of Object.entries(paths)) {
      for (const [method, op] of Object.entries(ops)) {
        if (!['get','post','delete','put','patch'].includes(method)) continue;
        const tags = (op.tags || []).map(t => '<span class="tag">' + esc(t) + '</span>').join('');
        rows.push(
          '<tr><td><span class="' + methodClass(method) + '">' + esc(method.toUpperCase()) +
          '</span></td><td class="path">' + esc(p) + '</td><td>' + tags +
          '</td><td class="sum">' + esc(op.summary || op.description || '') + '</td></tr>'
        );
      }
    }
    document.getElementById('out').innerHTML =
      '<table><thead><tr><th>方法</th><th>路径</th><th>标签</th><th>说明</th></tr></thead><tbody>' +
      rows.join('') + '</tbody></table>';
  })
  .catch(e => {
    document.getElementById('out').innerHTML = '<div class="err">加载 OpenAPI 失败: ' + esc(e.message) + '</div>';
  });
</script>
</body>
</html>`;
}

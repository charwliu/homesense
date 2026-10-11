'use strict';
let credential = '';
const status = document.getElementById('status');
const list = document.getElementById('scenes');
async function request(path, options = {}) {
  const response = await fetch(path, {...options, cache: 'no-store', credentials: 'omit', headers: {...options.headers, Authorization: `Bearer ${credential}`}});
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || '请求失败');
  return result;
}
async function refresh() {
  const data = await request('/api/v1/scenes');
  list.replaceChildren();
  for (const scene of data.scenes) {
    const row = document.createElement('article');
    const title = document.createElement('h2'); title.textContent = scene.name;
    const info = document.createElement('p'); info.textContent = `当前状态：${scene.activity}；最近执行：${scene.last_run?.status || '无记录'}`;
    const button = document.createElement('button'); button.textContent = '执行场景'; button.disabled = !scene.can_control;
    button.onclick = async () => {
      button.disabled = true;
      try {
        const result = await request('/api/v1/scene-actions', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({scene: scene.id, request_id: crypto.randomUUID()})});
        status.textContent = result.outcome === 'accepted' ? '请求已接受，请核对执行记录和设备反馈。' : '结果尚不明确，请核对记录；不要重复点击。';
        await refresh();
      } catch (error) { status.textContent = `${error.message}；未自动重试。`; }
    };
    row.append(title, info, button); list.append(row);
  }
}
document.getElementById('connect').onclick = async () => {
  credential = document.getElementById('token').value; document.getElementById('token').value = '';
  try { await refresh(); status.textContent = '已连接'; } catch (error) { credential = ''; list.replaceChildren(); status.textContent = error.message; }
};
document.getElementById('logout').onclick = () => { credential = ''; list.replaceChildren(); status.textContent = '已退出'; };

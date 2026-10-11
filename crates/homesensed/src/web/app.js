'use strict';
let csrf = '';
const started = new Map();
const status = document.getElementById('status');
const list = document.getElementById('scenes');
async function request(path, options = {}) {
  const response = await fetch(path, {...options, cache: 'no-store', credentials: 'same-origin', headers: {...options.headers, ...(csrf ? {'X-CSRF-Token': csrf} : {})}});
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
        const requestId = crypto.randomUUID();
        const result = await request('/api/v1/scene-actions', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({scene: scene.id, request_id: requestId})});
        if (result.outcome === 'accepted') started.set(scene.id, requestId);
        status.textContent = result.outcome === 'accepted' ? '请求已接受，请核对执行记录和设备反馈。' : '结果尚不明确，请核对记录；不要重复点击。';
        await refresh();
      } catch (error) { status.textContent = `${error.message}；未自动重试。`; }
    };
    const cancel = document.createElement('button'); cancel.textContent = '取消剩余动作'; cancel.disabled = !scene.can_control || !started.has(scene.id);
    cancel.onclick = async () => {
      cancel.disabled = true;
      try {
        const result = await request('/api/v1/scene-cancellations', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({start_request_id: started.get(scene.id), request_id: crypto.randomUUID()})});
        status.textContent = result.outcome === 'cancelled' ? '剩余动作已取消；已执行动作不回滚。' : '取消结果未知，请核对记录。';
        started.delete(scene.id); await refresh();
      } catch (error) { status.textContent = error.message; }
    };
    row.append(title, info, button, cancel); list.append(row);
  }
}
document.getElementById('connect').onclick = async () => {
  const credential = document.getElementById('token').value; document.getElementById('token').value = '';
  try {
    const response = await fetch('/api/v1/session', {method: 'POST', credentials: 'same-origin', headers: {Authorization: `Bearer ${credential}`}});
    if (!response.ok) throw new Error('登录失败');
    csrf = (await response.json()).csrf; await refresh(); status.textContent = '已连接（会话 15 分钟有效）';
  } catch (error) { csrf = ''; list.replaceChildren(); status.textContent = error.message; }
};
document.getElementById('logout').onclick = async () => {
  try { await request('/api/v1/session', {method: 'DELETE'}); csrf = ''; started.clear(); list.replaceChildren(); status.textContent = '已退出'; }
  catch (error) { status.textContent = '注销未确认：' + error.message; }
};

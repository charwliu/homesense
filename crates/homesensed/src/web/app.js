'use strict';
let csrf = '';
let publicationPreview=null;
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
  const capabilities = await request('/api/v1/session');
  document.getElementById('configuration').hidden = !capabilities.can_manage_configuration;
  const data = await request('/api/v1/scenes');
  await refreshDevices();
  renderScenes(data);
}
function renderScenes(data) {
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
    csrf = (await response.json()).csrf; eventCursor=null; await refresh(); connectEvents(); status.textContent = '已连接（会话 15 分钟有效）';
  } catch (error) { csrf = ''; list.replaceChildren(); status.textContent = error.message; }
};
document.getElementById('logout').onclick = async () => {
  try { await request('/api/v1/session', {method: 'DELETE'}); csrf = ''; clearTimeout(eventRetry); if(eventSocket)eventSocket.close();eventDevices.clear(); document.getElementById('draft-impact').textContent=''; document.getElementById('configuration').hidden=true; for(const id of ['device-document','automation-document','draft-id','base-revision','draft-revision']) document.getElementById(id).value=''; started.clear(); list.replaceChildren(); document.getElementById('devices').replaceChildren(); status.textContent = '已退出'; }
  catch (error) { status.textContent = '注销未确认：' + error.message; }
};

async function refreshDevices() {
  const data = await request('/api/v1/devices');
  eventDevices.clear(); for(const device of data.devices) eventDevices.set(device.id,device);
  renderDevices();
}
function renderDevices() {
  const devices = document.getElementById('devices'); devices.replaceChildren();
  for (const device of eventDevices.values()) {
    const row = document.createElement('article');
    const title = document.createElement('h2'); title.textContent = device.name;
    const info = document.createElement('p');
    info.textContent = device.observed === null ? '状态未知' : `${device.observed.on ? '开' : '关'}${device.observed.level === undefined ? '' : ' · ' + device.observed.level + '%'}`;
    info.textContent += ` · ${device.quality}${device.pending === null ? '' : ' · 等待反馈'}`;
    row.append(title, info);
    const send = async command => {
      try {
        const result = await request('/api/v1/device-commands', {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({device:device.id, request_id:crypto.randomUUID(), command})});
        status.textContent = result.outcome === 'accepted' ? '控制已接受，等待设备反馈。' : '结果未知，请核对设备；不要重复点击。';
        await refreshDevices();
      } catch(error) {status.textContent = `${error.message}；未自动重试。`;}
    };
    for (const on of [true,false]) {
      const button=document.createElement('button'); button.textContent=on?'开':'关'; button.disabled=!device.can_control;
      button.onclick=async()=>{button.disabled=true;await send({action:'power',on});}; row.append(button);
    }
    if(device.kind==='Dimmer') {
      const level=document.createElement('input'); level.type='number';level.min='0';level.max='100';level.step='1';level.setAttribute('aria-label',device.name+'亮度');
      level.value=device.observed?.level ?? '';
      const apply=document.createElement('button');apply.textContent='设置亮度';apply.disabled=!device.can_control;
      apply.onclick=async()=>{if(level.value===''||!level.checkValidity())return;apply.disabled=true;await send({action:'brightness',level:Number(level.value)});};row.append(level,apply);
    }
    devices.append(row);
  }
}

document.getElementById('refresh').onclick = () => refresh().catch(error => {status.textContent=error.message;});

document.getElementById('save-draft').onclick = async () => {
  const button=document.getElementById('save-draft'); button.disabled=true;
  try {
    const id=crypto.randomUUID(); document.getElementById('draft-id').value=id;
    const body={expected_revision:Number(document.getElementById('base-revision').value),revision:Number(document.getElementById('draft-revision').value),devices:JSON.parse(document.getElementById('device-document').value),automation:JSON.parse(document.getElementById('automation-document').value)};
    await request('/api/v1/config/drafts/'+id,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
    status.textContent='草稿已验证并保存；当前运行配置未改变。';
  } catch(error) {status.textContent=error.message+'；未自动重试，可按草稿 ID 核对。';}
  finally {button.disabled=false;}
};
document.getElementById('load-draft').onclick = async () => {
  try {
    const draft=await request('/api/v1/config/drafts/'+encodeURIComponent(document.getElementById('draft-id').value.trim()));
    document.getElementById('base-revision').value=draft.expected_revision;
    document.getElementById('draft-revision').value=draft.revision;
    document.getElementById('device-document').value=JSON.stringify(draft.devices,null,2);
    document.getElementById('automation-document').value=JSON.stringify(draft.automation,null,2);
    status.textContent='已读取草稿；当前运行配置未改变。';
  } catch(error) {status.textContent=error.message;}
};

document.getElementById('preview-draft').onclick = async () => {
  try {
    const result=await request('/api/v1/config/previews/'+encodeURIComponent(document.getElementById('draft-id').value.trim()));
    const impact=result.impact;
    publicationPreview={id:result.draft,boot:impact.boot}; document.getElementById('activate-draft').disabled=false;
    document.getElementById('draft-impact').textContent=`版本 ${impact.active_revision} → ${impact.candidate_revision}\n需要重建网关连接：${impact.connection_rebuild_required?'是':'否'}\n发布将结束旧版本规则和定时任务。\n受影响场景：\n` + (impact.interrupted_runs.map(run=>`${run.definition || run.audit_id} · ${run.status}`).join('\n') || '当前无未结束场景') + '\n此为当前快照，不锁定运行状态；尚未激活。';
  } catch(error) {document.getElementById('draft-impact').textContent='';status.textContent=error.message;}
};

document.getElementById('activate-draft').onclick = async () => {
  const preview=publicationPreview;
  if(!preview || preview.id!==document.getElementById('draft-id').value.trim()) {status.textContent='请先预览当前草稿。';return;}
  if(!window.confirm('激活将结束旧版本规则和未执行动作。已执行动作不会回滚。继续激活？'))return;
  document.getElementById('activate-draft').disabled=true; publicationPreview=null;
  try {
    const result=await request('/api/v1/config/activations/'+preview.id,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:crypto.randomUUID(),boot:preview.boot})});
    status.textContent=result.outcome==='activated'?`版本 ${result.revision} 已激活。`:'发布结果未知，请检查活动版本，不要重复激活。';
    await refresh();
  } catch(error) {status.textContent=error.message+'；发布可能已发生，请核对活动版本，未自动重试。';}
};

let eventSocket=null, eventCursor=null, eventRetry=null, eventAttempts=0;
const eventDevices=new Map();
function connectEvents() {
  clearTimeout(eventRetry);if(!csrf)return;
  if(eventSocket){eventSocket.onclose=null;eventSocket.close();}
  const socket=new WebSocket(location.origin.replace(/^http/,'ws')+'/api/v1/events');eventSocket=socket;
  socket.onopen=()=>socket.send(JSON.stringify({cursor:eventCursor}));
  socket.onmessage=event=>{
    try {
      const frame=JSON.parse(event.data);
      if(frame.type==='unavailable') {
        for(const [id,device] of eventDevices)eventDevices.set(id,{...device,quality:'Stale',can_control:false});
        renderDevices();status.textContent='配置正在切换，等待状态恢复。';return;
      }
      if(frame.type!=='state')return;
      if(frame.reset)eventDevices.clear();
      for(const device of frame.devices)eventDevices.set(device.id,device);
      renderDevices();renderScenes({scenes:frame.scenes});eventCursor=frame.cursor;eventAttempts=0;
    } catch(error){status.textContent='事件更新失败，请刷新状态。';socket.close();}
  };
  socket.onclose=()=>{
    if(!csrf)return;
    if(++eventAttempts>6){status.textContent='事件连接已断开，请重新登录。';return;}
    eventRetry=setTimeout(connectEvents,Math.min(10000,500*2**eventAttempts));
  };
}

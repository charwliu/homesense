'use strict';
let csrf = '';
let sessionEpoch=0;
let sessionBusy=false;
let publicationPreview=null;
let previewEpoch=0;
function invalidatePreview() {
  publicationPreview=null;previewEpoch++;
  document.getElementById('activate-draft').disabled=true;
  document.getElementById('draft-impact').textContent='';
  return previewEpoch;
}
for(const id of ['draft-id','base-revision','draft-revision','device-document','automation-document']) {
  document.getElementById(id).addEventListener('input',invalidatePreview);
}
let versionsBefore=null;
const started = new Map();
const status = document.getElementById('status');
const list = document.getElementById('scenes');
async function request(path, options = {}) {
  const epoch=sessionEpoch;
  const response = await fetch(path, {...options, cache: 'no-store', credentials: 'same-origin', headers: {...options.headers, ...(csrf ? {'X-CSRF-Token': csrf} : {})}});
  const result = await response.json();
  if(epoch!==sessionEpoch)throw new Error('会话已切换，已忽略旧响应');
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
  if(sessionBusy)return;sessionBusy=true;const epoch=++sessionEpoch;
  invalidatePreview();
  const credential = document.getElementById('token').value; document.getElementById('token').value = '';
  try {
    const response = await fetch('/api/v1/session', {method: 'POST', credentials: 'same-origin', headers: {Authorization: `Bearer ${credential}`}});
    if (!response.ok) throw new Error('登录失败');
    const session=await response.json();if(epoch!==sessionEpoch)return;
    csrf = session.csrf; eventCursor=null; await refresh(); connectEvents(); status.textContent = '已连接（会话 15 分钟有效）';
  } catch (error) { if(epoch===sessionEpoch){csrf = ''; list.replaceChildren(); status.textContent = error.message;} }
  finally {sessionBusy=false;}
};
document.getElementById('logout').onclick = async () => {
  if(sessionBusy)return;sessionBusy=true;++sessionEpoch;
  invalidatePreview();
  try { await request('/api/v1/session', {method: 'DELETE'}); csrf = ''; clearTimeout(eventRetry); if(eventSocket)eventSocket.close();eventDevices.clear(); document.getElementById('draft-impact').textContent=''; document.getElementById('configuration').hidden=true; document.getElementById('calendar-result').textContent='';document.getElementById('versions').replaceChildren();document.getElementById('draft-list').replaceChildren(); versionsBefore=null;document.getElementById('more-versions').disabled=true; for(const id of ['device-document','automation-document','draft-id','base-revision','draft-revision']) document.getElementById(id).value=''; started.clear(); list.replaceChildren(); document.getElementById('devices').replaceChildren(); status.textContent = '已退出'; }
  catch (error) { status.textContent = '注销未确认：' + error.message; }
  finally {sessionBusy=false;}
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
        status.textContent = result.outcome === 'accepted' ? (device.kind==='Motor'?'动作已接受；请观察实际移动和停止，位置仍未知。':'控制已接受，等待设备反馈。') : '结果未知，请核对设备；不要重复点击。';
        await refreshDevices();
      } catch(error) {status.textContent = `${error.message}；未自动重试。`;}
    };
    if(device.kind==='Motor') {
      info.textContent='位置未知 · '+(device.motor_calibrated?'方向和停止已校准':'尚未校准');
      for(const [action,label] of [['open','打开'],['close','关闭'],['stop','停止']]) {
        const button=document.createElement('button');button.textContent=label;button.disabled=!device.can_control;
        button.onclick=async()=>{button.disabled=true;await send({action});};row.append(button);
      }
    }
    if(device.kind==='Switch'||device.kind==='Dimmer') for (const on of [true,false]) {
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
  invalidatePreview();
  const button=document.getElementById('save-draft'); button.disabled=true;
  try {
    const id=crypto.randomUUID(); document.getElementById('draft-id').value=id;
    const body=encodeDraftJson(document.getElementById('base-revision').value,document.getElementById('draft-revision').value,document.getElementById('device-document').value,document.getElementById('automation-document').value);
    await request('/api/v1/config/drafts/'+id,{method:'POST',headers:{'Content-Type':'application/json'},body});
    status.textContent='草稿已验证并保存；当前运行配置未改变。';
  } catch(error) {status.textContent=error.message+'；未自动重试，可按草稿 ID 核对。';}
  finally {button.disabled=false;}
};
async function loadDraft(id) {
  const epoch=invalidatePreview();
  try {
    const draft=await request('/api/v1/config/drafts/'+encodeURIComponent(id)+'/source');
    if(epoch!==previewEpoch)return;
    document.getElementById('draft-id').value=draft.id;
    document.getElementById('base-revision').value=draft.expected_revision;
    document.getElementById('draft-revision').value=draft.revision;
    document.getElementById('device-document').value=draft.devices;
    document.getElementById('automation-document').value=draft.automation;
    status.textContent='已读取草稿；当前运行配置未改变。';
  } catch(error) {status.textContent=error.message;}
};

document.getElementById('load-draft').onclick=()=>loadDraft(document.getElementById('draft-id').value.trim());

document.getElementById('preview-draft').onclick = async () => {
  const epoch=invalidatePreview();const id=document.getElementById('draft-id').value.trim();
  try {
    const result=await request('/api/v1/config/previews/'+encodeURIComponent(id));
    if(epoch!==previewEpoch || id!==document.getElementById('draft-id').value.trim())return;
    const impact=result.impact;
    publicationPreview={id:result.draft,boot:impact.boot}; document.getElementById('activate-draft').disabled=false;
    document.getElementById('draft-impact').textContent=`版本 ${impact.active_revision} → ${impact.candidate_revision}\n需要重建网关连接：${impact.connection_rebuild_required?'是':'否'}\n发布将结束旧版本规则和定时任务。\n受影响场景：\n` + (impact.interrupted_runs.map(run=>`${run.definition || run.audit_id} · ${run.status}`).join('\n') || '当前无未结束场景') + '\n此为当前快照，不锁定运行状态；尚未激活。';
  } catch(error) {if(epoch===previewEpoch){invalidatePreview();status.textContent=error.message;}}
};

document.getElementById('activate-draft').onclick = async () => {
  const preview=publicationPreview;
  if(!preview || preview.id!==document.getElementById('draft-id').value.trim()) {status.textContent='请先预览当前草稿。';return;}
  if(!window.confirm('激活将结束旧版本规则和未执行动作。已执行动作不会回滚。继续激活？'))return;
  invalidatePreview();
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
  const epoch=sessionEpoch;
  const socket=new WebSocket(location.origin.replace(/^http/,'ws')+'/api/v1/events');eventSocket=socket;
  const current=()=>epoch===sessionEpoch && eventSocket===socket && !!csrf;
  socket.onopen=()=>{if(current())socket.send(JSON.stringify({cursor:eventCursor}));};
  socket.onmessage=event=>{
    if(!current())return;
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
    if(!current())return;
    if(++eventAttempts>6){status.textContent='事件连接已断开，请重新登录。';return;}
    eventRetry=setTimeout(()=>{if(current())connectEvents();},Math.min(10000,500*2**eventAttempts));
  };
}

async function loadVersions(before=null) {
  const first=document.getElementById('load-versions');const more=document.getElementById('more-versions');
  first.disabled=true;more.disabled=true;
  try {
    const page=await request('/api/v1/config/versions?limit=20'+(before===null?'':'&before='+encodeURIComponent(before)));
    const list=document.getElementById('versions');list.replaceChildren();
    for(const version of page.versions) {
      const row=document.createElement('p');
      const text=document.createElement('span');text.textContent=`版本 ${version.revision}${version.revision===page.active_revision?'（当前）':''} · 设备 ${version.device_bytes} 字节 · 自动化 ${version.automation_bytes??'无'} `;
      const download=document.createElement('button');download.textContent='导出';
      download.onclick=async()=>{
        download.disabled=true;const epoch=sessionEpoch;
        try {
          const response=await fetch(`/api/v1/config/versions/${version.revision}/export`,{cache:'no-store',credentials:'same-origin'});
          if(!response.ok)throw new Error('导出失败');
          const text=await response.text();if(epoch!==sessionEpoch)return;
          const blob=new Blob([text],{type:'application/json'});
          const url=URL.createObjectURL(blob);const link=document.createElement('a');link.href=url;link.download=`homesense-config-${version.revision}.json`;document.body.append(link);link.click();link.remove();setTimeout(()=>URL.revokeObjectURL(url),1000);
          status.textContent='已导出配置；当前运行版本未改变。';
        } catch(error) {status.textContent=error.message;}
        finally {download.disabled=false;}
      };
      row.append(text,download);list.append(row);
    }
    versionsBefore=page.next_before;
  } catch(error) {status.textContent=error.message;versionsBefore=null;}
  finally {first.disabled=false;more.disabled=versionsBefore===null;}
}
document.getElementById('load-versions').onclick=()=>loadVersions();
document.getElementById('more-versions').onclick=()=>{if(versionsBefore!==null)loadVersions(versionsBefore);};

document.getElementById('list-drafts').onclick=async()=>{
  const button=document.getElementById('list-drafts');button.disabled=true;
  try {
    const result=await request('/api/v1/config/drafts');const list=document.getElementById('draft-list');list.replaceChildren();
    if(!result.drafts.length)list.textContent='暂无已保存草稿。';
    for(const draft of result.drafts) {
      const row=document.createElement('p');const label=document.createElement('span');label.textContent=`草稿 ${draft.id} · ${draft.expected_revision} → ${draft.revision} `;
      const load=document.createElement('button');load.textContent='载入此草稿';load.onclick=()=>loadDraft(draft.id);row.append(label,load);list.append(row);
    }
  } catch(error) {status.textContent=error.message;}
  finally {button.disabled=false;}
};

document.getElementById('calendar-status').onclick = async () => {
  const epoch=sessionEpoch;
  const output=document.getElementById('calendar-result');output.textContent='读取中…';
  try {
    const data=await request('/api/v1/diagnostics/calendar');
    const state=data.calendar;
    output.textContent=state ? `配置版本：${data.revision}\n计划数量：${state.schedule_count}\n调度已停止：${state.stopped}\n最近时钟跳变：${state.last_clock_jump === null ? '尚未轮询' : state.last_clock_jump}\n内存水位（Unix ms）：${state.watermark_unix_ms}\n持久化水位（Unix ms）：${state.persisted_watermark_unix_ms ?? '尚未保存'}` : '当前运行时未安装日历调度器';
  } catch(error) {if(epoch===sessionEpoch)output.textContent=error.message;}
};

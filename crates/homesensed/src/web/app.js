'use strict';
let csrf = '';
let pendingLogoutCsrf='';
let sessionEpoch=0;
let sessionBusy=false;
let credentialIssueBusy=false;
let credentialEditSource=null;
let credentialEditEpoch=0;
let credentialEditBusy=false;
let backupInspectionEpoch=0;
let metadataSource=null;
let calibrationBinding=null;
let observedRevision=null;
let deviceSnapshotEpoch=0;
const deviceCards=new Map();
let lastSceneSnapshot=null;
let sceneActionBusy=false;
let sceneSnapshotEpoch=0;
let publicationPreview=null;
let publicationConfirmation=null;
let previewEpoch=0;
function invalidatePreview() {
  publicationPreview=null;publicationConfirmation=null;previewEpoch++;
  document.getElementById('publication-confirmation').hidden=true;
  document.getElementById('activate-draft').disabled=true;
  document.getElementById('draft-impact').textContent='';
  return previewEpoch;
}
for(const id of ['draft-id','base-revision','draft-revision','device-document','automation-document','metadata-device','metadata-name','metadata-room','binding-module-id','binding-gateway','binding-address','binding-channel','binding-enabled','calibration-direction','calibration-stop','calibration-evidence','calibration-time','edit-room-id','edit-room-name','edit-room-floor','edit-room-select','replacement-module','replacement-address','replacement-profile','replacement-verification','replacement-evidence','replacement-time']) {
  document.getElementById(id).addEventListener('input',invalidatePreview);
}
let modulePage=null;let moduleReadEpoch=0;let replacementSource=null;
let versionsBefore=null;
let selectedRoom='';
const started = new Map();
const status = document.getElementById('status');
const list = document.getElementById('scenes');
function selectionUnavailableMessage(reason) {
  return new Map([
    ['not_sampled','选择尚未读取，请等待有效快照。'],
    ['invalidated','选择快照已失效，请等待重新读取。'],
    ['cache_expired','选择快照已过期，请等待重新读取。'],
    ['runtime_unavailable','运行时不可用，请等待服务恢复。'],
    ['runtime_not_ready','运行时尚未就绪，请等待服务恢复。'],
    ['read_failed','选择读取失败，请等待有效快照恢复。'],
    ['read_timed_out','选择读取超时，请等待有效快照恢复。']
  ]).get(reason) || '选择状态不可用，请等待有效快照恢复。';
}
async function request(path, options = {}) {
  const epoch=sessionEpoch;
  const selectionEpoch=sceneSnapshotEpoch;
  const response = await fetch(path, {...options, cache: 'no-store', credentials: 'same-origin', headers: {...(options.body!==undefined ? {'Content-Type':'application/json'} : {}), ...options.headers, ...(csrf ? {'X-CSRF-Token': csrf} : {})}});
  if(epoch!==sessionEpoch)throw new Error('会话已切换，已忽略旧响应');
  if(response.status===401){clearLocalSession();status.textContent='会话已失效，请重新登录。';throw new Error('会话已失效，请重新登录。');}
  const result = await response.json();
  if(epoch!==sessionEpoch)throw new Error('会话已切换，已忽略旧响应');
  if (!response.ok) {
    if(path==='/api/v1/scenes' && response.status===503) {
      const reason=result.selection_state?.state==='unavailable'?result.selection_state.value:'runtime_unavailable';
      const message=selectionUnavailableMessage(reason);
      // A late failed read cannot invalidate a newer HTTP/WS snapshot.
      if(selectionEpoch===sceneSnapshotEpoch)markEventStateUnavailable(message,reason);
      throw new Error(message);
    }
    const messages=new Map([
      ['room_in_use','此房间仍有设备，请先在草稿中调整设备归属。'],
      ['credential_changed','授权已被其他操作修改，请重新读取。'],
      ['configuration_changed_or_unavailable','运行配置已改变，或服务暂时不可用。请刷新状态并核对草稿版本后重新预览。']
    ]);
    throw new Error(messages.get(result.error) || result.error || '请求失败');
  }
  return result;
}
async function refresh() {
  if(!csrf)throw new Error('请在本页重新登录后刷新状态。');
  const capabilities = await request('/api/v1/session');
  document.getElementById('configuration').hidden = !capabilities.can_manage_configuration;
  document.getElementById('diagnostics').hidden = !capabilities.can_view_diagnostics;
  document.getElementById('modules').hidden = !capabilities.can_view_diagnostics;
  const epoch=sceneSnapshotEpoch;
  const data = await request('/api/v1/scenes');
  await refreshDevices();
  if(epoch===sceneSnapshotEpoch)renderScenes(data);
}
function observeRevision(revision) {
  if(typeof revision!=='string'||!/^[1-9][0-9]*$/.test(revision))return true;
  if(observedRevision!==null && BigInt(revision)<BigInt(observedRevision))return false;
  if(observedRevision!==null && revision!==observedRevision) {
    deviceCards.clear();
    invalidatePreview();status.textContent='运行配置已切换；请重新核对草稿版本并预览。';
  }
  observedRevision=revision;return true;
}
function renderScenes(data) {
  if(!observeRevision(data.revision))return;
  sceneSnapshotEpoch++;lastSceneSnapshot=data;
  list.replaceChildren();
  for (const scene of data.scenes) {
    const row = document.createElement('article');
    const title = document.createElement('h2'); title.textContent = scene.name;
    const stateful=scene.switch_kind==='selection';
    const unavailable=scene.selection_state?.state==='unavailable';
    const selected=unavailable?'不可用':scene.selected===true?'开启':scene.selected===false?'关闭':'未知';
    const source=scene.selection_source==='inferred'?(scene.observation_current===false?'按键推断已失效':'按键推断'):scene.selection_source==='unknown'?'来源未知':scene.selection_source==='light_initialization'?'灯光初始化记录':scene.selection_source==='physical_feedback'?'本次连接灯光反馈':'本地选择意图';
    const relative=stateful && !unavailable && scene.selected==null && scene.relative_unknown_allowed===true;
    const hint=unavailable?selectionUnavailableMessage(scene.selection_state.value):scene.pending_relative?'切换执行中，等待读回。':relative?'需要点亮的未知灯会翻转一次，结果由读回确认。':scene.selection_state?.state==='unknown'?'尚未建立有效选择，可明确开启场景。':'';
    const historyHint=scene.selection_source==='physical_feedback'?`历史选择：${scene.historical_selected===true?'开启':scene.historical_selected===false?'关闭':'未知'}；`:'';
    const info = document.createElement('p'); info.textContent = `${stateful ? `当前选择：${selected}（${source}，不代表负载到位）；${hint}${historyHint}` : ''}负载反馈：${scene.activity}；${stateful?'最近开启执行':'最近执行'}：${scene.last_run?.status || '无记录'}`;
    if(scene.last_run) {
      const details=document.createElement('button');details.textContent='查看最近执行';
      const receipt=document.createElement('p');
      details.onclick=async()=>{
        const epoch=sessionEpoch;details.disabled=true;
        try {const run=await request('/api/v1/runs/'+encodeURIComponent(scene.last_run.boot)+'/'+encodeURIComponent(scene.last_run.run));receipt.textContent=`结果：${run.status}；配置版本：${run.revision}；记录版本：${run.version}；开始／更新：${run.started_ms}／${run.updated_ms} ms（该进程启动后的单调时间）。命令写出不代表物理动作完成。`;}
        catch(error){if(epoch===sessionEpoch)receipt.textContent=error.message;}
        finally{details.disabled=false;}
      };
      row.append(details,receipt);
    }
    const button = document.createElement('button'); button.textContent = relative?'相对切换场景':stateful ? (scene.selected===true?'关闭场景':'开启场景') : '执行场景'; button.disabled = !scene.can_control || sceneActionBusy || scene.pending_relative===true;
    button.onclick = async () => {
      if(sceneActionBusy || !scene.can_control || scene.pending_relative===true)return;
      sceneActionBusy=true;const epoch=sessionEpoch;
      if(lastSceneSnapshot)renderScenes(lastSceneSnapshot);
      try {
        const requestId = crypto.randomUUID();
        const body={scene: scene.id, request_id: requestId};
        if(stateful&&!relative)body.on=scene.selected!==true;
        const result = await request(relative?'/api/v1/scene-relative-actions':'/api/v1/scene-actions', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify(body)});
        if (result.outcome === 'accepted') started.set(scene.id, requestId);
        status.textContent = result.outcome === 'accepted' ? '请求已接受，请核对执行记录和设备反馈。' : result.outcome === 'unchanged' ? '场景选择已是目标值，本次未执行新的动作。' : '结果尚不明确，请核对记录；不要重复点击。';
        await refresh();
      } catch (error) { if(epoch===sessionEpoch)status.textContent = `${error.message}；未自动重试。`; }
      finally {if(epoch===sessionEpoch){sceneActionBusy=false;if(lastSceneSnapshot)renderScenes(lastSceneSnapshot);}}
    };
    const cancel = document.createElement('button'); cancel.textContent = '取消剩余动作'; cancel.disabled = !scene.can_control || !started.has(scene.id) || sceneActionBusy;
    cancel.onclick = async () => {
      if(sceneActionBusy || !scene.can_control || !started.has(scene.id))return;
      sceneActionBusy=true;const epoch=sessionEpoch;
      if(lastSceneSnapshot)renderScenes(lastSceneSnapshot);
      try {
        const result = await request('/api/v1/scene-cancellations', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({start_request_id: started.get(scene.id), request_id: crypto.randomUUID()})});
        status.textContent = result.outcome === 'cancelled' ? '剩余动作已取消；已执行动作不回滚。' : '取消结果未知，请核对记录。';
        started.delete(scene.id); await refresh();
      } catch (error) { if(epoch===sessionEpoch)status.textContent = error.message; }
      finally {if(epoch===sessionEpoch){sceneActionBusy=false;if(lastSceneSnapshot)renderScenes(lastSceneSnapshot);}}
    };
    const history=document.createElement('button');history.textContent='执行历史';history.disabled=!data.revision;
    history.onclick=()=>loadSceneHistory(scene.id,data.revision);
    row.append(title, info, button, cancel,history); list.append(row);
  }
}
document.getElementById('connect').onclick = async () => {
  if(sessionBusy)return;sessionBusy=true;clearLocalSession();const epoch=sessionEpoch;
  const credential = document.getElementById('token').value; document.getElementById('token').value = '';
  try {
    const response = await fetch('/api/v1/session', {method: 'POST', credentials: 'same-origin', headers: {Authorization: `Bearer ${credential}`}});
    if (!response.ok) throw new Error('登录失败');
    announceSessionChange();
    const session=await response.json();if(epoch!==sessionEpoch)return;
    csrf = session.csrf;pendingLogoutCsrf=''; eventCursor=null; await refresh(); if(epoch!==sessionEpoch)return; connectEvents(); status.textContent = '已连接（会话 15 分钟有效）';
  } catch (error) { if(epoch===sessionEpoch){clearLocalSession();status.textContent = error.message;} }
  finally {sessionBusy=false;}
};
function clearLocalSession() {
  ++sessionEpoch;clearModules();deviceCards.clear();sceneActionBusy=false;clearCredentialCreation();invalidatePreview();
  csrf = ''; observedRevision=null;lastSceneSnapshot=null; clearTimeout(eventRetry); if(eventSocket)eventSocket.close();eventDevices.clear(); selectedRoom='';document.getElementById('room-filter').replaceChildren();document.getElementById('room-filter').value=''; document.getElementById('draft-impact').textContent=''; document.getElementById('configuration').hidden=true;document.getElementById('diagnostics').hidden=true;historyEpoch++;detailEpoch++;document.getElementById('run-details').replaceChildren();document.getElementById('scene-history').hidden=true;document.getElementById('history-runs').replaceChildren();document.getElementById('history-title').textContent='';document.getElementById('history-next').disabled=true; document.getElementById('runtime-result').textContent='';document.getElementById('calendar-result').textContent='';document.getElementById('credential-list').textContent='';document.getElementById('revoke-credential-id').value='';document.getElementById('revoke-credential-confirm').checked=false;document.getElementById('revoke-credential-result').textContent='';document.getElementById('backup-result').textContent='';document.getElementById('backup-actions').replaceChildren();backupInspectionEpoch++;document.getElementById('backup-inspection').textContent='';document.getElementById('versions').replaceChildren();document.getElementById('draft-list').replaceChildren(); versionsBefore=null;document.getElementById('more-versions').disabled=true; metadataSource=null;clearCalibration();document.getElementById('binding-enabled').checked=false;for(const id of ['binding-module-id','binding-gateway','binding-address','binding-channel'])document.getElementById(id).value='';document.getElementById('metadata-device').replaceChildren();document.getElementById('metadata-room').replaceChildren();document.getElementById('edit-room-select').replaceChildren();document.getElementById('metadata-name').value=''; for(const id of ['device-document','automation-document','draft-id','base-revision','draft-revision','edit-room-id','edit-room-name','edit-room-floor']) document.getElementById(id).value=''; started.clear(); list.replaceChildren(); document.getElementById('devices').replaceChildren();
}
document.getElementById('logout').onclick = async () => {
  if(sessionBusy)return;sessionBusy=true;
  const logoutCsrf=csrf||pendingLogoutCsrf;pendingLogoutCsrf=logoutCsrf;clearLocalSession();announceSessionChange();status.textContent='本机内容已清除，正在注销服务端会话…';
  try {
    const response=await fetch('/api/v1/session',{method:'DELETE',cache:'no-store',credentials:'same-origin',headers:{'X-CSRF-Token':logoutCsrf}});
    if(!response.ok && response.status!==401)throw new Error('服务端注销未确认');
    pendingLogoutCsrf='';status.textContent='已退出';
  }catch(error){status.textContent='本机内容已清除；服务端注销未确认，请恢复网络后重试退出。';}
  finally {sessionBusy=false;}
};

async function refreshDevices() {
  const epoch=++deviceSnapshotEpoch;
  const data = await request('/api/v1/devices');
  if(epoch!==deviceSnapshotEpoch)return;
  const revision=data.revision_text ?? (Number.isSafeInteger(data.revision)?String(data.revision):null);
  if(revision===null || !observeRevision(revision))return;
  eventDevices.clear(); for(const device of data.devices) eventDevices.set(device.id,device);
  renderDevices();
}
document.getElementById('room-filter').onchange=()=>{
  selectedRoom=document.getElementById('room-filter').value;renderDevices();
};
function createDeviceCard(device) {
  const card={device,row:document.createElement('article'),title:document.createElement('h2'),info:document.createElement('p'),buttons:[],busy:false,target:null};
  if(device.room) {card.location=document.createElement('p');card.row.append(card.location);}
  card.row.append(card.title,card.info);
  const current=()=>deviceCards.get(device.id)===card;
  const send=async command=>{
    if(!current() || !card.device.can_control || (card.busy && card.device.kind!=='Motor'))return;
    const epoch=sessionEpoch;
    if(card.device.kind!=='Motor')card.busy=true;
    renderDevices();
    try {
      const result=await request('/api/v1/device-commands',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({device:device.id,request_id:crypto.randomUUID(),command})});
      if(epoch!==sessionEpoch || !current())return;
      if(command.action==='brightness' && card.target)card.target.phase=result.outcome==='accepted'?'submitted':'uncertain';
      status.textContent=result.outcome==='accepted'?(card.device.kind==='Motor'?'动作已接受；请观察实际移动和停止，位置仍未知。':'控制已接受，等待设备反馈。'):'结果未知，请核对设备；不要重复点击。';
      await refreshDevices();
    } catch(error) {
      if(epoch!==sessionEpoch || !current())return;
      if(command.action==='brightness' && card.target)card.target.phase='uncertain';
      status.textContent=`${error.message}；未自动重试。`;
    } finally {
      if(epoch===sessionEpoch && current()) {card.busy=false;renderDevices();}
    }
  };
  const button=(label,command)=>{
    const node=document.createElement('button');node.textContent=label;
    node.onclick=()=>send(command);card.buttons.push(node);card.row.append(node);
  };
  if(device.kind==='Motor') {
    for(const [action,label] of [['open','打开'],['close','关闭'],['stop','停止']])button(label,{action});
  }
  if(device.kind==='Switch'||device.kind==='Dimmer') {
    for(const on of [true,false])button(on?'开':'关',{action:'power',on});
  }
  if(device.kind==='Dimmer') {
    card.slider=document.createElement('input');card.slider.type='range';card.slider.min='0';card.slider.max='100';card.slider.step='1';
    card.slider.setAttribute('aria-label',device.name+'亮度');
    card.value=document.createElement('output');card.value.className='brightness-value';
    card.slider.oninput=()=>{
      if(!current() || card.slider.disabled)return;
      card.target={level:Number(card.slider.value),phase:'editing'};renderDevices();
    };
    card.slider.onchange=async()=>{
      if(!current() || card.slider.disabled)return;
      const level=Number(card.slider.value);
      if(!Number.isInteger(level)||level<0||level>100)return;
      card.target={level,phase:'sending'};
      await send({action:'brightness',level});
    };
    card.row.append(card.slider,card.value);
  }
  return card;
}
function renderDevices() {
  const filter=document.getElementById('room-filter');
  const rooms=new Map();let unassigned=false;
  for(const device of eventDevices.values()) {
    if(device.room)rooms.set('room:'+device.room.id,device.room);else unassigned=true;
  }
  if(selectedRoom && selectedRoom!=='none' && !rooms.has(selectedRoom))selectedRoom='';
  if(selectedRoom==='none'&&!unassigned)selectedRoom='';
  filter.replaceChildren();
  const add=(value,label)=>{const option=document.createElement('option');option.value=value;option.textContent=label;filter.append(option);};
  add('','全部房间');
  for(const [id,room] of [...rooms.entries()].sort((a,b)=>a[0].localeCompare(b[0])))add(id,[room.floor,room.name].filter(Boolean).join(' · '));
  if(unassigned)add('none','未分配房间');filter.value=selectedRoom;
  for(const id of deviceCards.keys())if(!eventDevices.has(id))deviceCards.delete(id);
  const rows=[];
  for(const device of eventDevices.values()) {
    let card=deviceCards.get(device.id);
    if(!card || card.device.kind!==device.kind || !!card.location!==!!device.room) {
      card=createDeviceCard(device);deviceCards.set(device.id,card);
    }
    card.device=device;card.title.textContent=device.name;
    if(card.location)card.location.textContent=[device.room.floor,device.room.name].filter(Boolean).join(' · ');
    const reported=Object.hasOwn(device,'reported')?device.reported:device.observed;
    card.info.textContent=reported==null?'状态未知':`${reported.on?'开':'关'}${reported.level===undefined?'':' · '+reported.level+'%'}${device.state_authority==='homesense_setting'?' · HomeSense设定':''}`;
    if(device.state_authority==='homesense_setting' && device.observed?.level!==undefined && reported?.level!==device.observed.level)card.info.textContent+=` · 读回 ${device.observed.level}%`;
    card.info.textContent+=` · ${device.quality}${device.pending===null?'':' · 等待反馈'}`;
    if(device.kind==='Motor')card.info.textContent='位置未知 · '+(device.motor_calibrated?'方向和停止已校准':'尚未校准');
    for(const button of card.buttons)button.disabled=!device.can_control || card.busy;
    if(card.slider) {
      if(card.target?.phase==='submitted' && device.pending===null && reported?.level===card.target.level)card.target=null;
      const level=card.target?.level ?? reported?.level;
      card.slider.value=level ?? 0;card.slider.disabled=!device.can_control || card.busy;
      card.slider.setAttribute('aria-label',device.name+'亮度');
      const suffix=card.target?.phase==='uncertain'?'（结果待核对）':card.target && card.target.phase!=='editing'?'（待确认）':'';
      card.value.textContent=level===undefined?'亮度未知':`${card.target?'目标亮度':'亮度'} ${level}%${suffix}`;
      card.slider.setAttribute('aria-valuetext',card.value.textContent);
    }
    if(!selectedRoom || selectedRoom===(device.room?'room:'+device.room.id:'none'))rows.push(card.row);
  }
  // Keep the native slider attached during live feedback so touch dragging is not cancelled.
  const devices=document.getElementById('devices');
  if(devices.children.length!==rows.length || rows.some((row,index)=>devices.children[index]!==row))devices.replaceChildren(...rows);
}

document.getElementById('refresh').onclick = () => {
  const epoch=sessionEpoch;
  return refresh().catch(error => {if(epoch===sessionEpoch)status.textContent=error.message;});
};

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
    renderMetadata(draft.devices,draft.id);
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
    const labels={added:'新增',removed:'移除',metadata:'名称／房间',enabled:'启用',disabled:'禁用',binding:'物理绑定',supply_relay:'供电继电器',default_on_level:'默认开启亮度',motor_calibration:'电机校准',gateway_or_module:'网关／模块'};
    const deviceChanges=(impact.device_changes||[]).map(item=>`${item.device}：${item.changes.map(change=>labels[change]||change).join('、')}`).join('\n')||'无设备字段变化';
    document.getElementById('draft-impact').textContent=`版本 ${impact.active_revision} → ${impact.candidate_revision}\n需要重建网关连接：${impact.connection_rebuild_required?'是':'否'}\n发布将结束旧版本规则和定时任务。\n设备变化：\n${deviceChanges}\n受影响场景：\n` + (impact.interrupted_runs.map(run=>`${run.definition || run.audit_id} · ${run.status}`).join('\n') || '当前无未结束场景') + '\n此为当前快照，不锁定运行状态；尚未激活。';
  } catch(error) {if(epoch===previewEpoch){invalidatePreview();status.textContent=error.message;}}
};

document.getElementById('activate-draft').onclick = async () => {
  const preview=publicationPreview;
  if(!preview || preview.id!==document.getElementById('draft-id').value.trim()) {status.textContent='请先预览当前草稿。';return;}
  publicationConfirmation={preview,epoch:previewEpoch};
  document.getElementById('publication-confirmation').hidden=false;
  document.getElementById('publication-confirmation-detail').textContent=document.getElementById('draft-impact').textContent;
};
document.getElementById('cancel-publication').onclick=()=>{
  publicationConfirmation=null;document.getElementById('publication-confirmation').hidden=true;
};
document.getElementById('confirm-publication').onclick=async()=>{
  const confirmation=publicationConfirmation;
  if(!confirmation || confirmation.epoch!==previewEpoch || confirmation.preview!==publicationPreview || confirmation.preview.id!==document.getElementById('draft-id').value.trim()) {invalidatePreview();status.textContent='确认已失效，请重新预览。';return;}
  const preview=confirmation.preview,epoch=sessionEpoch;
  invalidatePreview();
  try {
    const result=await request('/api/v1/config/activations/'+preview.id,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({request_id:crypto.randomUUID(),boot:preview.boot})});
    status.textContent=result.outcome==='activated'?`版本 ${result.revision} 已激活。`:'发布结果未知，请检查活动版本，不要重复激活。';
    await refresh();
  } catch(error) {if(epoch===sessionEpoch)status.textContent=error.message+'；发布可能已发生，请核对活动版本，未自动重试。';}
};

let eventSocket=null, eventCursor=null, eventRetry=null, eventAttempts=0;
const eventDevices=new Map();
function markEventStateUnavailable(message,reason='runtime_unavailable') {
  deviceSnapshotEpoch++;sceneSnapshotEpoch++;deviceCards.clear();invalidatePreview();
  for(const [id,device] of eventDevices)eventDevices.set(id,{...device,quality:'Stale',can_control:false});
  renderDevices();
  if(lastSceneSnapshot)renderScenes({...lastSceneSnapshot,scenes:lastSceneSnapshot.scenes.map(scene=>({...scene,selected:null,selection_state:{state:'unavailable',value:reason},observation_current:scene.selection_source==='inferred'?false:scene.observation_current,activity:'Unknown',can_control:false}))});
  status.textContent=message;
}
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
        const reason=frame.selection_state?.state==='unavailable'?frame.selection_state.value:'runtime_unavailable';
        markEventStateUnavailable(selectionUnavailableMessage(reason),reason);return;
      }
      if(frame.type!=='state')return;
      if(!observeRevision(frame.revision))return;
      deviceSnapshotEpoch++;
      if(frame.reset)eventDevices.clear();
      for(const device of frame.devices)eventDevices.set(device.id,device);
      renderDevices();renderScenes({scenes:frame.scenes,revision:frame.revision});eventCursor=frame.cursor;eventAttempts=0;
    } catch(error){status.textContent='事件更新失败，请刷新状态。';socket.close();}
  };
  socket.onclose=()=>{
    if(!current())return;
    eventCursor=null;
    // A revoked/expired session must clear private UI, while network failures retry.
    request('/api/v1/session').catch(()=>{});
    markEventStateUnavailable('事件连接已断开，正在重新连接；设备状态待确认。');
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
      const restore=document.createElement('button');restore.textContent='创建恢复草稿';restore.disabled=version.automation_bytes===null;
      restore.onclick=async()=>{
        const revision=document.getElementById('draft-revision').value.trim();
        if(!/^[1-9][0-9]{0,18}$/.test(revision)||BigInt(revision)>9223372036854775807n||BigInt(revision)<=BigInt(page.active_revision)){status.textContent='请先在新版本输入框填写大于活动版本的整数';return;}
        restore.disabled=true;const epoch=invalidatePreview();
        try {
          const result=await request(`/api/v1/config/versions/${version.revision}/restore-draft`,{method:'POST',body:JSON.stringify({id:crypto.randomUUID(),expected_revision:page.active_revision,revision})});
          if(epoch!==previewEpoch)return;
          await loadDraft(result.draft);
        } catch(error) {if(epoch===previewEpoch)status.textContent=error.message;}
        finally {restore.disabled=false;}
      };
      row.append(text,download,restore);list.append(row);
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
      const load=document.createElement('button');load.textContent='载入此草稿';load.onclick=()=>loadDraft(draft.id);const archive=document.createElement('button');archive.textContent='归档';archive.onclick=async()=>{
        const epoch=sessionEpoch;archive.disabled=true;
        try {await request('/api/v1/config/drafts/'+encodeURIComponent(draft.id)+'/archive',{method:'POST'});if(document.getElementById('draft-id').value===draft.id){invalidatePreview();document.getElementById('draft-id').value='';}row.remove();status.textContent='草稿已归档；已受理的发布不受影响。';}
        catch(e){if(epoch===sessionEpoch){status.textContent=e.message;archive.disabled=false;}}
      };row.append(label,load,archive);list.append(row);
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

function dailyBackupLabel(report) {
  const phases={unavailable:'尚无任务状态',waiting:'等待检查',checking:'正在检查',complete:'本轮完成',failed:'失败（设备服务继续运行）',stopped:'维护任务已停止'};
  const failures={clock:'无法确定 UTC 日期',directory:'备份目录不可用',snapshot_creation:'创建快照失败',snapshot_verification:'快照未通过校验（保留原文件）',retention:'保留清理失败',worker:'备份工作任务失败'};
  const phase=report&&Object.prototype.hasOwnProperty.call(phases,report.phase)?phases[report.phase]:phases.unavailable;
  const date=value=>Number.isSafeInteger(value)&&value>=0&&value<=100000000?new Date(value*86400000).toISOString().slice(0,10):'未记录';
  const failure=report&&Object.prototype.hasOwnProperty.call(failures,report.failure)?failures[report.failure]:null;
  const created=report?.created===true?'新建':report?.created===false?'复用已校验文件':'未确认';
  return `每日备份：${phase}${failure?'；'+failure:''}\n最近检查日期（UTC）：${date(report?.attempted_utc_day)}；最近成功日期（UTC）：${date(report?.last_success_utc_day)}；本轮文件：${created}`;
}

document.getElementById('runtime-status').onclick = async () => {
  const epoch=sessionEpoch;
  const output=document.getElementById('runtime-result');output.textContent='读取中…';
  try {
    const s=await request('/api/v1/diagnostics');
    const hapLabels={unknown:'未知',disabled:'未启用',listening:'监听中（不代表已配对或客户端可达）',degraded:'已降级（查看各桥状态）',reconfiguring:'正在重建'};
    const hap=Object.prototype.hasOwnProperty.call(hapLabels,s.hap)?hapLabels[s.hap]:'未知';
    const bridges=Object.entries(s.hap_bridges||{}).map(([id,status])=>`${id}：${status==='listening'?'监听中':status==='degraded'?'已降级':'未知'}`).join('；');
    output.textContent=`Apple 家庭入口：${hap}${bridges?'\n各桥：'+bridges:''}\n${dailyBackupLabel(s.daily_backup)}\n控制准入就绪：${s.ready}\n配置版本：${s.revision}\n传输代次：${s.transport_epoch}\n排队：${s.queued}；正在写出：${s.in_flight}\n网关已同步：${s.gateway_ready}\n存储健康：${s.storage_healthy}\n准入失败：${s.failed}；停止中：${s.stopping}；发布中：${s.publishing}\n这是读取时刻的准入状态，不证明设备已到达目标状态。`;
  } catch(error) {if(epoch===sessionEpoch)output.textContent=error.message;}
};

function executionLabel(value) {
  const labels={Pending:'等待执行',Running:'正在执行',CommandsWritten:'命令已写出（非到位确认）',Partial:'部分执行／可能已有输出',Cancelled:'已取消剩余动作',Failed:'执行失败',Interrupted:'运行中断（不自动重放）',Accepted:'已接受',Writing:'正在写出',Written:'已写出（非到位确认）',Observed:'已观测到匹配反馈',Uncertain:'执行结果不确定',ManualOverride:'人工控制接管',SceneReplacement:'被后续场景覆盖',CancelRequested:'收到取消请求',ServiceStopped:'服务停止',ConfigurationPublished:'旧记录：配置已发布',TransportFailure:'传输故障',PreparationFailed:'动作准备失败',PublicationBarrier:'进入配置发布屏障（不代表提交成功）'};
  return Object.prototype.hasOwnProperty.call(labels,value)?`${labels[value]} · ${value}`:`未知状态／原因：${String(value)}`;
}

let historyEpoch=0,detailEpoch=0;
async function loadSceneHistory(scene,revision,before=null) {
  const epoch=++historyEpoch;detailEpoch++;document.getElementById('run-details').replaceChildren();
  const panel=document.getElementById('scene-history'),runs=document.getElementById('history-runs'),next=document.getElementById('history-next');
  panel.hidden=false;runs.replaceChildren();next.disabled=true;document.getElementById('history-title').textContent='读取执行历史…';
  try {
    let path='/api/v1/scene-history?scene='+encodeURIComponent(scene)+'&revision='+encodeURIComponent(revision)+'&limit=20';
    if(before)path+='&before_boot='+encodeURIComponent(before.boot)+'&before_run='+encodeURIComponent(before.run);
    const page=await request(path);if(epoch!==historyEpoch)return;
    document.getElementById('history-title').textContent=`${page.scene} · 配置版本 ${page.revision} · 按创建顺序倒序`;
    for(const run of page.runs){const item=document.createElement('p');item.textContent=`${executionLabel(run.status)} · ${run.boot} / ${run.run} · 开始 ${run.started_ms} ms · 更新 ${run.updated_ms} ms`;
      for(const [kind,label] of [['commands','命令记录'],['cancellations','取消记录'],['expiries','过期记录']]){const button=document.createElement('button');button.textContent=label;button.onclick=()=>loadRunDetails(run,revision,kind);item.append(button);}
      runs.append(item);}
    if(!page.runs.length)runs.textContent='没有运行记录';
    next.disabled=page.next_before===null;next.onclick=()=>loadSceneHistory(scene,revision,page.next_before);
  } catch(error){if(epoch===historyEpoch)document.getElementById('history-title').textContent=error.message;}
}

async function loadRunDetails(run,revision,kind,after='0') {
  const epoch=++detailEpoch,box=document.getElementById('run-details');box.replaceChildren();box.textContent='读取运行详情…';
  try {
    const page=await request(`/api/v1/runs/${encodeURIComponent(run.boot)}/${encodeURIComponent(run.run)}/details?revision=${encodeURIComponent(revision)}&kind=${kind}&after=${encodeURIComponent(after)}&limit=20`);
    if(epoch!==detailEpoch)return;box.replaceChildren();
    const title=document.createElement('p');title.textContent=`${page.scene} · ${executionLabel(page.status)} · ${page.redacted?'部分记录因设备权限隐藏':'当前页可见记录'}；写出不代表物理动作完成。`;box.append(title);
    for(const record of page[kind]) {const row=document.createElement('p');row.textContent=`设备 ${record.device} · ${record.status!==undefined?executionLabel(record.status):record.reason!==undefined?executionLabel(record.reason):'调度过期'}${record.step===undefined?'':' · 步骤 '+record.step}${record.command===undefined?'':' · 命令 '+record.command}`;box.append(row);}
    if(!page[kind].length){const empty=document.createElement('p');empty.textContent='当前页没有可见记录';box.append(empty);}
    if(page.next_after!==null){const next=document.createElement('button');next.textContent='下一页详情';next.onclick=()=>loadRunDetails(run,revision,kind,page.next_after);box.append(next);}
  } catch(error){if(epoch===detailEpoch)box.textContent=error.message;}
}

document.getElementById('backup-status').onclick = async () => {
  const epoch=sessionEpoch;
  const output=document.getElementById('backup-result');output.textContent='读取中…';
  try {
    const data=await request('/api/v1/backups');
    const actions=document.getElementById('backup-actions');actions.replaceChildren();backupInspectionEpoch++;document.getElementById('backup-inspection').textContent='';
    for(const group of data.groups)for(const item of group.entries)if(item.regular_file){
      const button=document.createElement('button');button.textContent=`校验 ${item.name}`;
      button.onclick=async()=>{
        const selectedEpoch=sessionEpoch,inspectionEpoch=++backupInspectionEpoch;button.disabled=true;
        const result=document.getElementById('backup-inspection');result.textContent='校验中…';
        try {const value=await request('/api/v1/backups/inspect',{method:'POST',body:JSON.stringify({kind:group.kind,name:item.name})});if(inspectionEpoch!==backupInspectionEpoch)return;result.textContent=`${value.name}：校验通过\nSchema ${value.schema} · 活动版本 ${value.active_revision??'无'}\n此结果不代表历史版本全部通过语义校验，也不执行恢复。`;}
        catch(e){if(selectedEpoch===sessionEpoch && inspectionEpoch===backupInspectionEpoch)result.textContent=e.message;}
        finally {if(selectedEpoch===sessionEpoch)button.disabled=false;}
      };actions.append(button);
    }
    output.textContent=data.groups.map(group=>`${group.kind==='daily'?'每日备份':'发布前备份'}（未校验）\n`+(group.entries.length?group.entries.map(item=>`${item.name} · ${item.bytes} 字节 · ${item.regular_file?'普通文件':'非普通文件'}`).join('\n'):'暂无备份')).join('\n\n');
  } catch(e) {if(epoch===sessionEpoch)output.textContent=e.message;}
};

function clearCalibration() {
  calibrationBinding=null;
  for(const id of ['calibration-direction','calibration-evidence','calibration-time'])document.getElementById(id).value='';
  document.getElementById('calibration-stop').checked=false;
  document.getElementById('calibration-binding').textContent='';
}
async function saveCalibration(remove) {
  const epoch=invalidatePreview(),button=document.getElementById(remove?'remove-calibration':'save-calibration');button.disabled=true;
  try {
    if(!metadataSource || metadataSource!==document.getElementById('draft-id').value || !calibrationBinding || calibrationBinding.device!==document.getElementById('metadata-device').value)throw new Error('请先读取此草稿并选择设备。');
    if(calibrationBinding.gateway!==document.getElementById('binding-gateway').value || calibrationBinding.address!==document.getElementById('binding-address').value || String(calibrationBinding.channel)!==String(document.getElementById('binding-channel').value))throw new Error('物理绑定有未保存修改，请先生成并读取绑定草稿。');
    let calibration=null;
    if(!remove){
      const direction=document.getElementById('calibration-direction').value,evidence=document.getElementById('calibration-evidence').value,time=document.getElementById('calibration-time').value;
      if(!['up','down'].includes(direction) || !document.getElementById('calibration-stop').checked || !evidence.trim() || !/^[1-9][0-9]*$/.test(time))throw new Error('请完整填写方向、STOP 验证、证据和验证时间。');
      calibration={open_direction:direction,stop_verified:true,evidence,verified_at_unix_ms:time};
    }
    const body={id:crypto.randomUUID(),expected_revision:document.getElementById('base-revision').value,revision:document.getElementById('draft-revision').value,...calibrationBinding,calibration};
    const result=await request('/api/v1/config/drafts/'+encodeURIComponent(metadataSource)+(calibrationBinding.module_id?'/module-calibration':'/motor-calibration'),{method:'POST',body:JSON.stringify(body)});
    if(epoch!==previewEpoch)return;await loadDraft(result.draft);
  }catch(e){if(epoch===previewEpoch)status.textContent=e.message;}
  finally{button.disabled=false;}
}
document.getElementById('save-calibration').onclick=()=>saveCalibration(false);
document.getElementById('remove-calibration').onclick=()=>saveCalibration(true);

function renderMetadata(text,source) {
  metadataSource=source;renderReplacement(text,source);
  const config=JSON.parse(text),devices=document.getElementById('metadata-device'),rooms=document.getElementById('metadata-room');devices.replaceChildren();rooms.replaceChildren();
  for(const room of [{id:'',name:'未分配'},...(config.rooms||[])]){const option=document.createElement('option');option.value=room.id;option.textContent=room.name;rooms.append(option);}
  for(const device of config.devices){const option=document.createElement('option');option.value=device.id;option.textContent=device.name+' · '+device.id;devices.append(option);}
  devices.onchange=()=>{const device=config.devices.find(d=>d.id===devices.value);document.getElementById('metadata-name').value=device?.name||'';rooms.value=device?.room||'';
    document.getElementById('binding-module-id').value=device?.module_id||'';
    document.getElementById('binding-module-id').disabled=config.schema_version!==2;
    document.getElementById('binding-gateway').disabled=config.schema_version===2;document.getElementById('binding-address').disabled=config.schema_version===2;
    document.getElementById('binding-gateway').value=device?.gateway||'';
    document.getElementById('binding-address').value=device?.address||'';
    document.getElementById('binding-channel').value=device?.channel??'';
    document.getElementById('binding-enabled').checked=device?.enabled===true;
    clearCalibration();
    if(device){calibrationBinding=device.module_id?{device:device.id,module_id:device.module_id,channel:device.channel}:{device:device.id,gateway:device.gateway,address:device.address,channel:device.channel};document.getElementById('calibration-binding').textContent=`${device.module_id || (device.gateway+" / "+device.address)} / 通道 ${device.channel} · ${device.motor_calibration?'已有校准记录':'无校准记录'}`;}

  };
  devices.onchange();
  const editRooms=document.getElementById('edit-room-select');editRooms.replaceChildren();
  for(const room of [{id:'',name:'新增房间'},...(config.rooms||[])]){const option=document.createElement('option');option.value=room.id;option.textContent=room.name;editRooms.append(option);}
  editRooms.onchange=()=>{
    invalidatePreview();const room=(config.rooms||[]).find(r=>r.id===editRooms.value);
    document.getElementById('edit-room-id').value=room?.id||'';
    document.getElementById('edit-room-name').value=room?.name||'';
    document.getElementById('edit-room-floor').value=room?.floor||'';
  };editRooms.value='';editRooms.onchange();
}
document.getElementById('save-metadata').onclick=async()=>{
  const epoch=invalidatePreview(),button=document.getElementById('save-metadata');button.disabled=true;
  try {
    if(!metadataSource || metadataSource!==document.getElementById('draft-id').value)throw new Error('请先读取此草稿，再编辑设备。');
    const body={id:crypto.randomUUID(),expected_revision:document.getElementById('base-revision').value,revision:document.getElementById('draft-revision').value,device:document.getElementById('metadata-device').value,name:document.getElementById('metadata-name').value,room:document.getElementById('metadata-room').value||null};
    const result=await request('/api/v1/config/drafts/'+encodeURIComponent(document.getElementById('draft-id').value)+'/device-metadata',{method:'POST',body:JSON.stringify(body)});
    if(epoch!==previewEpoch)return;await loadDraft(result.draft);
  } catch(e){if(epoch===previewEpoch)status.textContent=e.message;}
  finally{button.disabled=false;}
};

document.getElementById('save-binding').onclick=async()=>{
  const epoch=invalidatePreview(),button=document.getElementById('save-binding');button.disabled=true;
  try {
    if(!metadataSource || metadataSource!==document.getElementById('draft-id').value)throw new Error('请先读取此草稿，再编辑设备。');
    const text=document.getElementById('binding-channel').value;
    if(!/^[1-9][0-9]*$/.test(text) || Number(text)>255)throw new Error('通道必须是 1–255 的整数，并符合模块能力。');
    const body={id:crypto.randomUUID(),expected_revision:document.getElementById('base-revision').value,revision:document.getElementById('draft-revision').value,device:document.getElementById('metadata-device').value,gateway:document.getElementById('binding-gateway').value,address:document.getElementById('binding-address').value,channel:Number(text),enabled:document.getElementById('binding-enabled').checked};
    const stable=document.getElementById('binding-module-id').value;
    const stableMode=!document.getElementById('binding-module-id').disabled;
    if(stableMode){if(!stable.trim())throw new Error('请选择模块逻辑ID。');body.module_id=stable;delete body.gateway;delete body.address;}
    const result=await request('/api/v1/config/drafts/'+encodeURIComponent(metadataSource)+(stableMode?'/module-binding':'/device-binding'),{method:'POST',body:JSON.stringify(body)});
    if(epoch!==previewEpoch)return;await loadDraft(result.draft);
  } catch(e){if(epoch===previewEpoch)status.textContent=e.message;}
  finally{button.disabled=false;}
};

async function saveRoom(remove) {
  const epoch=invalidatePreview(),button=document.getElementById(remove?'remove-room':'save-room');button.disabled=true;
  try {
    if(!metadataSource || metadataSource!==document.getElementById('draft-id').value)throw new Error('请先读取此草稿，再编辑房间。');
    const body={id:crypto.randomUUID(),expected_revision:document.getElementById('base-revision').value,revision:document.getElementById('draft-revision').value,remove,room:{id:document.getElementById('edit-room-id').value,name:document.getElementById('edit-room-name').value,floor:document.getElementById('edit-room-floor').value||null}};
    const result=await request('/api/v1/config/drafts/'+encodeURIComponent(metadataSource)+'/room',{method:'POST',body:JSON.stringify(body)});
    if(epoch!==previewEpoch)return;await loadDraft(result.draft);
  } catch(e){if(epoch===previewEpoch)status.textContent=e.message;}
  finally {button.disabled=false;}
};

document.getElementById('save-room').onclick=()=>saveRoom(false);
document.getElementById('remove-room').onclick=()=>saveRoom(true);

document.getElementById('read-credentials').onclick=async()=>{
  const epoch=sessionEpoch,output=document.getElementById('credential-list');
  output.textContent='读取中…';
  try {
    const result=await request('/api/v1/credentials');
    if(epoch!==sessionEpoch)return;
    const roles={Administrator:'管理员',Member:'成员',Viewer:'只读'};
    output.textContent=result.credentials.map(item=>`${item.id} · ${roles[item.grant.role]||item.grant.role}\n设备：${item.grant.devices.join('、')||'无'}\n场景：${item.grant.scenes.join('、')||'无'}`).join('\n\n')||'暂无凭证';
  }catch(e){if(epoch===sessionEpoch)output.textContent=e.message;}
};

// Keep revocation explicit: entering an ID alone never sends a mutation.
document.getElementById('revoke-credential').onclick=async()=>{
  const epoch=sessionEpoch,output=document.getElementById('revoke-credential-result'),button=document.getElementById('revoke-credential');
  const id=document.getElementById('revoke-credential-id').value.trim();
  if(!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)){output.textContent='请从授权清单复制完整凭证 ID';return;}
  if(!document.getElementById('revoke-credential-confirm').checked){output.textContent='请先确认撤销该凭证的所有会话';return;}
  button.disabled=true;output.textContent='正在撤销…';
  try {
    await request('/api/v1/credentials/'+encodeURIComponent(id)+'/revoke',{method:'POST'});
    if(epoch!==sessionEpoch)return;
    output.textContent='凭证已撤销；对应登录会话已失效';
    document.getElementById('revoke-credential-id').value='';document.getElementById('revoke-credential-confirm').checked=false;
    document.getElementById('credential-list').textContent='授权已变化，请重新读取清单';
  }catch(e){if(epoch===sessionEpoch)output.textContent=e.message;}
  finally {button.disabled=false;}
};

function clearCredentialCreation() {
  clearCredentialEdit();
  document.getElementById('issue-result').textContent='';
  document.getElementById('issue-role').value='Viewer';
  document.getElementById('issue-devices').value='';document.getElementById('issue-scenes').value='';
  document.getElementById('issue-confirm').checked=false;
}
document.getElementById('clear-issued-secret').onclick=()=>{document.getElementById('issue-result').textContent='';};
document.getElementById('issue-credential').onclick=async()=>{
  if(credentialIssueBusy)return;
  const epoch=sessionEpoch,output=document.getElementById('issue-result'),button=document.getElementById('issue-credential');
  if(!document.getElementById('issue-confirm').checked){output.textContent='请先核对并确认角色和范围';return;}
  const scope=id=>[...new Set(document.getElementById(id).value.split(/\r?\n/).map(value=>value.trim()).filter(Boolean))];
  const body={role:document.getElementById('issue-role').value,devices:scope('issue-devices'),scenes:scope('issue-scenes')};
  if(!['Viewer','Member','Administrator'].includes(body.role)||body.devices.length>256||body.scenes.length>4095||[...body.devices,...body.scenes].some(id=>id.length>256||/[\u0000-\u001f\u007f]/.test(id))) {output.textContent='角色或授权范围格式不正确';return;}
  const encoded=JSON.stringify(body);
  if(new TextEncoder().encode(encoded).length>65536){output.textContent='授权范围过大，请减少条目';return;}
  credentialIssueBusy=true;button.disabled=true;output.textContent='正在创建…';
  try {
    const issued=await request('/api/v1/credentials',{method:'POST',body:encoded});
    if(epoch!==sessionEpoch)return;
    output.textContent=`凭证 ID：${issued.id}\n令牌（仅显示一次）：${issued.secret}`;
    document.getElementById('issue-confirm').checked=false;
    document.getElementById('credential-list').textContent='授权已变化，请重新读取清单';
  }catch(e){if(epoch===sessionEpoch)output.textContent=e.message+'；若连接中断，创建可能已成功，请先读取清单核对，再决定是否撤销或重试。';}
  finally {credentialIssueBusy=false;button.disabled=false;}
};

function clearCredentialEdit() {
  credentialEditSource=null;credentialEditEpoch++;
  for(const id of ['edit-credential-id','edit-credential-devices','edit-credential-scenes'])document.getElementById(id).value='';
  document.getElementById('edit-credential-role').value='Viewer';
  document.getElementById('edit-credential-confirm').checked=false;
  document.getElementById('edit-credential-result').textContent='';
}
for(const id of ['edit-credential-id','edit-credential-role','edit-credential-devices','edit-credential-scenes'])document.getElementById(id).addEventListener('input',()=>{
  credentialEditEpoch++;document.getElementById('edit-credential-confirm').checked=false;
  if(id==='edit-credential-id')credentialEditSource=null;
});
document.getElementById('load-credential').onclick=async()=>{
  const epoch=sessionEpoch,editEpoch=++credentialEditEpoch,id=document.getElementById('edit-credential-id').value.trim(),output=document.getElementById('edit-credential-result');
  credentialEditSource=null;document.getElementById('edit-credential-confirm').checked=false;
  try {
    const result=await request('/api/v1/credentials');
    if(epoch!==sessionEpoch||editEpoch!==credentialEditEpoch)return;
    const credential=result.credentials.find(item=>item.id===id);if(!credential)throw new Error('未找到此凭证，请核对清单');
    document.getElementById('edit-credential-role').value=credential.grant.role;
    document.getElementById('edit-credential-devices').value=credential.grant.devices.join('\n');
    document.getElementById('edit-credential-scenes').value=credential.grant.scenes.join('\n');
    credentialEditSource={id,grant:credential.grant};output.textContent='已读取，请核对后修改；保存会替换完整范围';
  }catch(e){if(epoch===sessionEpoch&&editEpoch===credentialEditEpoch)output.textContent=e.message;}
};
document.getElementById('save-credential').onclick=async()=>{
  if(credentialEditBusy)return;
  const epoch=sessionEpoch,editEpoch=credentialEditEpoch,output=document.getElementById('edit-credential-result'),button=document.getElementById('save-credential');
  const id=document.getElementById('edit-credential-id').value.trim();
  if(!credentialEditSource||credentialEditSource.id!==id){output.textContent='请先读取此凭证的原授权';return;}
  if(!document.getElementById('edit-credential-confirm').checked){output.textContent='请确认替换完整范围并退出已有会话';return;}
  const scope=id=>[...new Set(document.getElementById(id).value.split(/\r?\n/).map(value=>value.trim()).filter(Boolean))];
  const body={expected:credentialEditSource.grant,role:document.getElementById('edit-credential-role').value,devices:scope('edit-credential-devices'),scenes:scope('edit-credential-scenes')};
  const encoded=JSON.stringify(body);
  if(new TextEncoder().encode(encoded).length>65536){output.textContent='授权范围过大';return;}
  credentialEditBusy=true;button.disabled=true;
  try {
    await request('/api/v1/credentials/'+encodeURIComponent(id),{method:'PATCH',body:encoded});
    if(epoch!==sessionEpoch)return;
    if(editEpoch===credentialEditEpoch){credentialEditSource=null;document.getElementById('edit-credential-confirm').checked=false;output.textContent='授权已更新，对应会话需重新登录；继续编辑前请重新读取';}
    document.getElementById('credential-list').textContent='授权已变化，请重新读取清单';
  }catch(e){if(epoch===sessionEpoch&&editEpoch===credentialEditEpoch)output.textContent=e.message+'；请重新读取授权核对结果，未自动重试。';}
  finally{credentialEditBusy=false;button.disabled=false;}
};

// Cookie replacement is origin-wide; do not leave other tabs with obsolete CSRF/UI state.
let sessionChannel=null;
try {
  if(typeof BroadcastChannel!=='undefined') {
    sessionChannel=new BroadcastChannel('homesense-session');
    sessionChannel.onmessage=event=>{
      if(event.data!=='session-changed')return;
      clearLocalSession();pendingLogoutCsrf='';
      status.textContent='其他标签页已更改登录状态，请在本页重新登录。';
    };
  }
}catch(_) { /* HTTP authorization remains authoritative when messaging is unavailable. */ }
function announceSessionChange() {
  try {sessionChannel?.postMessage('session-changed');}catch(_) {}
}


function clearModules() {
  modulePage=null;moduleReadEpoch++;replacementSource=null;
  document.getElementById('modules').hidden=true;
  document.getElementById('module-list').replaceChildren();document.getElementById('module-status').textContent='';document.getElementById('next-modules').disabled=true;
  document.getElementById('replacement-module').replaceChildren();document.getElementById('replacement-impact').textContent='';
  for(const id of ['replacement-address','replacement-profile','replacement-evidence','replacement-time'])document.getElementById(id).value='';
  document.getElementById('replacement-verification').value='pending';
}
function renderModules(page) {
  if(!observeRevision(page.revision))return;
  modulePage=page;const renderEpoch=sessionEpoch,list=document.getElementById('module-list');list.replaceChildren();
  const quality={confirmed:'新鲜反馈',stale:'过期反馈',unknown:'未知反馈',disabled:'禁用',unavailable:'运行时不可用',unsupported_feedback:'不支持状态读回'};
  for(const module of page.modules) {
    const row=document.createElement('article'),title=document.createElement('h3'),info=document.createElement('p');
    title.textContent=`${module.module_id||"旧配置：需迁移模块ID"} · ${module.gateway} / 0x${module.address} · ${module.profile}`;
    info.textContent=`${module.kind}；验证：${module.verification}；启用绑定回复：${module.response}（${module.fresh_responses}/${module.queryable_bindings}）；完整通道反馈见下方槽位`;
    row.append(title,info);
    for(const slot of module.slots) {
      const line=document.createElement('p'),binding=slot.binding;
      line.textContent=`通道 ${slot.channel}：${binding?binding.name+' · '+binding.id:'未绑定（不可控制）'}；${quality[slot.quality]||slot.quality||'未知'}；模块反馈 ${slot.actual?JSON.stringify(slot.actual):'未知'}；时间 ${slot.observed_ms??'未知'} ms；${binding?.supply_for.length?'供电关联 '+binding.supply_for.join(', '):''}`;
      row.append(line);
      if(!binding)continue;
      const history=document.createElement('p');history.textContent='本次启动命令记录：'+(page.commands||[]).filter(command=>command.device===binding.id).map(command=>`#${command.command} ${JSON.stringify(command.requested)} ${command.status} @${command.updated_ms}ms`).join('；');row.append(history);
      for(const action of binding.actions) {
        if(action==='brightness') {
          const input=document.createElement('input');input.type='number';input.min='0';input.max='100';input.step='1';input.setAttribute('aria-label',binding.name+'目标亮度');
          const button=document.createElement('button');button.textContent='设置亮度';button.disabled=!page.ready;
          button.onclick=()=>{try{return moduleDeviceAction(button,binding.id,{action:'brightness',level:parseModuleLevel(input.value)},page.revision,module,slot.channel,renderEpoch);}catch(error){document.getElementById('module-status').textContent=error.message;}};row.append(input,button);
        }else {
          for(const command of action==='power'?[{action:'power',on:true},{action:'power',on:false}]:[{action}]) {
            const button=document.createElement('button');button.textContent=command.action==='power'?(command.on?'打开':'关闭'):{open:'打开',close:'关闭',stop:'停止'}[command.action];button.disabled=!page.ready;
            button.onclick=()=>moduleDeviceAction(button,binding.id,command,page.revision,module,slot.channel,renderEpoch);row.append(button);
          }
        }
      }
    }
    list.append(row);
  }
  document.getElementById('module-status').textContent=`配置 ${page.revision}；连接代次 ${page.transport_epoch}；读取 ${page.captured_ms} ms。写出与反馈分别核对。`;
  document.getElementById('next-modules').disabled=page.next_offset==null;
}
function parseModuleLevel(value) {
  if(!/^(0|[1-9][0-9]?)$|^100$/.test(value))throw new Error('亮度必须为0–100的整数');return Number(value);
}
async function readModules(next=false) {
  const epoch=++moduleReadEpoch;
  try {
    const suffix=next&&modulePage?`?offset=${modulePage.next_offset}&limit=32&revision=${modulePage.revision}`:'';
    const page=await request('/api/v1/modules'+suffix);if(epoch!==moduleReadEpoch)return;renderModules(page);
  }catch(error){if(epoch===moduleReadEpoch){modulePage=null;document.getElementById('module-list').replaceChildren();document.getElementById('module-status').textContent=error.message;document.getElementById('next-modules').disabled=true;}}
}
async function moduleDeviceAction(button,device,command,revision,module,channel,renderEpoch) {
  if(button.disabled || renderEpoch!==sessionEpoch)return;const epoch=sessionEpoch;button.disabled=true;
  try {const result=await request('/api/v1/module-commands',{method:'POST',body:JSON.stringify({command,revision,module_id:module.module_id,channel,request_id:crypto.randomUUID()})});if(epoch===sessionEpoch)document.getElementById('module-status').textContent=JSON.stringify(result)+'；已接受／写出不代表状态反馈已确认。';}
  catch(error){if(epoch===sessionEpoch)document.getElementById('module-status').textContent=error.message+'；未自动重试。';}
  finally{if(epoch===sessionEpoch)button.disabled=false;}
}
document.getElementById('read-modules').onclick=()=>readModules();
document.getElementById('next-modules').onclick=()=>modulePage?.next_offset!=null?readModules(true):undefined;
function renderReplacement(text,source) {
  const config=JSON.parse(text),select=document.getElementById('replacement-module');select.replaceChildren();
  replacementSource={id:source,legacy:config.schema_version===1,modules:[]};
  for(const gateway of config.gateways||[])for(const module of gateway.modules){
    const key=replacementSource.modules.length;replacementSource.modules.push({module_id:module.id||null,gateway:gateway.id,address:module.address,profile:module.profile});const option=document.createElement('option');option.value=String(key);option.textContent=`${gateway.id} / 0x${module.address} · ${module.profile}`;select.append(option);
  }
  select.onchange=()=>{const selected=replacementSource.modules[Number(select.value)];document.getElementById('replacement-profile').value=selected?.profile||'';};
  select.value='0';select.onchange();
  document.getElementById('replacement-address').value='';document.getElementById('replacement-verification').value='pending';document.getElementById('replacement-evidence').value='';document.getElementById('replacement-time').value='';
}
document.getElementById('replace-module').onclick=async()=>{
  const button=document.getElementById('replace-module');if(button.disabled)return;const epoch=invalidatePreview(),session=sessionEpoch;button.disabled=true;
  try {
    if(!replacementSource || replacementSource.id!==document.getElementById('draft-id').value)throw new Error('请先读取源草稿。');
    const module=replacementSource.modules[Number(document.getElementById('replacement-module').value)],address=document.getElementById('replacement-address').value;
    if(!module?.module_id)throw new Error('请先迁移并保存稳定模块ID。');
    if(!/^[0-9a-fA-F]{2}$/.test(address) || address==='00')throw new Error('新地址必须是非零两位十六进制。');
    const verification={status:document.getElementById('replacement-verification').value};
    if(verification.status==='verified'){verification.evidence=document.getElementById('replacement-evidence').value;verification.verified_at_unix_ms=document.getElementById('replacement-time').value;if(!verification.evidence.trim() || !/^[1-9][0-9]*$/.test(verification.verified_at_unix_ms))throw new Error('请填写新模块证据及验证时间。');}
    const body={id:crypto.randomUUID(),expected_revision:document.getElementById('base-revision').value,revision:document.getElementById('draft-revision').value,module_id:module.module_id,new_address:address,profile:document.getElementById('replacement-profile').value,verification};
    const result=await request('/api/v1/config/drafts/'+encodeURIComponent(replacementSource.id)+'/module-replacement',{method:'POST',body:JSON.stringify(body)});if(epoch!==previewEpoch)return;
    await loadDraft(result.draft);document.getElementById('replacement-impact').textContent=JSON.stringify(result.impact,null,2)+'\n当前配置和硬件未改变；请继续预览发布影响。';
  }catch(error){if(epoch===previewEpoch)document.getElementById('replacement-impact').textContent=error.message+'；未自动重试。';}
  finally{if(session===sessionEpoch)button.disabled=false;}
};

document.getElementById('migrate-modules').onclick=async()=>{
  const button=document.getElementById('migrate-modules');if(button.disabled)return;const epoch=invalidatePreview(),session=sessionEpoch;button.disabled=true;
  try {
    if(!replacementSource?.legacy || replacementSource.id!==document.getElementById('draft-id').value)throw new Error('请先读取旧格式草稿。');
    const body={id:crypto.randomUUID(),expected_revision:document.getElementById('base-revision').value,revision:document.getElementById('draft-revision').value};
    const result=await request('/api/v1/config/drafts/'+encodeURIComponent(replacementSource.id)+'/module-migration',{method:'POST',body:JSON.stringify(body)});if(epoch!==previewEpoch)return;await loadDraft(result.draft);
    document.getElementById('replacement-impact').textContent=JSON.stringify(result.impact,null,2)+'\n模块ID已保存到新草稿；当前运行配置未改变。';
  }catch(error){if(epoch===previewEpoch)document.getElementById('replacement-impact').textContent=error.message+'；未自动重试。';}
  finally{if(session===sessionEpoch)button.disabled=false;}
};

// Shipped scene UI: explicit targets, in-flight locks, stale state and sessions.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const make=()=>({value:'',textContent:'',children:[],disabled:false,addEventListener(){},setAttribute(){},append(...v){this.children.push(...v)},replaceChildren(...v){this.children=v;this.textContent=''}});
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,make());return elements.get(id)};
const pending=[],requests=[];let sequence=0;
const context=vm.createContext({document:{getElementById:get,createElement:make},crypto:{randomUUID:()=>`request-${++sequence}`},clearTimeout(){},fetch:(path,options)=>{requests.push({path,options});return new Promise(resolve=>pending.push({path,options,resolve}))}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
vm.runInContext("csrf='synthetic'",context);
const scene=(id,selected,control=true)=>({id,name:id,switch_kind:'selection',selected,selection_state:selected===null?{state:'unknown'}:{state:'known',value:selected},activity:'Unknown',last_run:null,can_control:control});
const snapshot=(selected='daily')=>({revision:'1',scenes:[scene('reading',selected==='reading'),scene('daily',selected==='daily'),scene('eco',selected==='eco'),{id:'off',name:'off',switch_kind:'momentary',selected:null,activity:'Unknown',last_run:null,can_control:true}]});
const rows=()=>get('scenes').children,button=index=>rows()[index].children.find(x=>['开启场景','关闭场景','执行场景','相对切换场景'].includes(x.textContent));
const cancel=index=>rows()[index].children.find(x=>x.textContent==='取消剩余动作');
const respond=(result,status=200)=>{const p=pending.shift();assert(p);p.resolve({ok:status<400,status,json:async()=>result});return p};
const drain=()=>new Promise(resolve=>setImmediate(resolve));
async function refreshReply(snapshot) {
 await drain();assert.equal(pending[0].path,'/api/v1/session');respond({can_manage_configuration:false,can_view_diagnostics:false});
 await drain();assert.equal(pending[0].path,'/api/v1/scenes');respond(snapshot);
 await drain();assert.equal(pending[0].path,'/api/v1/devices');respond({revision:'1',devices:[],rooms:[]});await drain();
}
(async()=>{
 context.renderScenes({revision:'1',scenes:[{...scene('reading',null),selection_source:'physical_feedback',historical_selected:true}]});assert.match(rows()[0].children[1].textContent,/当前选择：未知.*本次连接灯光反馈.*历史选择：开启/);
 context.renderScenes({revision:'1',scenes:[{...scene('reading',true),selection_source:'light_initialization'}]});assert.match(rows()[0].children[1].textContent,/当前选择：开启.*灯光初始化记录/);
 context.renderScenes({revision:'1',scenes:[{...scene('reading',true),selection_source:'inferred',observation_current:true}]});assert.match(rows()[0].children[1].textContent,/当前选择：开启.*按键推断/);
 context.markEventStateUnavailable('stale');assert.match(rows()[0].children[1].textContent,/当前选择：不可用.*按键推断已失效/);assert.equal(button(0).disabled,true);
 context.renderScenes({revision:'1',scenes:[{...scene('reading',null),selection_source:'unknown'}]});assert.match(rows()[0].children[1].textContent,/当前选择：未知.*来源未知/);
 assert.match(rows()[0].children[1].textContent,/尚未建立有效选择，可明确开启场景/);assert.equal(button(0).disabled,false);
 for(const [reason,text] of [['cache_expired','过期'],['read_timed_out','超时'],['runtime_not_ready','未就绪'],['read_failed','读取失败']]) {
  context.markEventStateUnavailable(context.selectionUnavailableMessage(reason),reason);assert.match(rows()[0].children[1].textContent,/当前选择：不可用/);assert(rows()[0].children[1].textContent.includes(text));assert.equal(button(0).disabled,true);
 }
 context.renderScenes(snapshot());assert.equal(button(1).textContent,'关闭场景');assert.equal(button(0).textContent,'开启场景');assert.equal(button(3).textContent,'执行场景');assert.match(rows()[1].children[1].textContent,/当前选择：开启.*不代表负载到位/);
 const old=button(1),close=old.onclick();assert.equal(pending.length,1);assert.equal(JSON.parse(pending[0].options.body).on,false);
 // Incoming events cannot reopen scene controls while an accepted write is pending.
 context.renderScenes(snapshot('eco'));assert(rows().every((_,i)=>button(i).disabled));await old.onclick();await button(0).onclick();assert.equal(pending.length,1);
 respond({outcome:'accepted'},202);await refreshReply(snapshot(null));await close;assert(rows().every((_,i)=>!button(i).disabled));assert.equal(button(1).textContent,'开启场景');assert.equal(cancel(1).disabled,false);
 const cancelled=cancel(1).onclick();context.renderScenes(snapshot(null));assert.equal(cancel(1).disabled,true);assert(rows().every((_,i)=>button(i).disabled));assert.equal(pending[0].path,'/api/v1/scene-cancellations');respond({outcome:'cancelled'},202);await refreshReply(snapshot(null));await cancelled;assert.equal(cancel(1).disabled,true);
 context.renderScenes({revision:'1',scenes:[scene('reading',null)]});assert.match(rows()[0].children[1].textContent,/当前选择：未知/);assert.equal(button(0).textContent,'开启场景');
 const start=button(0).onclick();assert.equal(JSON.parse(pending[0].options.body).on,true);respond({outcome:'unchanged',retry:false},202);await refreshReply(snapshot('reading'));await start;assert.match(get('status').textContent,/未执行新的动作/);
 context.markEventStateUnavailable('stale');assert.equal(button(0).disabled,true);assert.match(rows()[0].children[1].textContent,/当前选择：不可用/);assert.equal(vm.runInContext('lastSceneSnapshot.scenes[0].selected',context),null);
 context.renderScenes(snapshot(null));const momentary=button(3).onclick();assert.equal(Object.hasOwn(JSON.parse(pending[0].options.body),'on'),false);
 // A late write from an old session cannot alter new status or release its lock.
 context.clearLocalSession();vm.runInContext("csrf='new';sceneActionBusy=true",context);get('status').textContent='new session';respond({outcome:'accepted'},202);await momentary;assert.equal(get('status').textContent,'new session');assert.equal(vm.runInContext('started.size',context),0);assert.equal(vm.runInContext('sceneActionBusy',context),true);
 context.renderScenes(snapshot('reading'));vm.runInContext("sceneActionBusy=false",context);context.renderScenes(snapshot('reading'));
 const failed=context.request('/api/v1/scenes');respond({error:'scene_selection_unavailable',selection_state:{state:'unavailable',value:'read_timed_out'}},503);await assert.rejects(failed,/选择读取超时/);assert.equal(button(0).disabled,true);assert.equal(vm.runInContext('lastSceneSnapshot.scenes[0].selected',context),null);
 context.renderScenes({revision:'1',scenes:[scene('reading',null)]});assert.equal(button(0).disabled,false);assert.match(rows()[0].children[1].textContent,/尚未建立有效选择/);
 // Newer state wins over a delayed failure from an older read.
 const lateFailure=context.request('/api/v1/scenes');context.renderScenes(snapshot('eco'));respond({error:'scene_selection_unavailable',selection_state:{state:'unavailable',value:'read_failed'}},503);await assert.rejects(lateFailure,/选择读取失败/);assert.equal(vm.runInContext('lastSceneSnapshot.scenes[2].selected',context),true);assert.equal(button(2).disabled,false);
 const oldSessionFailure=context.request('/api/v1/scenes');context.clearLocalSession();vm.runInContext("csrf='latest'",context);context.renderScenes(snapshot('daily'));respond({error:'scene_selection_unavailable',selection_state:{state:'unavailable',value:'read_timed_out'}},503);await assert.rejects(oldSessionFailure,/会话已切换/);assert.equal(vm.runInContext('lastSceneSnapshot.scenes[1].selected',context),true);assert.equal(button(1).disabled,false);
 context.renderScenes({revision:'1',scenes:[{...scene('reading',null),relative_unknown_allowed:true}]});assert.equal(button(0).textContent,'相对切换场景');
 const relative=button(0).onclick();assert.equal(pending[0].path,'/api/v1/scene-relative-actions');assert.equal(Object.hasOwn(JSON.parse(pending[0].options.body),'on'),false);await button(0).onclick();assert.equal(pending.length,1);
 respond({outcome:'accepted',status:'pending',retry:false},202);await refreshReply({revision:'1',scenes:[{...scene('reading',null),relative_unknown_allowed:false,pending_relative:true}]});await relative;
 assert.equal(button(0).disabled,true);assert.match(rows()[0].children[1].textContent,/等待读回/);await button(0).onclick();assert.equal(pending.length,0);
 context.markEventStateUnavailable('disconnected');assert.equal(button(0).disabled,true);assert.match(rows()[0].children[1].textContent,/不可用/);
 assert.equal(requests.filter(r=>r.path==='/api/v1/scene-relative-actions').length,1);
 assert.equal(pending.length,0);assert.equal(requests.filter(r=>r.path==='/api/v1/scene-actions').length,3);
 const immediate={id:'instant',name:'Synthetic immediate',execution_kind:'immediate',history_available:false,switch_kind:'momentary',activity:'Unknown',last_run:null,can_control:true};
 const immediateSnapshot={revision:'1',scenes:[immediate]};
 context.renderScenes(immediateSnapshot);
 assert.match(rows()[0].children[1].textContent,/即时执行不创建定时运行记录/);
 assert.equal(rows()[0].children.find(x=>x.textContent==='执行历史').disabled,true);
 assert.equal(cancel(0).disabled,true);
 vm.runInContext("started.set('instant','stale-timeline')",context);
 const immediateAction=button(0).onclick();
 assert.equal(pending[0].path,'/api/v1/scene-actions');
 assert.equal(Object.hasOwn(JSON.parse(pending[0].options.body),'on'),false);
 respond({outcome:'accepted',execution_kind:'immediate',receipt:{commands:[1,2],unknown_branches:['synthetic-unknown']}},202);
 await refreshReply(immediateSnapshot);await immediateAction;
 assert.equal(vm.runInContext("started.has('instant')",context),false);
 assert.equal(cancel(0).disabled,true);
 assert.match(get('status').textContent,/2条命令已准入，1个未知条件分支已跳过/);
 assert.equal(pending.length,0);
 console.log('Scene targets, busy guards, cancellation, unknown/stale values, immediate receipts and session isolation: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

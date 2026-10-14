// Execute the shipped renderer through network loss and session recovery.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path');
const source=fs.readFileSync(process.argv[2]||path.join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8');
const flush=()=>new Promise(resolve=>setImmediate(resolve));
function harness() {
 const elements=new Map(),timers=new Map(),callbacks=[],listeners=new Map(),sockets=[],requests=[],channels=[];
 let timerId=0;
 const element=()=>({value:'',disabled:false,hidden:false,textContent:'',children:[],addEventListener(){},setAttribute(){},append(...v){this.children.push(...v)},replaceChildren(...v){this.children=v}});
 const get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
 const navigator={onLine:true};
 class Socket {constructor(){sockets.push(this)}close(){this.closed=true;this.onclose?.()}send(value){this.sent=value}}
 class Channel {constructor(){channels.push(this)}postMessage(){}}
 const addListener=(name,fn)=>{listeners.set(name,[...(listeners.get(name)||[]),fn])};
 const document={getElementById:get,createElement:element,addEventListener:addListener,visibilityState:'visible'};
 const context=vm.createContext({document,navigator,AbortController,WebSocket:Socket,BroadcastChannel:Channel,crypto:{randomUUID:()=> 'synthetic-request'},location:{origin:'https://localhost'},
  addEventListener:addListener,
  clearTimeout:id=>timers.delete(id),setTimeout:(fn,delay)=>{callbacks.push(fn);timers.set(++timerId,{fn,delay});return timerId},
  fetch:(url,options)=>new Promise((resolve,reject)=>{
   const item={url,options,resolve,reject};requests.push(item);
   options.signal?.addEventListener('abort',()=>{if(!item.ignoreAbort)reject(new Error('aborted'))},{once:true});
  })
 });
 vm.runInContext(source,context);
 const run=code=>vm.runInContext(code,context);
 const event=name=>{for(const fn of listeners.get(name)||[])fn()};
 const fire=id=>{const timer=timers.get(id);assert.ok(timer,'expected active timer');timers.delete(id);timer.fn()};
 const onlyTimer=()=>{assert.equal(timers.size,1);return timers.keys().next().value};
 const ok=(index=requests.length-1,body={})=>requests[index].resolve({ok:true,status:200,json:async()=>body});
 const frame=(on=false,reset=true)=>({type:'state',reset,revision:'1',cursor:{sequence:2},devices:[{id:'light',name:'Light',kind:'Dimmer',quality:'Confirmed',pending:null,observed:{on,level:on?50:0},reported:{on,level:on?50:0},state_authority:'feedback',can_control:true}],scenes:[]});
 const seed=()=>{
  context.initial=frame(true);run("csrf='synthetic';eventCursor={sequence:1};for(const d of initial.devices)eventDevices.set(d.id,d);eventDevices.set('old-only',{id:'old-only',name:'Old',kind:'Switch',quality:'Confirmed',pending:null,observed:{on:true},can_control:true});renderDevices();connectEvents()");
  assert.ok(listeners.get('online')?.length,'online recovery listener must exist');
 };
 const unavailable=()=>{assert.equal(run("eventDevices.get('light').quality"),'Stale');assert.equal(run("eventDevices.get('light').can_control"),false);const row=get('devices').children[0];assert.equal(row.children[2].disabled,true);assert.equal(row.children[3].disabled,true)};
 const recovered=()=>{assert.equal(run("eventDevices.get('light').observed.on"),false);assert.equal(run("eventDevices.get('light').quality"),'Confirmed');assert.equal(run("eventDevices.get('light').can_control"),true);assert.equal(run("eventDevices.has('old-only')"),false);const row=get('devices').children[0];assert.equal(row.children[2].disabled,false);assert.equal(row.children[3].disabled,true)};
 const full=()=>{const socket=sockets.at(-1);socket.onopen();assert.equal(JSON.parse(socket.sent).cursor,null);socket.onmessage({data:JSON.stringify(frame())});recovered()};
 const readOnly=()=>{for(const r of requests){assert.equal(r.url,'/api/v1/session');assert.equal(r.options.method||'GET','GET');assert.equal(r.options.body,undefined)}};
 return {context,run,event,fire,onlyTimer,ok,frame,seed,unavailable,recovered,full,readOnly,get,navigator,document,listeners,timers,callbacks,sockets,requests,channels};
}
async function failedReads(h,count) {
 h.sockets.at(-1).onclose();h.unavailable();
 for(let i=0;i<count;i++) {h.fire(h.onlyTimer());h.requests.at(-1).reject(new Error('network unavailable'));await flush();h.unavailable()}
}
(async()=>{
 {
  const h=harness();h.seed();await failedReads(h,7);
  assert.ok([...h.timers.values()][0].delay<=10000,'slow retries stay capped');
  h.navigator.onLine=false;h.event('offline');h.unavailable();assert.equal(h.timers.size,0);
  const before=h.requests.length;h.event('pageshow');assert.equal(h.requests.length,before);
  h.navigator.onLine=true;h.event('online');h.unavailable();assert.equal(h.requests.length,before+1);h.ok();await flush();h.full();h.readOnly();
  console.log('Recovery after seven failures and explicit offline/online: full latest Off replaces old On, read-only');
 }
 {
  const h=harness();h.seed();await failedReads(h,7);
  h.fire(h.onlyTimer());h.ok();await flush();h.full();h.readOnly();
  console.log('Capped retries continue without an online event');
 }
 {
  const h=harness();h.seed();const old=h.sockets[0],oldMessage=old.onmessage,oldClose=old.onclose;
  h.event('online');const prior=h.requests.at(-1);let finishJson;
  prior.resolve({ok:true,status:200,json:()=>new Promise(resolve=>{finishJson=resolve})});await flush();
  h.navigator.onLine=false;h.event('offline');h.navigator.onLine=true;h.event('online');h.event('online');h.ok();await flush();h.full();
  const socketCount=h.sockets.length;finishJson({});await flush();oldMessage({data:JSON.stringify(h.frame(true))});oldClose();for(const fn of [...h.callbacks])fn();await flush();
  h.recovered();assert.equal(h.sockets.length,socketCount);h.readOnly();
  console.log('Repeated network changes, late JSON/socket/timer callbacks preserve the newest generation');
 }
 for(const reason of ['revoked','expired']) {
  const h=harness();h.seed();h.get('device-document').value='private';h.event('online');
  h.requests.at(-1).resolve({ok:false,status:401,json:async()=>{throw Error('401 must not require JSON')}});await flush();
  assert.equal(h.run('csrf'),'');assert.equal(h.run('eventDevices.size'),0);assert.equal(h.get('device-document').value,'');assert.equal(h.timers.size,0);
  const requests=h.requests.length,sockets=h.sockets.length;h.event('online');h.event('pageshow');for(const fn of h.callbacks)fn();await flush();assert.equal(h.requests.length,requests);assert.equal(h.sockets.length,sockets);h.readOnly();
  console.log(`${reason} session 401 clears private UI and stops automatic recovery`);
 }
 {
  const h=harness();h.seed();h.event('online');h.requests.at(-1).ignoreAbort=true;const probe=h.requests.at(-1);
  const logout=h.get('logout').onclick();h.ok();await logout;probe.resolve({ok:true,status:200,json:async()=>({})});await flush();
  const requests=h.requests.length,sockets=h.sockets.length;h.event('online');h.event('pageshow');for(const fn of h.callbacks)fn();await flush();assert.equal(h.requests.length,requests);assert.equal(h.sockets.length,sockets);assert.equal(h.run('eventDevices.size'),0);assert.equal(h.run('csrf'),'');
  assert.equal(h.requests.filter(r=>r.options.method==='DELETE').length,1,'only explicit logout mutates');
  console.log('Logout cancels queued recovery and ignores a late probe');
 }
 {
  const h=harness();h.seed();h.event('online');const old=h.requests.at(-1);old.ignoreAbort=true;
  h.channels[0].onmessage({data:'session-changed'});assert.equal(h.run('csrf'),'');h.run("csrf='new-session'");h.event('online');h.ok();await flush();h.full();
  old.resolve({ok:false,status:401,json:async()=>({})});await flush();h.recovered();assert.equal(h.run('csrf'),'new-session');h.readOnly();
  console.log('Cross-tab session switch fences an old 401 without clearing the new session');
 }
 {
  const h=harness();h.seed();const watchdog=h.onlyTimer();h.fire(watchdog);h.unavailable();h.fire(h.onlyTimer());
  assert.equal([...h.timers.values()][0].delay,5000);h.fire(h.onlyTimer());await flush();h.unavailable();
  h.fire(h.onlyTimer());h.ok();await flush();const socket=h.sockets.at(-1);socket.onopen();socket.onmessage({data:JSON.stringify(h.frame(false,false))});h.unavailable();
  h.fire(h.onlyTimer());h.ok();await flush();h.full();h.readOnly();
  console.log('Handshake/probe timeouts retry safely; a partial initial frame cannot restore controls');
 }
 {
  const h=harness();h.seed();h.full();const prior=h.sockets.at(-1);h.event('pageshow');h.unavailable();assert.equal(prior.closed,true);h.ok();await flush();h.full();h.readOnly();
  const before=h.requests.length;h.document.visibilityState='hidden';h.event('visibilitychange');assert.equal(h.requests.length,before);
  h.document.visibilityState='visible';h.event('visibilitychange');h.unavailable();h.ok();await flush();h.full();h.readOnly();
  console.log('pageshow/foreground replace a potentially half-open connection with a full snapshot');
 }
 {
  const h=harness();h.seed();const row=h.get('devices').children[0];const action=row.children[3].onclick();assert.equal(h.requests.length,1);assert.equal(h.requests[0].url,'/api/v1/device-commands');
  h.navigator.onLine=false;h.event('offline');h.requests[0].reject(new Error('lost control response'));await action;
  h.navigator.onLine=true;h.event('online');h.ok();await flush();h.full();
  assert.equal(h.requests.filter(r=>r.options.method==='POST').length,1,'the failed user command must never replay');for(const r of h.requests.slice(1)){assert.equal(r.url,'/api/v1/session');assert.equal(r.options.method||'GET','GET')}
  console.log('A lost user command response is never replayed during recovery');
 }
})().catch(error=>{console.error(error);process.exitCode=1});

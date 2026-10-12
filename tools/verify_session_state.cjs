// Offline late-response regression. No network or real credentials.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map();
const element=id=>{if(!elements.has(id))elements.set(id,{value:'',disabled:false,hidden:false,textContent:'',addEventListener(){},replaceChildren(){this.textContent='';}});return elements.get(id);};
const pending=[],sockets=[],channels=[];
class Channel {constructor(name){assert.equal(name,'homesense-session');channels.push(this);this.messages=[];}postMessage(value){this.messages.push(value)}}
class Socket {constructor(){sockets.push(this)}close(){this.onclose?.()}send(){}}
const context=vm.createContext({BroadcastChannel:Channel,document:{getElementById:element},fetch:(path,options)=>new Promise((resolve,reject)=>pending.push({path,options,resolve,reject})),clearTimeout(){},setTimeout(){throw Error('obsolete socket reconnected')},WebSocket:Socket,location:{origin:'http://127.0.0.1:1'}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 vm.runInContext("csrf='synthetic';connectEvents()",context);
 const oldSocket=sockets[0];const old=context.request('/api/v1/config/versions');const rejected=assert.rejects(old,/会话已切换/);
 const logout=element('logout').onclick();
 // Concurrent login cannot race the server's cookie replacement.
 await element('connect').onclick();assert.equal(pending.length,2);
 pending[1].resolve({ok:true,json:async()=>({logged_out:true})});await logout;
 assert.equal(element('configuration').hidden,true);assert.equal(element('diagnostics').hidden,true);
 pending[0].resolve({ok:true,json:async()=>({versions:[{revision:'private'}]})});await rejected;
 oldSocket.onmessage({data:JSON.stringify({type:'state',reset:true,devices:[{id:'old-device'}],scenes:[]})});oldSocket.onclose();
 assert.equal(vm.runInContext('eventDevices.size',context),0);
 assert.equal(element('versions').textContent,'');assert.equal(vm.runInContext('csrf',context),'');
 vm.runInContext("csrf='new-session';connectEvents()",context);
 element('issue-result').textContent='synthetic-secret';element('device-document').value='private-config';element('configuration').hidden=false;element('diagnostics').hidden=false;
 const unauthorized=context.request('/api/v1/devices');const expired=assert.rejects(unauthorized,/会话已失效/);pending[2].resolve({ok:false,status:401,json:async()=>{throw Error('malformed body')}});await expired;
 assert.equal(element('issue-result').textContent,'');assert.equal(element('device-document').value,'');assert.equal(element('configuration').hidden,true);assert.equal(element('diagnostics').hidden,true);assert.equal(vm.runInContext('csrf',context),'');
 element('device-document').value='previous-user';element('diagnostics').hidden=false;const failedLogin=element('connect').onclick();assert.equal(element('device-document').value,'');assert.equal(element('diagnostics').hidden,true);pending[3].resolve({ok:false,status:403});await failedLogin;assert.equal(vm.runInContext('csrf',context),'');
 vm.runInContext("csrf='still-valid-server-cookie'",context);element('device-document').value='private-again';element('diagnostics').hidden=false;const offlineLogout=element('logout').onclick();assert.equal(element('device-document').value,'');assert.equal(element('diagnostics').hidden,true);pending[4].reject(new Error('offline'));await offlineLogout;assert.match(element('status').textContent,/服务端注销未确认/);assert.equal(vm.runInContext('csrf',context),'');
 const retryLogout=element('logout').onclick();assert.equal(pending[5].options.headers['X-CSRF-Token'],'still-valid-server-cookie');pending[5].resolve({ok:true});await retryLogout;assert.equal(vm.runInContext('pendingLogoutCsrf',context),'');
 vm.runInContext("csrf='tab-session';pendingLogoutCsrf='obsolete';connectEvents()",context);
 element('device-document').value='other-tab-private';const priorEpoch=vm.runInContext('sessionEpoch',context);
 channels[0].onmessage({data:'unknown'});assert.equal(vm.runInContext('sessionEpoch',context),priorEpoch);
 channels[0].onmessage({data:'session-changed'});assert.equal(element('device-document').value,'');assert.equal(vm.runInContext('csrf',context),'');assert.equal(vm.runInContext('pendingLogoutCsrf',context),'');assert.match(element('status').textContent,/其他标签页/);
 assert.ok(channels[0].messages.length>0);assert.ok(channels[0].messages.every(value=>value==='session-changed'));
 let finishRefresh;context.refresh=()=>new Promise(resolve=>{finishRefresh=resolve;});
 const loginDuringChange=element('connect').onclick();pending.at(-1).resolve({ok:true,json:async()=>({csrf:'temporary'})});
 await new Promise(resolve=>setImmediate(resolve));const socketCount=sockets.length;
 channels[0].onmessage({data:'session-changed'});finishRefresh();await loginDuringChange;
 assert.equal(sockets.length,socketCount);assert.equal(vm.runInContext('csrf',context),'');assert.match(element('status').textContent,/其他标签页/);
 console.log('Late HTTP/WebSocket responses and overlapping session changes: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

// Offline late-response regression. No network or real credentials.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map();
const element=id=>{if(!elements.has(id))elements.set(id,{value:'',disabled:false,hidden:false,textContent:'',addEventListener(){},replaceChildren(){this.textContent='';}});return elements.get(id);};
const pending=[],sockets=[];
class Socket {constructor(){sockets.push(this)}close(){this.onclose?.()}send(){}}
const context=vm.createContext({document:{getElementById:element},fetch:path=>new Promise(resolve=>pending.push({path,resolve})),clearTimeout(){},setTimeout(){throw Error('obsolete socket reconnected')},WebSocket:Socket,location:{origin:'http://127.0.0.1:1'}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 vm.runInContext("csrf='synthetic';connectEvents()",context);
 const oldSocket=sockets[0];const old=context.request('/api/v1/config/versions');const rejected=assert.rejects(old,/会话已切换/);
 const logout=element('logout').onclick();
 // Concurrent login cannot race the server's cookie replacement.
 await element('connect').onclick();assert.equal(pending.length,2);
 pending[1].resolve({ok:true,json:async()=>({logged_out:true})});await logout;
 assert.equal(element('configuration').hidden,true);
 pending[0].resolve({ok:true,json:async()=>({versions:[{revision:'private'}]})});await rejected;
 oldSocket.onmessage({data:JSON.stringify({type:'state',reset:true,devices:[{id:'old-device'}],scenes:[]})});oldSocket.onclose();
 assert.equal(vm.runInContext('eventDevices.size',context),0);
 assert.equal(element('versions').textContent,'');assert.equal(vm.runInContext('csrf',context),'');
 console.log('Late HTTP/WebSocket responses and overlapping session changes: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

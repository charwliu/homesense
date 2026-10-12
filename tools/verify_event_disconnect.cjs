// Lost event streams must not leave stale devices controllable in the UI.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const make=()=>({value:'',textContent:'',children:[],addEventListener(){},setAttribute(){},append(...v){this.children.push(...v)},replaceChildren(){this.children=[]}}),elements=new Map();const get=id=>{if(!elements.has(id))elements.set(id,make());return elements.get(id)};
const sockets=[],timers=[];class Socket {constructor(){sockets.push(this)}close(){this.onclose?.()}send(value){this.sent=value}}
const context=vm.createContext({document:{getElementById:get,createElement:make},WebSocket:Socket,location:{origin:'http://localhost'},clearTimeout(){},setTimeout(fn){timers.push(fn)}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
vm.runInContext("csrf='fixture';eventCursor={sequence:50};eventDevices.set('light',{id:'light',name:'Light',kind:'Dimmer',quality:'Confirmed',observed:{on:true,level:50},can_control:true});publicationPreview={id:'draft'};connectEvents()",context);
sockets[0].onclose();assert.equal(vm.runInContext("eventDevices.get('light').quality",context),'Stale');assert.equal(vm.runInContext("eventDevices.get('light').can_control",context),false);assert.equal(vm.runInContext('publicationPreview',context),null);assert.equal(timers.length,1);
// A new authenticated full snapshot restores controls; an old socket cannot revoke them.
timers.shift()();sockets[1].onopen();assert.equal(JSON.parse(sockets[1].sent).cursor,null);sockets[1].onmessage({data:JSON.stringify({type:'state',revision:'1',reset:true,cursor:null,scenes:[],devices:[{id:'light',name:'Light',kind:'Dimmer',quality:'Confirmed',observed:{on:true,level:50},can_control:true}]})});
assert.equal(vm.runInContext("eventDevices.get('light').can_control",context),true);sockets[0].onclose?.();assert.equal(vm.runInContext("eventDevices.get('light').can_control",context),true);
console.log('Disconnect marks stale, revokes preview, and fresh reconnect restores: passed');

// HTTP device snapshots must not replace a newer published graph.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const make=()=>({value:'',textContent:'',children:[],addEventListener(){},append(...v){this.children.push(...v)},replaceChildren(){this.children=[]}}),elements=new Map();
const get=id=>{if(!elements.has(id))elements.set(id,make());return elements.get(id)};const pending=[];
const context=vm.createContext({document:{getElementById:get,createElement:make},fetch:()=>new Promise(resolve=>pending.push(resolve))});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 const response=(revision,name)=>({ok:true,json:async()=>({revision:Number(revision),revision_text:revision,devices:[{id:name,name,kind:'Keypad',quality:'Unknown',observed:null,can_control:false}]})});
 const old=context.refreshDevices(),latest=context.refreshDevices();pending[1](response('9007199254740994','new'));await latest;pending[0](response('9007199254740993','old'));await old;
 assert.equal(vm.runInContext("eventDevices.has('new')",context),true);assert.equal(vm.runInContext("eventDevices.has('old')",context),false);
 const ambiguous=context.refreshDevices();pending[2]({ok:true,json:async()=>({revision:9007199254740996,devices:[]})});await ambiguous;
 assert.equal(vm.runInContext('eventDevices.size',context),1);
 const beforeDisconnect=context.refreshDevices();context.markEventStateUnavailable('disconnected');pending[3](response('9007199254740994','late'));await beforeDisconnect;
 assert.equal(vm.runInContext("eventDevices.has('late')",context),false);assert.equal(vm.runInContext("eventDevices.get('new').quality",context),'Stale');
 const earlier=context.refreshDevices(),newer=context.refreshDevices();pending[5](response('9007199254740994','newest'));await newer;pending[4](response('9007199254740994','earlier'));await earlier;
 assert.equal(vm.runInContext("eventDevices.has('newest')",context),true);assert.equal(vm.runInContext("eventDevices.has('earlier')",context),false);
 console.log('HTTP refresh rejects old graphs, earlier requests and pre-disconnect replies: passed');
})().catch(e=>{console.error(e);process.exitCode=1});

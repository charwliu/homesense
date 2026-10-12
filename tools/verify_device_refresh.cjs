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
 console.log('HTTP refresh rejects old graphs and unsafe numeric revisions: passed');
})().catch(e=>{console.error(e);process.exitCode=1});

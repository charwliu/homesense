const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map(),pending=[];
const element=id=>{if(!elements.has(id))elements.set(id,{value:'',textContent:'',addEventListener(){}});return elements.get(id);};
const context=vm.createContext({document:{getElementById:element},fetch:()=>new Promise(resolve=>pending.push(resolve))});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 const read=async (hap,hap_bridges={})=>{const task=element('runtime-status').onclick();pending.shift()({ok:true,json:async()=>({hap,hap_bridges,ready:true,revision:'1',transport_epoch:'1',queued:0,in_flight:0,gateway_ready:true,storage_healthy:true})});await task;return element('runtime-result').textContent;};
 assert.match(await read('degraded'),/Apple 家庭入口：已降级（查看各桥状态）\n控制准入就绪：true/);
 assert.match(await read('degraded',{one:'listening',two:'degraded'}),/各桥：one：监听中；two：已降级/);
 assert.match(await read('listening'),/不代表已配对或客户端可达/);
 assert.match(await read('disabled'),/Apple 家庭入口：未启用/);
 assert.match(await read('toString'),/Apple 家庭入口：未知/);
 console.log('Channel lifecycle is distinct from control readiness and field reachability: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

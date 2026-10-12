const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map(),pending=[];
const element=id=>{if(!elements.has(id))elements.set(id,{value:'',textContent:'',addEventListener(){}});return elements.get(id);};
const context=vm.createContext({document:{getElementById:element},fetch:()=>new Promise(resolve=>pending.push(resolve))});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 const read=async (hap,hap_bridges={},daily_backup=undefined)=>{const task=element('runtime-status').onclick();pending.shift()({ok:true,json:async()=>({hap,hap_bridges,daily_backup,ready:true,revision:'1',transport_epoch:'1',queued:0,in_flight:0,gateway_ready:true,storage_healthy:true})});await task;return element('runtime-result').textContent;};
 assert.match(await read('degraded'),/Apple 家庭入口：已降级（查看各桥状态）[\s\S]*控制准入就绪：true/);
 assert.match(await read('degraded',{one:'listening',two:'degraded'}),/各桥：one：监听中；two：已降级/);
 assert.match(await read('listening'),/不代表已配对或客户端可达/);
 assert.match(await read('disabled'),/Apple 家庭入口：未启用/);
 assert.match(await read('toString'),/Apple 家庭入口：未知/);
 assert.match(await read('listening'),/每日备份：尚无任务状态/);
 const failed=await read('listening',{}, {phase:'failed',attempted_utc_day:10,last_success_utc_day:9,created:null,failure:'snapshot_verification'});
 assert.match(failed,/快照未通过校验（保留原文件）/);assert.match(failed,/最近成功日期（UTC）：1970-01-10/);assert.match(failed,/控制准入就绪：true/);assert.doesNotMatch(failed,/本轮完成/);
 assert.match(await read('listening',{}, {phase:'complete',attempted_utc_day:10,last_success_utc_day:10,created:false,failure:null}),/本轮文件：复用已校验文件/);
 assert.match(await read('listening',{}, {phase:'toString',attempted_utc_day:-1,created:true,failure:'toString'}),/每日备份：尚无任务状态/);
 console.log('Channel lifecycle is distinct from control readiness and field reachability: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map();const element=id=>{if(!elements.has(id))elements.set(id,{value:'',textContent:'',addEventListener(){}});return elements.get(id)};
const pending=[];const context=vm.createContext({document:{getElementById:element},fetch:(path)=>new Promise(resolve=>pending.push({path,resolve}))});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 const task=element('read-credentials').onclick();assert.equal(pending[0].path,'/api/v1/credentials');pending.shift().resolve({ok:true,json:async()=>({credentials:[{id:'fixture',grant:{role:'Member',devices:['lamp'],scenes:[]}}]})});await task;
 assert.equal(element('credential-list').textContent,'fixture · 成员\n设备：lamp\n场景：无');
 const late=element('read-credentials').onclick();vm.runInContext('sessionEpoch++',context);element('credential-list').textContent='';pending.shift().resolve({ok:true,json:async()=>({credentials:[]})});await late;assert.equal(element('credential-list').textContent,'');
 console.log('Credential listing and session fencing: passed');
})().catch(e=>{console.error(e);process.exitCode=1});

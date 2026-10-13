const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const make=()=>({value:'',children:[],disabled:false,textContent:'',addEventListener(){},setAttribute(){},append(...xs){this.children.push(...xs)},replaceChildren(...xs){this.children=xs}});
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,make());return elements.get(id)};
const requests=[],pending=[];let sequence=0;
const context=vm.createContext({document:{getElementById:get,createElement:make},crypto:{randomUUID:()=>`request-${++sequence}`},clearTimeout(){},fetch:(path,options)=>{requests.push({path,options});return new Promise(resolve=>pending.push(resolve))}});
vm.runInContext(fs.readFileSync('crates/homesensed/src/web/app.js','utf8'),context);
vm.runInContext("csrf='synthetic'",context);
const binding={id:'light',name:'Fixture',enabled:true,supply_for:[],actual:{level:30},quality:'confirmed',observed_ms:'10',actions:['brightness']};
const page={boot:'fixture',revision:'2',transport_epoch:'3',captured_ms:'11',ready:true,next_offset:null,commands:[{device:'light',command:'1',status:'accepted',requested:{kind:'level',level:40},updated_ms:'11'}],modules:[{module_id:'stable-fixture-module',gateway:'bus',address:'15',profile:'fixture',kind:'Dimmer',verification:'simulated',response:'fresh',fresh_responses:1,queryable_bindings:1,slots:[{channel:1,binding},{channel:2,binding:null}]}]};
(async()=>{
 context.renderModules(page);const row=get('module-list').children[0];assert.match(row.children[0].textContent,/stable-fixture-module/);assert(row.children.some(c=>c.textContent.includes('未绑定（不可控制）')));assert(row.children.some(c=>c.textContent.includes('accepted')));
 const input=row.children.find(c=>c.type==='number'),button=row.children.find(c=>c.textContent==='设置亮度');input.value='101';await button.onclick();assert.equal(requests.length,0);assert.match(get('module-status').textContent,/0–100/);
 input.value='35';const write=button.onclick();await button.onclick();assert.equal(requests.length,1);const body=JSON.parse(requests[0].options.body);assert.equal(requests[0].path,'/api/v1/module-commands');assert.equal(body.module_id,'stable-fixture-module');assert.equal(body.channel,1);assert.equal(body.revision,'2');for(const key of ['address','gateway','device'])assert.equal(Object.hasOwn(body,key),false);
 pending.shift()({ok:true,status:202,json:async()=>({outcome:'accepted',commands:[1]})});await write;context.clearLocalSession();assert.equal(get('module-list').children.length,0);assert.equal(get('replacement-module').children.length,0);assert.equal(get('modules').hidden,true);await button.onclick();assert.equal(requests.length,1);
 context.renderReplacement(JSON.stringify({schema_version:2,gateways:[{id:'bus',modules:[{id:'stable-fixture-module',address:'61',profile:'fixture'}]}]}),'source-fixture');assert.equal(get('replacement-module').children[0].textContent.includes('0x61'),true);
 console.log('Module physical observations, stable control targets, bounds, duplicate guard and retired sessions: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

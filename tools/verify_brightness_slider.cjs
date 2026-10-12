// Execute the shipped slider against old snapshots, accepted writes and lost sessions.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const element=()=>({value:'',textContent:'',children:[],attributes:{},disabled:false,addEventListener(){},setAttribute(k,v){this.attributes[k]=v},append(...v){this.children.push(...v)},replaceChildren(...v){this.children=v}});
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
const requests=[];
const context=vm.createContext({document:{getElementById:get,createElement:element},crypto:{randomUUID:()=>`fixture-${requests.length}`},clearTimeout(){},fetch:(path,options)=>new Promise(resolve=>requests.push({path,options,resolve}))});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
const base={id:'light',name:'Light',kind:'Dimmer',quality:'Confirmed',pending:null,can_control:true,state_authority:'homesense_setting'};
const device=level=>({...base,observed:{on:true,level},reported:{on:true,level}});
const render=d=>{context.input=d;vm.runInContext("eventDevices.clear();eventDevices.set(input.id,input);renderDevices()",context);return get('devices').children[0]};
const response=value=>({ok:true,status:200,json:async()=>value});
const flush=()=>new Promise(resolve=>setImmediate(resolve));
(async()=>{
 for(const [previous,next] of [[30,17],[17,30]]) {
  const row=render(device(previous)),slider=row.children[4];assert.equal(slider.type,'range');assert.equal(slider.min,'0');assert.equal(slider.max,'100');
  assert.equal(row.children.some(x=>x.textContent==='设置亮度'),false);
  const before=requests.length;
  slider.value=String(next);slider.oninput();slider.oninput();
  assert.equal(requests.length,before,'dragging must not send commands');
  assert.equal(render(device(previous)),row);assert.equal(row.children[4],slider,'live updates must not replace the active slider');assert.equal(Number(slider.value),next);
  const release=slider.onchange();assert.equal(requests.length,before+1);assert.equal(slider.disabled,true);
  assert.deepEqual(JSON.parse(requests[before].options.body).command,{action:'brightness',level:next});
  render({...device(previous),pending:{target:next}});assert.equal(Number(slider.value),next);
  requests[before].resolve({...response({outcome:'accepted'}),status:202});await flush();
  assert.equal(requests[before+1].path,'/api/v1/devices');
  requests[before+1].resolve(response({revision:1,revision_text:'1',devices:[device(previous)]}));await release;
  assert.equal(Number(slider.value),next,'accepted request followed by old recorded value must not pull the slider back');
  assert.match(row.children[5].textContent,/待确认/);assert.equal(slider.disabled,false);
  render(device(next));assert.equal(Number(slider.value),next);assert.doesNotMatch(row.children[5].textContent,/待确认/);
  render(device(previous));assert.equal(Number(slider.value),previous,'confirmed target must release the draft so new external state remains visible');
 }
 const old=render(device(17)).children[4];old.value='40';old.oninput();
 const count=requests.length;context.markEventStateUnavailable('fixture disconnect');render(device(17));await old.onchange();assert.equal(requests.length,count,'pre-disconnect gesture must not replay after recovery');
 const slider=render(device(17)).children[4];slider.value='30';slider.oninput();const failed=slider.onchange();requests[count].resolve({ok:false,status:503,json:async()=>({error:'fixture_unavailable'})});await failed;
 assert.equal(requests.length,count+1,'failure must not retry or request another command');assert.equal(Number(slider.value),30);assert.match(get('devices').children[0].children[5].textContent,/结果待核对/);
 context.clearLocalSession();assert.equal(get('devices').children.length,0);
 assert.equal(Number(render(device(17)).children[4].value),17,'logout must discard prior target');
 const viewer=render({...device(17),can_control:false}).children[4];assert.equal(viewer.disabled,true);await viewer.onchange();assert.equal(requests.length,count+1);
 render({...base,reported:null,observed:null});assert.match(get('devices').children[0].children[5].textContent,/亮度未知/);
 console.log('Slider: one command per release, stable drag/accepted target, confirmed feedback, no replay/retry and session/scope isolation: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

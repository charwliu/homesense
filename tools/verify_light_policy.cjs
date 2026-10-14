// Shipped Web renderer separates bounded local targets from current feedback.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const element=()=>({value:'',textContent:'',children:[],addEventListener(){},setAttribute(){},append(...items){this.children.push(...items)},replaceChildren(...items){this.children=items}});
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
const context=vm.createContext({document:{getElementById:get,createElement:element}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
const set=value=>{context.input=value;vm.runInContext('eventDevices.clear();eventDevices.set(input.id,input);renderDevices()',context);return get('devices').children[0]};
const dimmer={id:'lamp',name:'Fixture',kind:'Dimmer',quality:'Confirmed',pending:null,can_control:true,observed:{on:true,level:36},reported:{on:true,level:30},state_authority:'pending_target'};
let row=set(dimmer);assert.match(row.children[1].textContent,/开 · 30% · 调光目标 · 当前读回 36%/);assert.equal(row.children[4].value,30);
row=set({...dimmer,reported:null});assert.match(row.children[1].textContent,/状态未知/);assert.equal(row.children[4].type,'range');assert.equal(row.children[4].value,0);assert.match(row.children[5].textContent,/亮度未知/);
row=set({...dimmer,kind:'Switch',observed:{on:false},reported:{on:false},state_authority:'feedback'});assert.match(row.children[1].textContent,/关/);assert.doesNotMatch(row.children[1].textContent,/调光目标/);
row=set({...dimmer,observed:{on:true,level:17},reported:{on:true,level:17},state_authority:'feedback'});assert.match(row.children[1].textContent,/开 · 17%/);assert.doesNotMatch(row.children[1].textContent,/调光目标|当前读回/);assert.equal(row.children[4].value,17);
for(const kind of ['Dimmer','Switch']) {
 const off={...dimmer,kind,observed:{on:false,level:0},reported:{on:false,level:100},state_authority:'feedback'};
 row=set(off);assert.equal(row.children[2].disabled,false);assert.equal(row.children[3].disabled,true,'fresh actual Off disables only the power-Off button, regardless of restore brightness');
 for(const patch of [{quality:'Unknown'},{quality:'Stale'},{observed:null},{observed:{on:true,level:17}},{pending:{command:1}},{state_authority:'pending_target',reported:{on:true,level:30}}]) {
  row=set({...off,...patch});assert.equal(row.children[3].disabled,false,'unknown, stale, actual On or pending must not be inferred Off from displayed brightness');
 }
 row=set({...off,can_control:false});assert.equal(row.children[2].disabled,true);assert.equal(row.children[3].disabled,true,'permission gate remains authoritative');
 row=set(off);vm.runInContext("deviceCards.get('lamp').busy=true;renderDevices()",context);assert.equal(row.children[2].disabled,true);assert.equal(row.children[3].disabled,true);
 vm.runInContext("deviceCards.get('lamp').busy=false",context);
}
row=set({...dimmer,observed:{on:false,level:0},reported:{on:false,level:100},state_authority:'feedback'});
for(const phase of ['editing','sending','submitted','uncertain']) {
 context.phase=phase;vm.runInContext("deviceCards.get('lamp').target={level:30,phase};renderDevices()",context);assert.equal(row.children[3].disabled,false,'local unresolved target is not steady Off');
}
vm.runInContext("deviceCards.get('lamp').target=null;renderDevices()",context);assert.equal(row.children[3].disabled,true);
vm.runInContext("deviceCards.get('lamp').target={level:0,phase:'submitted'}",context);row=set({...dimmer,observed:{on:false,level:0},reported:{on:false,level:0},state_authority:'feedback'});assert.equal(row.children[3].disabled,true,'completed local Off must disable the button on the same render');
context.markEventStateUnavailable('fixture disconnect');row=get('devices').children[0];assert.equal(row.children[2].disabled,true);assert.equal(row.children[3].disabled,true,'disconnect disables all controls');
console.log('Bounded targets, steady actual feedback, Off-only button disable and pending/permission/busy/disconnect guards: passed');

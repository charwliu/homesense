// Shipped Web renderer separates bounded local targets from current feedback.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const element=()=>({value:'',textContent:'',children:[],addEventListener(){},setAttribute(){},append(...items){this.children.push(...items)},replaceChildren(...items){this.children=items}});
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
const context=vm.createContext({document:{getElementById:get,createElement:element}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
const set=value=>{context.input=value;vm.runInContext('eventDevices.clear();eventDevices.set(input.id,input);renderDevices()',context);return get('devices').children[0]};
const motor={id:'curtain',name:'Synthetic curtain',kind:'Motor',quality:'Unknown',pending:null,can_control:true,motor_calibrated:true,observed:null,reported:null};
for(const estimate of [null,{source:'inferred',movement:'Opening',position:null,percent_hundredths:3700,stop_led_active:true},{source:'inferred',movement:'Stopped',position:'Open',percent_hundredths:10000,stop_led_active:false},{source:'inferred',movement:'Stopped',position:'Intermediate',percent_hundredths:null,pending:'Stop',stop_led_active:true}]) {
 const row=set({...motor,motor_estimate:estimate});
 for(const button of row.children.slice(2))assert.equal(button.disabled,false,'motion, endpoint, unknown position and red/green hints never disable a manual motor action');
 if(estimate?.percent_hundredths===3700)assert.match(row.children[1].textContent,/估计 37%/);
 if(estimate?.position==='Intermediate') {assert.match(row.children[1].textContent,/估计中间位置/);assert.doesNotMatch(row.children[1].textContent,/50%/);}
 if(estimate?.stop_led_active===false)assert.match(row.children[1].textContent,/停止键活动提示：绿/);
}
const row=set({...motor,can_control:false});for(const button of row.children.slice(2))assert.equal(button.disabled,true);
set(motor);context.markEventStateUnavailable('fixture disconnect');for(const button of get('devices').children[0].children.slice(2))assert.equal(button.disabled,true,'event loss still disables control');
console.log('Curtain inference labels, manual repeat/stop and permission/disconnect guards: passed');

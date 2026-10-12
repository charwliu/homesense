// Shipped Web renderer keeps measured values distinct from recorded settings.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const element=()=>({value:'',textContent:'',children:[],addEventListener(){},setAttribute(){},append(...items){this.children.push(...items)},replaceChildren(...items){this.children=items}});
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
const context=vm.createContext({document:{getElementById:get,createElement:element}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
const set=value=>{context.input=value;vm.runInContext('eventDevices.clear();eventDevices.set(input.id,input);renderDevices()',context);return get('devices').children[0]};
const dimmer={id:'lamp',name:'Fixture',kind:'Dimmer',quality:'Confirmed',pending:null,can_control:true,observed:{on:true,level:36},reported:{on:true,level:30},state_authority:'homesense_setting'};
let row=set(dimmer);assert.match(row.children[1].textContent,/开 · 30% · HomeSense设定 · 读回 36%/);assert.equal(row.children[4].value,30);
row=set({...dimmer,reported:null});assert.match(row.children[1].textContent,/状态未知/);assert.equal(row.children[4].value,'');
row=set({...dimmer,kind:'Switch',observed:{on:false},reported:{on:false},state_authority:'feedback'});assert.match(row.children[1].textContent,/关/);assert.doesNotMatch(row.children[1].textContent,/HomeSense设定/);
console.log('Recorded dimmer settings, measured switch state, diagnostic mismatch and unknown projection: passed');

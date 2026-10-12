// Execute the shipped renderer with synthetic metadata only.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const element=()=>({value:'',textContent:'',children:[],addEventListener(){},append(...items){this.children.push(...items)},replaceChildren(...items){this.children=items}});
const elements=new Map();const get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
const context=vm.createContext({document:{getElementById:get,createElement:element}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
const set=devices=>{context.input=devices;vm.runInContext('eventDevices.clear();for(const d of input)eventDevices.set(d.id,d);renderDevices()',context)};
const base={observed:null,quality:'Unknown',pending:null,kind:'Keypad',can_control:false};
const first={...base,id:'a',name:'A',room:{id:'none',name:'<script>room</script>',floor:'F1'}};
const second={...base,id:'b',name:'B',room:{id:'b',name:'Room B'}};
const third={...base,id:'c',name:'C',room:null};
set([first,second,third]);assert.equal(get('devices').children.length,3);
get('room-filter').value='room:none';get('room-filter').onchange();assert.equal(get('devices').children.length,1);
assert.equal(get('devices').children[0].children[0].textContent,'F1 · <script>room</script>');
set([{...first,quality:'Confirmed'},second,third]);assert.equal(get('room-filter').value,'room:none');assert.equal(get('devices').children.length,1);
set([second,third]);assert.equal(get('room-filter').value,'');assert.equal(get('devices').children.length,2);
get('room-filter').value='none';get('room-filter').onchange();assert.equal(get('devices').children.length,1);
set([]);assert.equal(get('room-filter').value,'');assert.equal(get('devices').children.length,0);assert.equal(get('room-filter').children.length,1);
console.log('Room filter updates, removed scope, unassigned and text rendering: passed');

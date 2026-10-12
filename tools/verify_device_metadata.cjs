// Device metadata form sends narrow strings, never reserializes source numbers.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const make=()=>({value:'',textContent:'',children:[],handlers:{},addEventListener(event,handler){this.handlers[event]=handler},append(child){this.children.push(child);if(this.children.length===1)this.value=child.value},replaceChildren(){this.children=[];this.value=''}});
const elements=new Map(),element=id=>{if(!elements.has(id))elements.set(id,make());return elements.get(id)};
const pending=[];const context=vm.createContext({document:{getElementById:element,createElement:make},crypto:{randomUUID:()=> 'new-id'},fetch:(path,options)=>new Promise(resolve=>pending.push({path,options,resolve})),clearTimeout(){},setTimeout(){},location:{origin:'http://127.0.0.1:1'}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 const raw='{"revision":9007199254740993,"devices":[{"id":"light","name":"Original"}],"rooms":[{"id":"study","name":"Study"}]}';
 context.renderMetadata(raw,'source-id');assert.equal(element('metadata-name').value,'Original');element('metadata-name').value='New';element('metadata-room').value='study';element('base-revision').value='9007199254740993';element('draft-revision').value='9007199254740994';element('draft-id').value='source-id';element('device-document').value=raw;
 const done=element('save-metadata').onclick();assert.equal(pending[0].path,'/api/v1/config/drafts/source-id/device-metadata');assert.equal(pending[0].options.headers['Content-Type'],'application/json');
 assert.deepEqual(JSON.parse(pending[0].options.body),{id:'new-id',expected_revision:'9007199254740993',revision:'9007199254740994',device:'light',name:'New',room:'study'});
 pending.shift().resolve({ok:false,json:async()=>({error:'synthetic rejection'})});await done;assert.equal(element('device-document').value,raw);assert.equal(element('activate-draft').disabled,true);
 // An edited source ID must not silently apply old form values to another draft.
 element('draft-id').value='other-id';await element('save-metadata').onclick();assert.equal(pending.length,0);assert.match(element('status').textContent,/请先读取/);
 element('draft-id').value='source-id';const inflight=element('save-metadata').onclick();
 element('metadata-name').value='Later edit';element('metadata-name').handlers.input();
 pending.shift().resolve({ok:true,json:async()=>({draft:'returned-id'})});await inflight;
 assert.equal(pending.length,0,'stale completion must not load over current editing');assert.equal(element('metadata-name').value,'Later edit');
 element('edit-room-id').value='study';element('edit-room-name').value='Study';element('edit-room-floor').value='F2';
 const roomEdit=element('save-room').onclick();assert.equal(pending[0].path,'/api/v1/config/drafts/source-id/room');const roomBody=JSON.parse(pending[0].options.body);assert.equal(roomBody.revision,'9007199254740994');assert.deepEqual(roomBody.room,{id:'study',name:'Study',floor:'F2'});
 element('edit-room-floor').handlers.input();pending.shift().resolve({ok:true,json:async()=>({draft:'late-room-id'})});await roomEdit;assert.equal(pending.length,0);
 console.log('Device and room source binding, precision and in-flight edits: passed');
})().catch(e=>{console.error(e);process.exitCode=1});

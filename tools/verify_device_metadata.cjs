// Device metadata form sends narrow strings, never reserializes source numbers.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const make=()=>({value:'',textContent:'',children:[],addEventListener(){},append(child){this.children.push(child);if(this.children.length===1)this.value=child.value},replaceChildren(){this.children=[];this.value=''}});
const elements=new Map(),element=id=>{if(!elements.has(id))elements.set(id,make());return elements.get(id)};
const pending=[];const context=vm.createContext({document:{getElementById:element,createElement:make},crypto:{randomUUID:()=> 'new-id'},fetch:(path,options)=>new Promise(resolve=>pending.push({path,options,resolve})),clearTimeout(){},setTimeout(){},location:{origin:'http://127.0.0.1:1'}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 const raw='{"revision":9007199254740993,"devices":[{"id":"light","name":"Original"}],"rooms":[{"id":"study","name":"Study"}]}';
 context.renderMetadata(raw);assert.equal(element('metadata-name').value,'Original');element('metadata-name').value='New';element('metadata-room').value='study';element('base-revision').value='9007199254740993';element('draft-revision').value='9007199254740994';element('draft-id').value='source-id';element('device-document').value=raw;
 const done=element('save-metadata').onclick();assert.equal(pending[0].path,'/api/v1/config/drafts/source-id/device-metadata');
 assert.deepEqual(JSON.parse(pending[0].options.body),{id:'new-id',expected_revision:'9007199254740993',revision:'9007199254740994',device:'light',name:'New',room:'study'});
 pending.shift().resolve({ok:false,json:async()=>({error:'synthetic rejection'})});await done;assert.equal(element('device-document').value,raw);assert.equal(element('activate-draft').disabled,true);
 console.log('Metadata form preserves source and string revisions: passed');
})().catch(e=>{console.error(e);process.exitCode=1});

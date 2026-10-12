// Synthetic UI: archive invalidates the selected draft's publication preview.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const make=()=>({value:'',textContent:'',children:[],addEventListener(){},append(...items){this.children.push(...items)},replaceChildren(){this.children=[]},remove(){this.removed=true}});
const elements=new Map(),element=id=>{if(!elements.has(id))elements.set(id,make());return elements.get(id)};
const pending=[];
const context=vm.createContext({document:{getElementById:element,createElement:make},fetch:(path,options)=>new Promise(resolve=>pending.push({path,options,resolve})),clearTimeout(){},setTimeout(){},location:{origin:'http://127.0.0.1:1'}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 vm.runInContext("csrf='synthetic';publicationPreview={draft:'demo'}",context);
 element('draft-id').value='demo';
 const list=element('list-drafts').onclick();pending.shift().resolve({ok:true,json:async()=>({drafts:[{id:'demo',expected_revision:'1',revision:'2'}]})});await list;
 const row=element('draft-list').children[0],archive=row.children[2];const done=archive.onclick();
 assert.equal(pending[0].path,'/api/v1/config/drafts/demo/archive');assert.equal(pending[0].options.method,'POST');assert.equal(pending[0].options.body,undefined);
 pending.shift().resolve({ok:true,json:async()=>({archived:true})});await done;
 assert.equal(row.removed,true);assert.equal(element('draft-id').value,'');assert.equal(element('activate-draft').disabled,true);assert.equal(vm.runInContext('publicationPreview',context),null);
 console.log('Draft archive clears selected preview: passed');
})().catch(e=>{console.error(e);process.exitCode=1});

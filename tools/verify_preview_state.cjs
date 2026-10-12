// Offline UI state regression. No server, credentials, or physical controls.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map();
const element=id=>{if(!elements.has(id))elements.set(id,{value:'',disabled:false,textContent:'',listeners:{},addEventListener(name,handler){this.listeners[name]=handler;}});return elements.get(id);};
const pending=[];
const context=vm.createContext({document:{getElementById:element},fetch:(path)=>new Promise(resolve=>pending.push({path,resolve})),window:{confirm:()=>true},console});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
const preview=()=>element('preview-draft').onclick();
const reply=(index,id,ok=true)=>pending[index].resolve({ok,json:async()=>ok?{draft:id,impact:{boot:'test',active_revision:1,candidate_revision:2,connection_rebuild_required:false,interrupted_runs:[],device_changes:[{device:"fixture:motor",changes:["binding","motor_calibration"]}]}}:{error:'failed'}});
(async()=>{
 element('draft-id').value='a';let task=preview();reply(0,'a');await task;assert.equal(element('activate-draft').disabled,false);
 assert.match(element('draft-impact').textContent,/fixture:motor：物理绑定、电机校准/);
 // A failed refresh must revoke the previously successful preview.
 task=preview();assert.equal(element('activate-draft').disabled,true);reply(1,'a',false);await task;
 await element('activate-draft').onclick();assert.equal(pending.length,2);assert.equal(element('activate-draft').disabled,true);
 // A response arriving after an edit must not re-enable activation.
 task=preview();element('device-document').listeners.input();reply(2,'a');await task;assert.equal(element('activate-draft').disabled,true);
 // Out-of-order preview responses must not replace the newer selection.
 task=preview();element('draft-id').value='b';element('draft-id').listeners.input();let latest=preview();reply(4,'b');await latest;reply(3,'a');await task;
 assert.equal(vm.runInContext('publicationPreview.id',context),'b');
 // Selecting another draft immediately revokes capability, even before loading.
 element('draft-id').listeners.input();assert.equal(element('activate-draft').disabled,true);assert.equal(vm.runInContext('publicationPreview',context),null);
 // Accepted activation also removes the now-obsolete impact text.
 vm.runInContext('refresh=async()=>{}',context);context.crypto={randomUUID:()=>"fixture"};
 task=preview();reply(5,'b');await task;
 await element('activate-draft').onclick();assert.equal(pending.length,6);assert.equal(element('publication-confirmation').hidden,false);
 element('cancel-publication').onclick();await element('confirm-publication').onclick();assert.equal(pending.length,6);
 task=preview();reply(6,'b');await task;await element('activate-draft').onclick();element('metadata-name').listeners.input();await element('confirm-publication').onclick();assert.equal(pending.length,7);
 task=preview();reply(7,'b');await task;await element('activate-draft').onclick();
 const activated=element('confirm-publication').onclick();assert.equal(element('draft-impact').textContent,'');assert.equal(element('publication-confirmation').hidden,true);
 await element('confirm-publication').onclick();assert.equal(pending.length,9,'double confirmation must not resubmit');
 pending[8].resolve({ok:true,json:async()=>({outcome:'activated',revision:2})});await activated;
 assert.equal(element('activate-draft').disabled,true);
 // External publication must revoke confirmation, including >2^53 versions.
 context.observeRevision('9007199254740993');
 vm.runInContext("publicationPreview={id:'b',boot:'test'}",context);
 await element('activate-draft').onclick();assert.equal(element('publication-confirmation').hidden,false);
 context.observeRevision('9007199254740993');assert.equal(element('publication-confirmation').hidden,false);
 assert.equal(context.observeRevision('9007199254740992'),false);assert.equal(element('publication-confirmation').hidden,false);
 context.observeRevision('9007199254740994');assert.equal(element('publication-confirmation').hidden,true);assert.equal(element('activate-draft').disabled,true);
 await element('confirm-publication').onclick();assert.equal(pending.length,9);
 console.log('Preview failure, edit, revision change and out-of-order invalidation: passed');
})().catch(error=>{console.error(error);process.exitCode=1;});

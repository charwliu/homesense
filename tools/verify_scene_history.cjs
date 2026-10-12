// Shipped history UI: pagination and late responses, without network access.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const element=()=>({value:'',textContent:'',children:[],addEventListener(){},append(...items){this.children.push(...items)},replaceChildren(...items){this.children=items;this.textContent=''}});
const elements=new Map();const get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
const pending=[];const context=vm.createContext({document:{getElementById:get,createElement:element},fetch:path=>new Promise(resolve=>pending.push({path,resolve}))});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
const respond=(i,page)=>pending[i].resolve({ok:true,json:async()=>page});
(async()=>{
 const first=context.loadSceneHistory('书房+','9223372036854775807');
 assert.match(pending[0].path,/scene=%E4%B9%A6%E6%88%BF%2B/);assert.match(pending[0].path,/revision=9223372036854775807/);
 respond(0,{scene:'书房+',revision:'9223372036854775807',runs:[{status:'Cancelled',boot:'boot',run:'2',started_ms:'1',updated_ms:'2'}],next_before:{boot:'boot',run:'2'}});await first;
 assert.equal(get('history-runs').children.length,1);assert.equal(get('history-next').disabled,false);
 const next=get('history-next').onclick();assert.match(pending[1].path,/before_boot=boot&before_run=2/);
 respond(1,{scene:'书房+',revision:'9223372036854775807',runs:[],next_before:null});await next;assert.equal(get('history-next').disabled,true);assert.equal(get('history-runs').textContent,'没有运行记录');
 const stale=context.loadSceneHistory('old','1');const fresh=context.loadSceneHistory('new','1');
 respond(3,{scene:'new',revision:'1',runs:[],next_before:null});await fresh;respond(2,{scene:'old',revision:'1',runs:[],next_before:null});await stale;assert.match(get('history-title').textContent,/^new/);
 const detail=context.loadRunDetails({boot:'boot',run:'2'},'1','commands');
 respond(4,{scene:'new',status:'Partial',redacted:true,commands:[],next_after:'9223372036854775807'});await detail;
 const detailBox=get('run-details');assert.match(detailBox.children[0].textContent,/隐藏/);assert.equal(detailBox.children[1].textContent,'当前页没有可见记录');
 const continuation=detailBox.children[2].onclick();assert.match(pending[5].path,/after=9223372036854775807/);
 respond(5,{scene:'new',status:'Partial',redacted:false,commands:[{device:'lamp',status:'Written',step:0,command:'9'}],next_after:null});await continuation;assert.match(detailBox.children[1].textContent,/Written/);assert.equal(detailBox.children.length,2);
 console.log('Scene history/detail pagination, integer precision and stale response isolation: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

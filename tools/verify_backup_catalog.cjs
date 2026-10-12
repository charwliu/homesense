// Synthetic browser model: backup metadata, text rendering, and session fencing.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map();
const element=id=>{if(!elements.has(id))elements.set(id,{value:'',textContent:'',children:[],addEventListener(){},append(child){this.children.push(child)},replaceChildren(){this.textContent='';this.children=[];}});return elements.get(id);};
const pending=[];
const context=vm.createContext({document:{getElementById:element,createElement:()=>({})},fetch:path=>new Promise(resolve=>pending.push({path,resolve})),clearTimeout(){},setTimeout(){},location:{origin:'http://127.0.0.1:1'}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 vm.runInContext("csrf='synthetic'",context);
 const read=element('backup-status').onclick();assert.equal(pending[0].path,'/api/v1/backups');
 pending.shift().resolve({ok:true,json:async()=>({verified:false,groups:[{kind:'daily',entries:[{name:'<script>.sqlite',bytes:'9007199254740993',regular_file:true}]},{kind:'publication',entries:[]}]})});await read;
 assert.match(element('backup-result').textContent,/未校验/);assert.match(element('backup-result').textContent,/<script>\.sqlite/);assert.match(element('backup-result').textContent,/9007199254740993/);
 const button=element('backup-actions').children[0];
 const first=button.onclick(),second=button.onclick();assert.equal(pending[0].path,'/api/v1/backups/inspect');
 pending[1].resolve({ok:true,json:async()=>({name:'new.sqlite',schema:18,active_revision:'9007199254740993'})});await second;
 pending[0].resolve({ok:true,json:async()=>({name:'old.sqlite',schema:18,active_revision:'1'})});await first;
 assert.match(element('backup-inspection').textContent,/new.sqlite/);assert.match(element('backup-inspection').textContent,/9007199254740993/);pending.length=0;
 const inspection=button.onclick();
 const stale=element('backup-status').onclick();const logout=element('logout').onclick();pending[2].resolve({ok:true,json:async()=>({})});await logout;
 pending[1].resolve({ok:true,json:async()=>({groups:[{kind:'daily',entries:[{name:'stale'}]}]})});await stale;
 pending[0].resolve({ok:true,json:async()=>({name:'obsolete.sqlite',schema:18})});await inspection;assert.equal(element('backup-inspection').textContent,'');assert.equal(element('backup-actions').children.length,0);
 assert.equal(element('backup-result').textContent,'');console.log('Backup metadata and logout fencing: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

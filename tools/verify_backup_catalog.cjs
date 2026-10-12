// Synthetic browser model: backup metadata, text rendering, and session fencing.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const elements=new Map();
const element=id=>{if(!elements.has(id))elements.set(id,{value:'',textContent:'',addEventListener(){},replaceChildren(){this.textContent='';}});return elements.get(id);};
const pending=[];
const context=vm.createContext({document:{getElementById:element},fetch:path=>new Promise(resolve=>pending.push({path,resolve})),clearTimeout(){},setTimeout(){},location:{origin:'http://127.0.0.1:1'}});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/app.js'),'utf8'),context);
(async()=>{
 vm.runInContext("csrf='synthetic'",context);
 const read=element('backup-status').onclick();assert.equal(pending[0].path,'/api/v1/backups');
 pending.shift().resolve({ok:true,json:async()=>({verified:false,groups:[{kind:'daily',entries:[{name:'<script>.sqlite',bytes:'9007199254740993',regular_file:true}]},{kind:'publication',entries:[]}]})});await read;
 assert.match(element('backup-result').textContent,/未校验/);assert.match(element('backup-result').textContent,/<script>\.sqlite/);assert.match(element('backup-result').textContent,/9007199254740993/);
 const stale=element('backup-status').onclick();const logout=element('logout').onclick();pending[1].resolve({ok:true,json:async()=>({})});await logout;
 pending[0].resolve({ok:true,json:async()=>({groups:[{kind:'daily',entries:[{name:'stale'}]}]})});await stale;
 assert.equal(element('backup-result').textContent,'');console.log('Backup metadata and logout fencing: passed');
})().catch(error=>{console.error(error);process.exitCode=1});

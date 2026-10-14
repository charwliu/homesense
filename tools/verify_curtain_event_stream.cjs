// Shipped WebSocket handler: motor inference updates despite unchanged actual
// feedback cursor/version, without any HTTP refresh or repeated user action.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path'),crypto=require('node:crypto');
const app=process.argv[2]||path.join(__dirname,'../crates/homesensed/src/web/app.js');
const source=fs.readFileSync(app,'utf8');const old=process.argv.includes('--expect-old');
const input=process.argv[3]&&!process.argv[3].startsWith('--')?JSON.parse(fs.readFileSync(process.argv[3],'utf8')):null;
const element=()=>({value:'',disabled:false,hidden:false,textContent:'',children:[],addEventListener(){},setAttribute(){},append(...items){this.children.push(...items)},replaceChildren(...items){this.children=items}});
const elements=new Map(),get=id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id)};
const sockets=[],requests=[];class Socket {constructor(){sockets.push(this)}close(){this.onclose?.()}send(value){this.sent=value}}
const context=vm.createContext({document:{getElementById:get,createElement:element,addEventListener(){},visibilityState:'visible'},navigator:{onLine:true},AbortController,
 WebSocket:Socket,crypto:{randomUUID:()=> 'synthetic'},location:{origin:'https://localhost'},addEventListener(){},clearTimeout(){},setTimeout(){return 1},fetch:(...args)=>{requests.push(args);return new Promise(()=>{})}});
vm.runInContext(source,context);const run=code=>vm.runInContext(code,context);
run("csrf='synthetic';connectEvents()");const socket=sockets[0];socket.onopen();assert.equal(JSON.parse(socket.sent).cursor,null);
const base={id:'curtain',name:'Synthetic curtain',kind:'Motor',room:null,motor_calibrated:true,can_control:true,observed:null,reported:null,pending:null,quality:'Unknown',state_authority:'feedback',version:2};
const model={source:'inferred',movement:null,position:null,percent_hundredths:null,stop_led_active:null,last_written_id:null,last_written_ms:null,last_written_command:null,pending:null};
const frame=(estimate,reset=false)=>({type:'state',reset,revision:'1',cursor:{boot:'synthetic',revision:1,transport_epoch:1,sequence:6,scope:'synthetic'},devices:[{...base,motor_estimate:{...model,...estimate}}],scenes:[]});
const frames=input?.events||[
 frame({},true),frame({movement:'Opening',stop_led_active:true,last_written_id:1,last_written_ms:10,last_written_command:'Open'}),
 frame({movement:'Stopped',position:'Open',percent_hundredths:10000,stop_led_active:true,last_written_id:1,last_written_ms:10,last_written_command:'Open'}),
 frame({movement:'Stopped',position:'Open',percent_hundredths:10000,stop_led_active:false,last_written_id:1,last_written_ms:10,last_written_command:'Open'})
];
const rendering=[];let previousCursor=null;
for(const event of frames) {
 socket.onmessage({data:JSON.stringify(event)});
 const device=event.devices.find(d=>d.kind==='Motor');if(!device)continue;context.motorId=device.id;
 const text=run('deviceCards.get(motorId).info.textContent');rendering.push(text);
 assert.equal(requests.length,0,'incoming state must render without an HTTP refresh');assert.equal(sockets.length,1,'unchanged cursor must not force a reconnect');
 if(previousCursor)assert.deepEqual(event.cursor,previousCursor);previousCursor=event.cursor;
 if(old) {assert.match(text,/位置未知/);assert.doesNotMatch(text,/估计正在打开|估计已停止|估计全开|100%/);}
 else {
  const estimate=device.motor_estimate;
  if(estimate.movement==='Opening')assert.match(text,/估计正在打开/);
  if(estimate.movement==='Stopped')assert.match(text,/估计已停止/);
  if(estimate.position==='Open')assert.match(text,/估计全开/);
  if(estimate.percent_hundredths===10000)assert.match(text,/估计 100%/);
  if(estimate.stop_led_active===true)assert.match(text,/停止键活动提示：红/);
  if(estimate.stop_led_active===false)assert.match(text,/停止键活动提示：绿/);
 }
 const buttons=run('deviceCards.get(motorId).buttons.map(b=>b.disabled)');assert.equal(Array.from(buttons).some(Boolean),false,'natural completion/green hint cannot disable manual actions');
 assert.equal(device.observed,null,'inference must not fabricate actual feedback');
}
console.log(JSON.stringify({script_sha256:crypto.createHash('sha256').update(source).digest('hex'),mode:old?'counterfactual_old_script_not_field_attribution':'current_shipped_handler',real_wss_capture:!!input,rendering,http_refreshes:requests.length,new_user_actions:0,passed:true}));

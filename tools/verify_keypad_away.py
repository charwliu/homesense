#!/usr/bin/env python3
"""Ordinary production CLI over synthetic loopback TCP/TLS/WSS only."""
from pathlib import Path
import argparse,base64,hashlib,http.client,json,os,signal,socket,sqlite3,ssl,struct,subprocess,threading,time,traceback,uuid
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--sha256',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
os.umask(0o077);assert a.binary.is_absolute() and hashlib.sha256(a.binary.read_bytes()).hexdigest()==a.sha256;a.output.mkdir(exist_ok=False)
stop=threading.Event();start=time.monotonic();phase='initial';frames=[];errors=[];process=None;ws=None
stamp=lambda:round((time.monotonic()-start)*1000,3)
def save(name,data):(a.output/name).write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
def port():
 with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]

listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen(4);listener.settimeout(.1)
web=port();origin=f'https://127.0.0.1:{web}';secret=os.urandom(32).hex();(a.output/'token').write_text(secret)
cert=a.output/'cert.pem';key=a.output/'key.pem'
subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1','-keyout',str(key),'-out',str(cert)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
ctx=ssl.create_default_context(cafile=str(cert))
class SocketEvents:
    def __init__(self):
        self.stream=ctx.wrap_socket(socket.create_connection(('127.0.0.1',web),timeout=2),server_hostname='127.0.0.1');self.stream.settimeout(.25);self.pending=b'';self.frames=[];self.fail=None;self.sending=threading.Lock()
        k=base64.b64encode(os.urandom(16)).decode();self.stream.sendall((f'GET /api/v1/events HTTP/1.1\r\nHost: 127.0.0.1:{web}\r\nOrigin: {origin}\r\nAuthorization: Bearer {secret}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {k}\r\n\r\n').encode())
        while b'\r\n\r\n' not in self.pending:self.pending+=self.stream.recv(4096)
        header,self.pending=self.pending.split(b'\r\n\r\n',1);assert header.startswith(b'HTTP/1.1 101'),header
        expected=base64.b64encode(hashlib.sha1((k+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
        assert expected.lower() in header.lower(),header
        self.send(1,b'{"cursor":null}');self.thread=threading.Thread(target=self.read,daemon=True);self.thread.start()
    def send(self,opcode,data):
        mask=os.urandom(4);n=len(data);head=bytes([0x80|opcode,0x80|n]) if n<126 else bytes([0x80|opcode,0xfe])+struct.pack('!H',n)
        with self.sending:self.stream.sendall(head+mask+bytes(x^mask[i%4] for i,x in enumerate(data)))
    def exact(self,n):
        while len(self.pending)<n:
            if stop.is_set():raise EOFError()
            try:d=self.stream.recv(max(4096,n-len(self.pending)))
            except socket.timeout:continue
            if not d:raise EOFError()
            self.pending+=d
        x,self.pending=self.pending[:n],self.pending[n:];return x
    def read(self):
        try:
            while not stop.is_set():
                b1,b2=self.exact(2);assert not b2&128;n=b2&127
                if n==126:n=struct.unpack('!H',self.exact(2))[0]
                if n==127:n=struct.unpack('!Q',self.exact(8))[0]
                assert n<=512*1024;data=self.exact(n);op=b1&15
                if op==9:self.send(10,data)
                elif op==1:self.frames.append({'at_ms':stamp(),'phase':phase,'message':json.loads(data)})
                elif op==8:raise EOFError()
                else:assert op==10,(op,data)
        except BaseException as e:
            if not stop.is_set():self.fail=repr(e)
    def close(self):
        self.stream.close();self.thread.join(timeout=2)

def request(path,method='GET',body=None,token=None):
 c=http.client.HTTPSConnection('127.0.0.1',web,context=ctx,timeout=3)
 try:
  headers={'Origin':origin,'Authorization':'Bearer '+(token or secret),'Idempotency-Key':str(uuid.uuid4())}
  if body is not None:headers['Content-Type']='application/json'
  c.request(method,path,body=None if body is None else json.dumps(body),headers=headers);r=c.getresponse();return {'status':r.status,'body':json.loads(r.read())}
 finally:c.close()
def wait(fn,seconds,label):
 until=time.monotonic()+seconds
 while time.monotonic()<until:
  assert process.poll() is None,('CLI exited',label)
  v=fn()
  if v:return v
  time.sleep(.025)
 raise AssertionError('timeout '+label)
modules=[];devices=[];reviews=[];kinds={};values={};reset_seen=[]
verification={'status':'verified','verified_at_unix_ms':1,'evidence':'synthetic loopback only'}
for n in range(12):
 kind='switch' if n<7 else 'dimmer';address=(0x20+n) if n<7 else (0x40+n-7);name=f'{kind}-{n}';kinds[address]=kind
 modules.append({'id':name,'address':f'{address:02X}','profile':'r8i8_din_pro_v2_7_7' if kind=='switch' else 'ad8_din_rs485_v2_2_1','verification':verification})
 ids=[]
 for channel in range(1,9):
  id=f'{name}:{channel}';ids.append(id);values[address,channel]=0xf1 if kind=='switch' else 50
  devices.append({'id':id,'name':id,'module_id':name,'channel':channel,'enabled':True,'room':'visible-room' if n==0 else 'coverage-room',**({} if n==0 else {'catalog_visible':False})})
 reviews.append({'module_id':name,'devices':ids,'review_evidence':'explicit synthetic full eight-output scope'})
modules.append({'id':'panel-module','address':'6A','profile':'ts6_86_rs485_v2_6_5_6key','verification':verification,'led_mode':'manual'})
config={'schema_version':2,'revision':1,'rooms':[{'id':'visible-room','name':'Visible fixture room'},{'id':'coverage-room','name':'Coverage fixture room'}],'gateways':[{'id':'bus','endpoint':f'127.0.0.1:{listener.getsockname()[1]}','modules':modules}],'devices':devices,'keypads':[{'id':'panel','name':'Synthetic entry','module_id':'panel-module','room':'visible-room','enabled':True,'keys':[{'number':n,'label':str(n),'led_endpoint':{'id':f'key:{n}','name':f'Synthetic key {n}','enabled':True},**({'action':{'kind':'away_off','review_evidence':'native-only reviewed fixture','indicator_reset':'green'}} if n==6 else {})} for n in range(1,7)]}]}
automation={'schema_version':1,'configuration_revision':1,'variables':[],'rules':[],'away_off':{'enabled':True,'review_evidence':'synthetic explicit96; no field inputs','active_execution_policy':'cancel_affected_lighting','modules':reviews},'immediate_scenes':[{'id':'hidden-target','enabled':True,'review_evidence':'synthetic hidden scene reference','activate':[{'source':'fixture/hidden-switch','action':{'kind':'command','device':'switch-6:8','command':{'kind':'switch','on':True}}}]}]}
save('devices.json',config);save('automation.json',automation)
def gateway():
 try:
  while not stop.is_set():
   try:c,peer=listener.accept()
   except socket.timeout:continue
   assert peer[0]=='127.0.0.1'
   with c:
    c.settimeout(.1);pending=b''
    def emit(address,channels):c.sendall(b''.join(bytes([0xae,address,0xd0+channel,values[address,channel],0xee]) for channel in channels))
    try:
     while not stop.is_set():
      try:data=c.recv(4096)
      except socket.timeout:continue
      if not data:break
      pending+=data
      while len(pending)>=5:
       frame,pending=pending[:5],pending[5:];assert frame[0]==0xae and frame[-1]==0xee,frame.hex();address,cmd,value=frame[1:4]
       frames.append({'at_ms':stamp(),'phase':phase,'raw':frame.hex()})
       if address==0x6a:
        assert cmd==6 and value==0xf2,frame.hex()
        with sqlite3.connect(a.output/'state/automation-tcp.sqlite') as db:
         count=db.execute("SELECT count(*) FROM command_journal WHERE intent=x'01080000' AND status IN ('written','observed')").fetchone()[0]
        reset_seen.append({'at_ms':stamp(),'written_modules_at_wire':count});assert count>=12,count
       elif cmd==0:
        assert value==(0xf2 if kinds[address]=='switch' else 0),frame.hex()
        for channel in range(1,9):values[address,channel]=value
        emit(address,range(1,9))
       elif kinds[address]=='dimmer' and cmd==0xa0:assert value==0xf2;emit(address,range(1,9))
       elif 0xa1<=cmd<=0xa8:assert value==0xf2;emit(address,[cmd-0xa0])
       else:assert kinds[address]=='switch' and 1<=cmd<=8 and value in [0xf1,0xf2],frame.hex();values[address,cmd]=value;emit(address,[cmd])
    except (ConnectionResetError,BrokenPipeError):pass
 except BaseException as e:errors.append(repr(e))
 finally:listener.close()
def bulk():return [f for f in frames if bytes.fromhex(f['raw'])[2]==0]
args=[str(a.binary),'serve-automation-tcp',str(a.output/'devices.json'),str(a.output/'automation.json'),str(a.output/'state'),'forever','--coexistence-confirmed','--recoveries','1','--write-interval-ms','20','--web-listen',f'127.0.0.1:{web}','--web-origin',origin,'--web-token',str(a.output/'token'),'--web-cert',str(cert),'--web-key',str(key),'--web-role','administrator']
worker=threading.Thread(target=gateway,daemon=True);result={'passed':False,'binary_sha256':a.sha256,'field_commands':0,'network':'loopback fake gateways only'}
try:
 worker.start()
 with (a.output/'server.log').open('xb') as log:
  process=subprocess.Popen(args,stdout=log,stderr=subprocess.STDOUT)
  def ready():
   try:
    r=request('/api/v1/modules');return r if r['status']==200 and r['body'].get('ready') else None
   except (OSError,ssl.SSLError,http.client.HTTPException):return None
  save('initial-ready.json',wait(ready,15,'full96 sync'))
  initial=request('/api/v1/devices');save('actual-dto.json',initial)
  lights=[d for d in initial['body']['devices'] if d['id']!='panel'];assert len(lights)==96 and sum(d['catalog_visible'] for d in lights)==8
  panel=next(d for d in initial['body']['devices'] if d['id']=='panel');key=panel['keys'][5]
  assert key['action']=={'kind':'away_off','path':'/api/v1/away-off'} and key['card_action']=={'kind':'away_off','path':'/api/v1/away-off','command':{'action':'keypad_press','keypad':'panel','key':6}},key
  assert key['can_activate'] and key['action_gate']['available']
  rooms=request('/api/v1/rooms');save('rooms.json',rooms);assert [r['id'] for r in rooms['body']['rooms']]==['visible-room'],rooms
  def issue(scope):
   response=request('/api/v1/credentials','POST',{'role':'Member','devices':scope,'scenes':['hidden-target']});assert response['status']==201,response;return response['body']['secret']
  all_ids=[d['id'] for d in devices];full_token=issue(all_ids+['key:6']);partial_token=issue(all_ids[:-1]+['key:6']);key_token=issue(['key:6']);nokey_token=issue(all_ids)
  for token,name in [(partial_token,'missing-output'),(key_token,'only-key')]:
   row=request('/api/v1/devices',token=token);save(name+'-dto.json',row);key=next(d for d in row['body']['devices'] if d['id']=='panel')['keys'][5];assert key['action'] is None and key['card_action'] is None and not key['can_activate'] and not key['action_gate']['available'],key
  body={'revision':'1','request_id':'card-away','command':{'action':'keypad_press','keypad':'panel','key':6}}
  for token in [partial_token,key_token,nokey_token]:assert request('/api/v1/away-off','POST',body,token)['status']==403
  assert not bulk() and not reset_seen
  ws=SocketEvents();wait(lambda:any(f['message'].get('type')=='state' for f in ws.frames),3,'initial WSS')
  phase='card-away';response=request('/api/v1/away-off','POST',body,full_token);save('card-receipt.json',response)
  assert response['status']==202 and response['body']['outcome']=='accepted' and len(response['body']['commands'])==13 and response['body']['physical_result_confirmed'] is False,response
  wait(lambda:len(bulk())==12 and len(reset_seen)==1,5,'12 modules then reset')
  count=len(frames);replay=request('/api/v1/away-off','POST',body,full_token);save('replay.json',replay);assert replay['status']==200 and replay['body']==response['body'],replay;time.sleep(.1);assert len(bulk())==12 and len(reset_seen)==1
  plain={'revision':'1','request_id':'card-away'};assert request('/api/v1/away-off','POST',plain,full_token)['status']==409
  phase='hidden-scene';r=request('/api/v1/scene-actions','POST',{'scene':'hidden-target','request_id':'hidden-on','on':True},full_token);save('hidden-scene-receipt.json',r);assert r['status']==202,r
  wait(lambda:any(f['raw']=='ae2608f1ee' and f['phase']=='hidden-scene' for f in frames),4,'hidden scene target command')
  phase='direct-away';before_reset=len(reset_seen);r=request('/api/v1/away-off','POST',{'revision':'1','request_id':'direct-away'},full_token);save('direct-receipt.json',r);assert r['status']==202 and len(r['body']['commands'])==12,r
  wait(lambda:len(bulk())==24,5,'direct12');time.sleep(.2);assert len(reset_seen)==before_reset
  final=request('/api/v1/devices');save('final-dto.json',final);key=next(d for d in final['body']['devices'] if d['id']=='panel')['keys'][5];assert key['led']['observed'] is None and key['led']['quality']=='Unknown' and key['led']['last_written']['color']=='green',key
  def pushed_green():
   return next((f for f in ws.frames if f['message'].get('type')=='state' and any(d['id']=='panel' and d['keys'][5]['led'].get('last_written') is not None and d['keys'][5]['led']['last_written'].get('color')=='green' for d in f['message'].get('devices',[]))),None)
  save('green-wss.json',wait(pushed_green,4,'green WSS'));save('wss.json',ws.frames)
  for event in ws.frames:
   if event['message'].get('type')=='state':assert all('catalog_visible' in d for d in event['message'].get('devices',[]) if d['id']!='panel')
  with sqlite3.connect(a.output/'state/automation-tcp.sqlite') as db:
   rows=db.execute('SELECT command_id,device_id,hex(intent),status,updated_ms FROM command_journal ORDER BY command_id').fetchall();effects=db.execute('SELECT count(*) FROM module_off_effect').fetchone()[0]
  save('journal.json',rows);assert effects==192,effects
  result.update(passed=True,raw_outputs_retained=96,daily_visible_outputs=8,card_bulk_frames=12,card_reset_frames=1,card_commands=13,direct_commands=12,replay_extra_commands=0,only_key_and_partial_denied=True,green_only_after_12_written=True,hidden_scene_control_preserved=True,actual_LED_observation_unknown=True,wss_frames=len(ws.frames))
except BaseException as e:result.update(error=repr(e),traceback=traceback.format_exc())
finally:
 result['unexpected_exit']=process is not None and process.poll() is not None
 if process is not None and process.poll() is None:
  process.send_signal(signal.SIGTERM)
  try:result['exit_code']=process.wait(timeout=5)
  except subprocess.TimeoutExpired:process.kill();result['exit_code']=process.wait();result['forced_kill']=True
 stop.set()
 if ws:
  try:ws.close()
  except OSError:pass
 worker.join(timeout=2);save('frames.json',frames);save('reset-wire-audit.json',reset_seen);result['worker_errors']=errors;save('result.json',result);print(json.dumps(result))
 if not result['passed'] or errors:raise SystemExit(2)

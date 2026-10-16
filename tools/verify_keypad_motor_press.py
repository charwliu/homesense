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
  time.sleep(.02)
 raise AssertionError('timeout '+label)
def gateway():
 try:
  while not stop.is_set():
   try:c,peer=listener.accept()
   except socket.timeout:continue
   assert peer[0]=='127.0.0.1'
   with c:
    c.settimeout(.1);pending=b''
    try:
     while not stop.is_set():
      try:data=c.recv(4096)
      except socket.timeout:continue
      if not data:break
      pending+=data
      while len(pending)>=5:
       f,pending=pending[:5],pending[5:];assert f[0]==0xae and f[-1]==0xee,f.hex();address,command,value=f[1:4]
       frames.append({'at_ms':stamp(),'phase':phase,'raw':f.hex()})
       if address==3:
        assert 0xa1<=command<=0xa8 and value==0xf2,f.hex();c.sendall(bytes([0xae,3,0xd0+command-0xa0,0xf2,0xee]))
       elif address==0x22:assert command==1 and value in [0xf0,0xf1,0xf2],f.hex()
       elif address==0x41:assert 1<=command<=6 and value in [0xf1,0xf2],f.hex()
       else:raise AssertionError(f.hex())
    except (ConnectionResetError,BrokenPipeError):pass
 except BaseException as e:errors.append(repr(e))
 finally:listener.close()
verification={'status':'verified','verified_at_unix_ms':1,'evidence':'synthetic loopback regression only'}
config={'schema_version':2,'revision':1,'gateways':[{'id':'bus','endpoint':f'127.0.0.1:{listener.getsockname()[1]}','modules':[
 {'id':'motor-module','address':'22','profile':'c4_din_pro_v2_7_7','verification':verification},
 {'id':'panel-module','address':'41','profile':'ts6_86_rs485_v2_6_5_6key','led_mode':'manual','verification':verification},
 {'id':'switch-module','address':'03','profile':'r8i8_din_pro_v2_7_7','verification':verification}]}],
 'devices':[{'id':'curtain','name':'Synthetic motor','module_id':'motor-module','channel':1,'enabled':True,'motor_calibration':{'open_direction':'up','stop_verified':True,'evidence':'synthetic','verified_at_unix_ms':1},'curtain_state':{'open_travel_ms':5000,'close_travel_ms':5000,'stop_led_idle_ms':1,'direction_led_policy':{'kind':'written_direction_delay','opposite_red_after_ms':2000},'evidence':'synthetic','reviewed_at_unix_ms':1,'leds':{'open':['key:1'],'close':['key:2'],'stop':['key:3']}}},
 {'id':'light','name':'Synthetic switch','module_id':'switch-module','channel':1,'enabled':True}],
 'keypads':[{'id':'panel','name':'Synthetic panel','module_id':'panel-module','enabled':True,'keys':[{'number':n,'label':str(n),'led_endpoint':{'id':f'key:{n}','name':f'Synthetic LED {n}','enabled':True},**({'action':{'kind':'motor','device':'curtain','action':{1:'open',2:'close',3:'stop',4:'open'}[n],**({'press_behavior':'repeat_to_stop'} if n in [1,2] else {}),'review_evidence':'synthetic native card only'}} if n in [1,2,3,4] else {})} for n in range(1,7)]}]}
automation={'schema_version':1,'configuration_revision':1,'variables':[],'rules':[]}
save('devices.json',config);save('automation.json',automation)
args=[str(a.binary),'serve-automation-tcp',str(a.output/'devices.json'),str(a.output/'automation.json'),str(a.output/'state'),'forever','--coexistence-confirmed','--recoveries','1','--write-interval-ms','20','--web-listen',f'127.0.0.1:{web}','--web-origin',origin,'--web-token',str(a.output/'token'),'--web-cert',str(cert),'--web-key',str(key),'--web-role','administrator']
def cmd(action,request_id,**fields):return request('/api/v1/device-commands','POST',{'device':'curtain','revision':'1','request_id':request_id,'command':{'action':action,**fields}})
def press(n,id):return cmd('keypad_press',id,keypad='panel',key=n)
def motor_frames():return [f for f in frames if f['raw'].startswith('ae2201')]
def next_motor(n,expected):
 wait(lambda:len(motor_frames())>n,4,'motor '+expected);row=motor_frames()[n];assert row['raw']==expected,row;return row
result={'passed':False,'binary_sha256':a.sha256,'network':'synthetic loopback only','field_writes':0}
worker=threading.Thread(target=gateway,daemon=True)
try:
 worker.start()
 with (a.output/'server.log').open('xb') as log:
  process=subprocess.Popen(args,stdout=log,stderr=subprocess.STDOUT)
  def ready():
   try:
    r=request('/api/v1/modules');return r if r['status']==200 and r['body'].get('ready') else None
   except (OSError,ssl.SSLError,http.client.HTTPException):return None
  save('initial-ready.json',wait(ready,12,'readiness'))
  initial=request('/api/v1/devices');save('actual-initial-dto.json',initial)
  panel=next(d for d in initial['body']['devices'] if d['id']=='panel');keys=panel['keys']
  for i,direction in [(0,'open'),(1,'close'),(2,'stop'),(3,'open')]:
   assert keys[i]['action']=={'kind':'motor','device':'curtain','command':{'action':direction},'path':'/api/v1/device-commands'},keys[i]
  for i in [0,1]:assert keys[i]['card_action']['command']=={'action':'keypad_press','keypad':'panel','key':i+1}
  for i in [2,3,4,5]:assert keys[i]['card_action'] is None
  issued=request('/api/v1/credentials','POST',{'role':'Member','devices':['curtain','key:1'],'scenes':[]});assert issued['status']==201,issued
  limited=request('/api/v1/devices',token=issued['body']['secret']);save('actual-partial-dto.json',limited)
  limited_panel=next(d for d in limited['body']['devices'] if d['id']=='panel')
  assert limited_panel['keys'][0]['card_action'] is not None and all(k['card_action'] is None for k in limited_panel['keys'][1:])
  for body in [
   {'device':'curtain','request_id':'missing-revision','command':{'action':'keypad_press','keypad':'panel','key':1}},
   {'device':'curtain','revision':'1','request_id':'bad-key','command':{'action':'keypad_press','keypad':'panel','key':7}},
   {'device':'curtain','revision':'1','request_id':'bad-field','command':{'action':'keypad_press','keypad':'panel','key':1,'movement':'Opening'}}]:
   assert request('/api/v1/device-commands','POST',body)['status']==400
  bad=request('/api/v1/device-commands','POST',{'device':'curtain','revision':'0','request_id':'stale','command':{'action':'keypad_press','keypad':'panel','key':1}});assert bad['status']==409,bad
  phase='initial-open';n=len(motor_frames());r=press(1,'open-card-1');save('open-receipt.json',r);assert r['status']==202 and r['body']['outcome']=='accepted',r
  first=next_motor(n,'ae2201f1ee')
  # No API reads or WSS client during this entire initial delayed edge.
  time.sleep(2.35)
  open_led=next(f for f in frames if f['raw']=='ae4101f1ee');opposite=next(f for f in frames if f['raw']=='ae4102f1ee')
  assert 0<=open_led['at_ms']-first['at_ms']<500,(first,open_led)
  assert 1950<=opposite['at_ms']-first['at_ms']<3000,(first,opposite)
  assert all(f['raw']!='ae4102f2ee' for f in frames),'opposite must never be forced green'
  result['no_client_reads_edge_ms']=opposite['at_ms']-first['at_ms']
  n=len(motor_frames());duplicate=press(1,'open-card-1');assert duplicate['status']==200 and duplicate['body']==r['body'],(duplicate,r);time.sleep(.1);assert len(motor_frames())==n
  ws=SocketEvents()
  wait(lambda:any(f['message'].get('type')=='state' for f in ws.frames),3,'initial WSS state')
  phase='same-direction-stop';r=press(1,'open-card-2');assert r['status']==202,r;next_motor(n,'ae2201f0ee');time.sleep(.25)
  wait(lambda:any(f['raw']=='ae4103f2ee' for f in frames),3,'stop LED green')
  phase='reverse';n=len(motor_frames());assert press(2,'close-card-1')['status']==202;next_motor(n,'ae2201f2ee');time.sleep(.2)
  n=len(motor_frames());assert press(1,'open-card-3')['status']==202;next_motor(n,'ae2201f1ee');time.sleep(.2)
  phase='absolute-details';n=len(motor_frames());assert cmd('open','absolute-open')['status']==202;next_motor(n,'ae2201f1ee');time.sleep(.2)
  phase='card-stop-again';n=len(motor_frames());assert press(1,'open-card-4')['status']==202;next_motor(n,'ae2201f0ee');time.sleep(.2)
  phase='close-to-endpoint';n=len(motor_frames());assert press(2,'close-card-2')['status']==202;next_motor(n,'ae2201f2ee');time.sleep(5.3)
  final=request('/api/v1/devices');save('actual-closed-dto.json',final);curtain=next(d for d in final['body']['devices'] if d['id']=='curtain');assert curtain['motor_estimate']['movement']=='Stopped' and curtain['motor_estimate']['position']=='Closed',curtain
  n=len(motor_frames());assert press(2,'close-endpoint-again')['status']==202;next_motor(n,'ae2201f2ee');time.sleep(.2)
  n=len(motor_frames());assert cmd('stop','absolute-stop')['status']==202;next_motor(n,'ae2201f0ee');time.sleep(.3)
  assert ws.fail is None,ws.fail
  states=[f for f in ws.frames if f['message'].get('type')=='state'];assert states,ws.frames
  for frame in states:
   parents=[d for d in frame['message'].get('devices',[]) if d['id']=='panel']
   if parents:
    for key_dto in parents[0]['keys']:assert key_dto['led']['observed'] is None and key_dto['led']['quality']=='Unknown'
  save('wss.json',ws.frames)
  with sqlite3.connect(a.output/'state/automation-tcp.sqlite') as c:
   rows=c.execute('SELECT command_id,device_id,hex(intent),status,updated_ms,run_id FROM command_journal ORDER BY command_id').fetchall()
  save('journal.json',rows);assert all(row[3]=='written' for row in rows if row[1]=='curtain'),rows
  first_motor=next(row for row in rows if row[0]==1 and row[1]=='curtain')
  first_opposite=next(row for row in rows if row[1]=='key:2' and row[2]=='01060100' and row[5]==1 and row[3]=='written')
  ledger_delay=first_opposite[4]-first_motor[4];assert 2000<=ledger_delay<3000,(first_motor,first_opposite)
  projected=[]
  for event in states:
   for panel_dto in event['message'].get('devices',[]):
    if panel_dto['id']=='panel':
     led=panel_dto['keys'][1]['led'];written=led.get('last_written') or {}
     if written.get('command')==str(first_opposite[0]):projected.append(written)
  assert projected and all(row['color']=='red' and int(row['updated_ms'])==first_opposite[4] for row in projected),projected
  result['ledger_direction_delay_ms']=ledger_delay;result['wss_delayed_hint_matches_ledger']=True
  result.update(passed=True,absolute_details_preserved=True,same_id_no_reevaluation=True,same_direction_stop=True,reverse_absolute=True,endpoint_defaults_direction=True,exact_child_grant_no_parent=True,wss_state_messages=len(states),motor_frames=len(motor_frames()))
except BaseException as e:result.update(error=repr(e),traceback=traceback.format_exc())
finally:
 result['unexpected_cli_exit']=process is not None and process.poll() is not None
 if process is not None and process.poll() is None:
  process.send_signal(signal.SIGTERM)
  try:result['exit_code']=process.wait(timeout=5)
  except subprocess.TimeoutExpired:process.kill();result['exit_code']=process.wait();result['forced_kill']=True
 stop.set()
 if ws:
  try:ws.close()
  except OSError:pass
 worker.join(timeout=2);save('frames.json',frames);result['worker_errors']=errors;save('result.json',result);print(json.dumps(result))
 if not result['passed'] or errors:raise SystemExit(2)

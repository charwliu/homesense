#!/usr/bin/env python3
"""Ordinary production CLI over synthetic loopback TCP/TLS/WSS only."""
from pathlib import Path
import argparse,base64,hashlib,http.client,json,os,signal,socket,sqlite3,ssl,struct,subprocess,threading,time,traceback,uuid
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--sha256',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
os.umask(0o077);assert a.binary.is_absolute() and hashlib.sha256(a.binary.read_bytes()).hexdigest()==a.sha256;a.output.mkdir(exist_ok=False);(a.output/'state').mkdir(mode=0o700)
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

http_timing=[]
def request(path,method='GET',body=None,token=None):
 c=http.client.HTTPSConnection('127.0.0.1',web,context=ctx,timeout=12)
 try:
  headers={'Origin':origin,'Authorization':'Bearer '+(token or secret),'Idempotency-Key':str(uuid.uuid4())}
  if body is not None:headers['Content-Type']='application/json'
  begin=time.monotonic();c.request(method,path,body=None if body is None else json.dumps(body),headers=headers);r=c.getresponse();out={'status':r.status,'body':json.loads(r.read())};http_timing.append({'path':path,'method':method,'phase':phase,'elapsed_s':round(time.monotonic()-begin,3),'status':r.status,'outcome':out['body'].get('outcome')});save('http-timing.json',http_timing);return out
 finally:c.close()
def wait(fn,seconds,label):
 until=time.monotonic()+seconds
 while time.monotonic()<until:
  assert process.poll() is None,('CLI exited',label)
  v=fn()
  if v:return v
  time.sleep(.025)
 raise AssertionError('timeout '+label)

modules=[{'id':f'module-{addr:02x}','address':f'{addr:02x}','profile':profile,'verification':{'status':'verified','evidence':'synthetic loopback fixture only','verified_at_unix_ms':1}} for addr,profile in [(i,'r8i8_din_pro_v2_7_7') for i in range(1,8)]+[(i,'ad8_din_rs485_v2_2_1') for i in range(0x11,0x16)]]
config={'schema_version':2,'revision':1,'gateways':[{'id':'bus','endpoint':'127.0.0.1:1','modules':modules}], 'devices':[{'id':f'port-{m["address"]}-{ch}','name':f'Test output {m["address"]}/{ch}','module_id':m['id'],'channel':ch,'enabled':True,'catalog_visible':m['address']=='01' and ch==1} for m in modules for ch in range(1,9)]}
automation={'schema_version':1,'configuration_revision':1,'variables':[],'rules':[]}
assert len(config['gateways'])==1
original_endpoint=config['gateways'][0]['endpoint'];config['gateways'][0]['endpoint']=f'127.0.0.1:{listener.getsockname()[1]}'
mods={m['id']:m for m in config['gateways'][0]['modules']};profiles={int(m['address'],16):m['profile'] for m in mods.values()};mapping={d['id']:(int(mods[d['module_id']]['address'],16),d['channel']) for d in config['devices']};outputs={k for k,v in profiles.items() if v.startswith(('r8i8','ad8'))};assert len(outputs)==12
values={(address,ch):0xf2 if profiles[address].startswith('r8i8') else 0 for address in outputs for ch in range(1,9)};leds={};cases=[]
policy={'idle_interval_ms':10800000,'active_interval_ms':10000,'backoff_ms':[30000,60000],'quiet_rounds_per_step':2,'round_budget_ms':10000,'observation_grace_ms':5000}
config['refresh_policy']=policy
for group in automation.get('scene_switch_groups',[]):
 for policy_name in ('initialization','reconciliation'):
  if group.get(policy_name):group[policy_name]['max_age_ms']=10815000
for item in automation.get('scene_tracking',[]):item['max_age_ms']=10815000
save('devices.json',config);save('automation.json',automation)
unsolicited=[]
def gateway():
 try:

  while not stop.is_set():
   try:c,peer=listener.accept();break
   except socket.timeout:continue
  if stop.is_set():return
  assert peer[0]=='127.0.0.1'
  with c:
   c.settimeout(.1);pending=b''
   def emit(addr,channels):c.sendall(b''.join(bytes([0xae,addr,0xd0+ch,values[addr,ch],0xee]) for ch in channels))
   while not stop.is_set():
    if unsolicited:
     addr,ch=unsolicited.pop(0);emit(addr,[ch]);save("unsolicited-emitted.json",{"at_ms":stamp(),"address":addr,"channel":ch})
    try:data=c.recv(4096)
    except socket.timeout:continue
    if not data:break
    pending+=data
    while len(pending)>=5:
     f,pending=pending[:5],pending[5:];assert f[0]==0xae and f[-1]==0xee;addr,cmd,v=f[1:4]
     fr={'at_ms':stamp(),'phase':phase,'raw':f.hex()};frames.append(fr)
     if addr in outputs:
      sw=profiles[addr].startswith('r8i8')
      if cmd==0:
       assert v==(0xf2 if sw else 0)
       for ch in range(1,9):values[addr,ch]=v
       emit(addr,range(1,9))
      elif 0xa0<=cmd<=0xa8 and v==0xf2:
       assert cmd!=0xa0 or not sw;time.sleep(.020);emit(addr,range(1,9) if cmd==0xa0 else [cmd-0xa0])
      elif 1<=cmd<=8 or (not sw and 0xb1<=cmd<=0xb8):
       ch=cmd if cmd<=8 else cmd-0xb0
       assert v in [0xf1,0xf2] if sw else 0<=v<=100
       values[addr,ch]=v;emit(addr,[ch])
      else:raise AssertionError(('unrequested output frame',fr))
     else:
      assert profiles[addr].startswith('ts6') and 1<=cmd<=6 and v in [0xf0,0xf1,0xf2],('motor/parameter/unexpected frame',fr)
      leds[addr,cmd]=v
 except (ConnectionResetError,BrokenPipeError):pass
 except BaseException as e:errors.append(repr(e))
 finally:listener.close()
def norm():return {k:(v==0xf1 if profiles[k[0]].startswith('r8i8') else v) for k,v in values.items()}
expected={k:False if profiles[k[0]].startswith('r8i8') else 0 for k in values}
def settle(label):
 wait(lambda:norm()==expected,8,label+' physical vector')
 def confirmed():
  r=request('/api/v1/modules');assert r['status']==200
  save(label+'-latest-modules.json',r)
  got={}
  for m in r['body']['modules']:
   addr=int(m['address'],16)
   if addr not in outputs:continue
   for slot in m['slots']:
    if slot['quality']!='confirmed':return False
    actual=slot['actual'];got[addr,slot['channel']]=actual['on'] if profiles[addr].startswith('r8i8') else actual['level']
  return got==expected
 wait(confirmed,8,label+'96confirmed readback');wait(lambda:(lambda d:d['queued']==0 and not d['in_flight'])(request('/api/v1/diagnostics')['body']),8,label+' quiescent');assert not errors,errors
 save(label+'-modules.json',request('/api/v1/modules'))
args=[str(a.binary),'serve-automation-tcp',str(a.output/'devices.json'),str(a.output/'automation.json'),str(a.output/'state'),'forever','--coexistence-confirmed','--recoveries','1','--write-interval-ms','20','--web-listen',f'127.0.0.1:{web}','--web-origin',origin,'--web-token',str(a.output/'token'),'--web-cert',str(cert),'--web-key',str(key),'--web-role','administrator']
worker=threading.Thread(target=gateway,daemon=True);result={'passed':False,'scope':'generated 96-port configuration over loopback only; no field hardware or private inputs','binary_sha256':a.sha256}
try:
 worker.start()
 with (a.output/'server.log').open('xb') as log:
  process=subprocess.Popen(args,stdout=log,stderr=subprocess.STDOUT)
  def ready():
   try:r=request('/api/v1/modules');return r['status']==200 and r['body']['ready']
   except (OSError,ssl.SSLError,http.client.HTTPException):return False
  wait(ready,15,'ready96');settle('baseline');save('initial-dto.json',request('/api/v1/devices'))
  phase='idle-before-activity';ws=SocketEvents();samples=[];begin=time.monotonic();deadline=begin+72;emitted=False;initial_queries=len([f for f in frames if 0xa0<=int(f['raw'][4:6],16)<=0xa8])
  while time.monotonic()<deadline:
   if not emitted and time.monotonic()-begin>=8:
    assert len([f for f in frames if 0xa0<=int(f['raw'][4:6],16)<=0xa8])==initial_queries,'idle timer queried before its deadline'
    phase='active-then-quiet';unsolicited.append((min(addr for addr in outputs if profiles[addr].startswith('r8i8')),1));emitted=True
   m=request('/api/v1/modules');d=request('/api/v1/diagnostics');sc=request('/api/v1/scenes')
   ports=[(module['address'],slot['channel'],slot['quality'],slot.get('observed_ms')) for module in m['body']['modules'] if module['profile'].startswith(('r8i8','ad8')) for slot in module['slots']]
   samples.append({'at_ms':stamp(),'ports':ports,'captured_ms':m['body']['captured_ms'],'scene_status':sc['status'],'selections':[x.get('selected') for x in sc['body'].get('scenes',[])],'queued':d['body']['queued']})
   time.sleep(.35)
  queries=[f for f in frames if 0xa0<=int(f['raw'][4:6],16)<=0xa8];query_times={}
  for f in queries:query_times.setdefault(f['raw'],[]).append(f['at_ms'])
  cycles={k:[round(y-x,3) for x,y in zip(v,v[1:])] for k,v in query_times.items()}
  steady_leds=[f for f in frames if f['phase']=='active-then-quiet' and int(f['raw'][2:4],16) not in outputs]
  unavailable=[f for f in ws.frames if f['message'].get('type')=='unavailable'];save('steady-samples.json',samples);save('query-cycles.json',cycles);save('ws-frames.json',ws.frames);save('steady-leds.json',steady_leds)
  result.update(passed=True,reply_delay_ms=20,steady_s=72,samples=len(samples),steady_LED_writes=len(steady_leds),WSS_frames=len(ws.frames),WSS_unavailable=len(unavailable),max_cycle_ms=max((x for v in cycles.values() for x in v),default=0),stale_samples=sum(any(p[2]!='confirmed' for p in row['ports']) for row in samples))
  assert all(x['scene_status']==200 for x in samples),result
  assert not unavailable,result
  assert len(query_times)==61 and all(len(times)>=3 for times in query_times.values()),result
  assert all(all(slot[2]=='confirmed' for slot in x['ports']) for x in samples),result
  assert not any(1<=int(f['raw'][4:6],16)<=8 and int(f['raw'][2:4],16) in outputs for f in frames), 'actuator write without user input'
except BaseException as e:result.update(passed=False,error=repr(e),traceback=traceback.format_exc())
finally:
 if process is not None and process.poll() is None:
  process.send_signal(signal.SIGTERM)
  try:result['exit_code']=process.wait(timeout=6)
  except subprocess.TimeoutExpired:process.kill();result['exit_code']=process.wait();result['forced_kill']=True
 stop.set();
 if ws:ws.close()
 worker.join(timeout=2);save('frames.json',frames);save('cases.json',cases);result['worker_errors']=errors;save('result.json',result);
 for credential in ('token','key.pem'):
  (a.output/credential).unlink(missing_ok=True)
 print(json.dumps(result,ensure_ascii=False))
 if not result['passed'] or errors:raise SystemExit(2)

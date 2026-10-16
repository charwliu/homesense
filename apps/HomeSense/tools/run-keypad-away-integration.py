"""Native Away acceptance against a pinned CLI and a new synthetic loopback bus.
Never use a production config, token or gateway. This tool does not build Rust.
"""
from pathlib import Path
import argparse,hashlib,http.client,json,os,signal,socket,sqlite3,ssl,subprocess,threading,time,traceback
p=argparse.ArgumentParser()
p.add_argument('--binary',type=Path,required=True);p.add_argument('--sha256',required=True)
p.add_argument('--output',type=Path,required=True);p.add_argument('--scratch',type=Path,required=True)
p.add_argument('--full-suite',action='store_true',help='Also enable all three original actual-server integrations in the same Swift run')
a=p.parse_args();os.umask(0o077)
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert a.binary.is_absolute() and sha(a.binary)==a.sha256
package=Path(__file__).resolve().parent.parent;repo=package.parent.parent
fixture=repo/'config/test/native-app-keypad-away'
a.output.mkdir(parents=True,exist_ok=False)
save=lambda n,d:(a.output/n).write_text(json.dumps(d,ensure_ascii=False,indent=2)+'\n')
stop=threading.Event();frames=[];errors=[];process=None;started=time.monotonic()
listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen(4);listener.settimeout(.1)
with socket.socket() as reservation:
 reservation.bind(('127.0.0.1',0));web=reservation.getsockname()[1]
origin=f'https://localhost:{web}';token=os.urandom(32).hex();(a.output/'token').write_text(token)
cert=a.output/'cert.pem';key=a.output/'key.pem'
subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=localhost','-addext','subjectAltName=DNS:localhost,IP:127.0.0.1','-addext','extendedKeyUsage=serverAuth','-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign','-keyout',str(key),'-out',str(cert)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
ctx=ssl.create_default_context(cafile=str(cert))
def get(path):
 c=http.client.HTTPSConnection('localhost',web,context=ctx,timeout=3)
 try:
  c.request('GET',path,headers={'Origin':origin,'Authorization':'Bearer '+token})
  r=c.getresponse();return {'status':r.status,'body':json.loads(r.read())}
 finally:c.close()
def gateway():
 states={3:True,4:True}
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
       f,pending=pending[:5],pending[5:];assert f[0]==0xae and f[-1]==0xee,f.hex()
       address,command,value=f[1:4];record={'at_ms':round((time.monotonic()-started)*1000,3),'raw':f.hex()}
       if address in states:
        if command==0:
         assert value==0xf2;states[address]=False;record['kind']='module_off'
         c.sendall(b''.join(bytes([0xae,address,0xd0+i,0xf2,0xee]) for i in range(1,9)))
        elif command==0xa0 or 0xa1<=command<=0xa8:
         assert value==0xf2;record['kind']='query'
         indices=range(1,9) if command==0xa0 else [command-0xa0]
         c.sendall(b''.join(bytes([0xae,address,0xd0+i,0xf1 if states[address] else 0xf2,0xee]) for i in indices))
        else:raise AssertionError('unexpected per-device write '+f.hex())
       elif address==0x41:
        assert 1<=command<=6 and value in [0xf1,0xf2];record['kind']='LED'
       else:raise AssertionError('unexpected address '+f.hex())
       frames.append(record)
    except (ConnectionResetError,BrokenPipeError):pass
 except BaseException as e:errors.append(repr(e))
 finally:listener.close()
config=json.loads((fixture/'devices.template.json').read_text());assert len(config['gateways'])==1 and all(g['endpoint'].startswith('127.0.0.1:') for g in config['gateways'])
config['gateways'][0]['endpoint']=f'127.0.0.1:{listener.getsockname()[1]}'
save('devices.json',config);save('automation.json',json.loads((fixture/'automation.json').read_text()))
save('service.json',{'fixture_kind':'synthetic-loopback-keypad-away','origin':origin,'binary_sha256':a.sha256})
cmd=[str(a.binary),'serve-automation-tcp',str(a.output/'devices.json'),str(a.output/'automation.json'),str(a.output/'state'),'forever','--coexistence-confirmed','--recoveries','1','--write-interval-ms','20','--web-listen',f'127.0.0.1:{web}','--web-origin',origin,'--web-token',str(a.output/'token'),'--web-cert',str(cert),'--web-key',str(key),'--web-role','administrator']
result={'passed':False,'binary_sha256_before':sha(a.binary),'all_endpoints_loopback':True,'production_or_phone_operations':0}
worker=threading.Thread(target=gateway,daemon=True)
try:
 worker.start()
 with (a.output/'server.log').open('xb') as log:
  process=subprocess.Popen(cmd,stdout=log,stderr=subprocess.STDOUT)
  deadline=time.monotonic()+12
  while True:
   assert process.poll() is None,'fixtureCLI exited before ready'
   try:
    ready=get('/api/v1/modules')
    if ready['status']==200 and ready['body'].get('ready'):break
   except (OSError,ssl.SSLError,http.client.HTTPException):pass
   if time.monotonic()>deadline:raise AssertionError('fixture readiness timeout')
   time.sleep(.05)
  save('initial-ready.json',ready);save('actual-initial-dto.json',get('/api/v1/devices'))
  env=os.environ.copy();env['HOMESENSE_KEYPAD_AWAY_ACTUAL_FIXTURE']=str(a.output.resolve())
  if a.full_suite:
   test=['python3',str(package/'tools/run-keypad-motor-integration.py'),'--binary',str(a.binary),'--sha256',a.sha256,'--output',str(a.output/'motor-and-full-suite'),'--scratch',str(a.scratch),'--full-suite']
  else:test=['swift','test','--package-path',str(package),'--scratch-path',str(a.scratch),'--filter','HomeKeypadAwayActualIntegrationTests']
  save('command.json',{'CLI_argv':cmd,'test_argv':test,'fixture_environment':{'HOMESENSE_KEYPAD_AWAY_ACTUAL_FIXTURE':env['HOMESENSE_KEYPAD_AWAY_ACTUAL_FIXTURE']},'no_real_credentials_or_live_config':True})
  with (a.output/'test-driver.log').open('xb') as log:
   completed=subprocess.run(test,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=180)
  result['test_exit']=completed.returncode;assert completed.returncode==0,'native integration failed; see preserved test-driver/swift-test logs'
  operation=json.loads((a.output/'actual-client-away-operation.json').read_text());save('correlation.json',operation)
  assert operation['http_status']==202 and operation['receipt']['outcome']=='accepted'
  module_frames=[f for f in frames if f.get('kind')=='module_off'];assert sorted(f['raw'] for f in module_frames)==['ae0300f2ee','ae0400f2ee']
  led=[f for f in frames if f.get('kind')=='LED' and f['raw']=='ae4106f2ee'];assert len(led)==1
  assert led[0]['at_ms']>=max(f['at_ms'] for f in module_frames)
  with sqlite3.connect(a.output/'state/automation-tcp.sqlite') as db:
   journal=db.execute('SELECT command_id,device_id,hex(intent),status,updated_ms FROM command_journal ORDER BY command_id').fetchall()
  save('journal.json',journal)
  modules=[r for r in journal if r[1].startswith('module-off-ticket:')]
  assert len(modules)==2 and all(r[3] in ['written','observed'] for r in modules)
  assert {str(r[0]) for r in modules}.issubset(set(map(str,operation['receipt']['commands'])))
  indicator=[r for r in journal if r[1]=='key:6'];assert len(indicator)==1 and indicator[0][3] in ['written','observed']
  assert indicator[0][4]>=max(r[4] for r in modules)
  result.update(passed=True,actual_App_POSTs=1,written_module_frames=2,written_green_key6_tail=1,actual_hidden_outputs_retained=16,daily_cards=1,full_suite=a.full_suite,request_id=operation['request']['request_id'])
except BaseException as e:result.update(error=repr(e),traceback=traceback.format_exc())
finally:
 if process is not None and process.poll() is None:
  process.send_signal(signal.SIGTERM)
  try:result['server_exit']=process.wait(timeout=5)
  except subprocess.TimeoutExpired:process.kill();result['server_exit']=process.wait();result['forced_kill']=True
 stop.set();worker.join(timeout=2);save('frames.json',frames)
 result.update(worker_errors=errors,binary_sha256_after=sha(a.binary))
 result['passed']=result['passed'] and not errors and result['binary_sha256_after']==a.sha256
 save('result.json',result)
 save('evidence-manifest.json',{p.name:sha(p) for p in a.output.iterdir() if p.is_file() and p.name not in ['token','key.pem','evidence-manifest.json']})
 print(json.dumps(result,ensure_ascii=False))
 if not result['passed']:raise SystemExit(2)

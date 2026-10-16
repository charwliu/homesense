"""Read-only native App catalog check. Private candidates/IDs arrive via env.
Only the output copy's gateway endpoint changes. No live network or real token.
"""
from pathlib import Path
import argparse,hashlib,http.client,json,os,signal,socket,sqlite3,ssl,subprocess,threading,time,traceback,re
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--sha256',required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--scratch',type=Path,required=True);a=p.parse_args();os.umask(0o077)
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
inputs={name:Path(os.environ['HOMESENSE_FULL_CATALOG_'+name.upper()]) for name in ['devices','automation','expected']}
assert a.binary.is_absolute() and sha(a.binary)==a.sha256
assert all(x.is_absolute() and x.is_file() for x in inputs.values())
expected=json.loads(inputs['expected'].read_text());input_hashes={n:sha(v) for n,v in inputs.items()}
assert expected['fixture_kind']=='private-full-catalog-expectations'
assert expected['source_sha256']=={n:input_hashes[n] for n in ['devices','automation']}
config=json.loads(inputs['devices'].read_text());automation=json.loads(inputs['automation'].read_text());assert config['revision']==automation['configuration_revision']
assert len(expected['scene_ids'])==len(set(expected['scene_ids']))==130 and len(expected['keypads'])==28 and len(expected['floors'])==3
assert set(expected['scene_ids'])=={s['id'] for group in ['immediate_scenes','timed_scenes'] for s in automation.get(group,[]) if s.get('enabled')}
assert {x['id'] for x in expected['keypads']}=={x['id'] for x in config['keypads']}
assert len(config['gateways'])==1
modules={int(m['address'],16):m for m in config['gateways'][0]['modules']};outputs={n:m for n,m in modules.items() if m['profile'].startswith(('r8i8','ad8'))};assert len(outputs)==12
assert len(expected['mode_variables'])>0
package=Path(__file__).resolve().parent.parent;a.output.mkdir(parents=True,exist_ok=False)
def save(n,j):(a.output/n).write_text(json.dumps(j,ensure_ascii=False,indent=2)+'\n')
stop=threading.Event();frames=[];errors=[];process=None;worker=None;started=time.monotonic()
listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen(4);listener.settimeout(.1)
with socket.socket() as reserve:reserve.bind(('127.0.0.1',0));web=reserve.getsockname()[1]
origin=f'https://localhost:{web}';(a.output/'token').write_text(os.urandom(32).hex());token=(a.output/'token').read_text();cert=a.output/'cert.pem';key=a.output/'key.pem'
subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=localhost','-addext','subjectAltName=DNS:localhost,IP:127.0.0.1','-addext','extendedKeyUsage=serverAuth','-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign','-keyout',str(key),'-out',str(cert)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
ctx=ssl.create_default_context(cafile=str(cert))
config['gateways'][0]['endpoint']=f'127.0.0.1:{listener.getsockname()[1]}'
save('devices.json',config);save('automation.json',automation);save('expected.json',expected)
save('service.json',{'fixture_kind':'private-full-catalog-readonly-loopback','origin':origin,'binary_sha256':a.sha256})
def gateway():
 try:
  while not stop.is_set():
   try:c,peer=listener.accept()
   except socket.timeout:continue
   assert peer[0]=='127.0.0.1'
   with c:
    c.settimeout(.1);pending=b''
    while not stop.is_set():
     try:data=c.recv(4096)
     except socket.timeout:continue
     if not data:break
     pending+=data
     while len(pending)>=5:
      f,pending=pending[:5],pending[5:];assert f[0]==0xae and f[-1]==0xee,f.hex();address,command,value=f[1:4]
      record={'at_ms':round((time.monotonic()-started)*1000,3),'raw':f.hex()};frames.append(record)
      if address in outputs:
       switch=outputs[address]['profile'].startswith('r8i8')
       assert 0xa1<=command<=0xa8 or (not switch and command==0xa0),'unrequested lighting output or unsupported R818 A0 '+f.hex()
       assert value==0xf2
       channels=[command-0xa0] if command!=0xa0 else range(1,9)
       record['kind']='query';c.sendall(b''.join(bytes([0xae,address,0xd0+ch,0xf2 if switch else 0,0xee]) for ch in channels))
      else:
       assert modules[address]['profile'].startswith('ts6') and 1<=command<=6 and value in [0xf0,0xf1,0xf2],'motor/parameter/unrequested frame '+f.hex()
       record['kind']='startup_LED'
 except (ConnectionResetError,BrokenPipeError):pass
 except BaseException as e:errors.append(repr(e))
 finally:listener.close()
def get(path):
 c=http.client.HTTPSConnection('localhost',web,context=ctx,timeout=5)
 try:
  c.request('GET',path,headers={'Origin':origin,'Authorization':'Bearer '+token});r=c.getresponse();return {'status':r.status,'body':json.loads(r.read())}
 finally:c.close()
def variables():
 with sqlite3.connect('file:'+str(a.output/'state/automation-tcp.sqlite')+'?mode=ro',uri=True) as db:
  rows={name:{'value':v,'version':version} for name,v,version in db.execute('SELECT name,value,version FROM logical_variable WHERE revision=?',(config['revision'],))}
 return {n:rows[n] for n in expected['mode_variables']}
args=[str(a.binary),'serve-automation-tcp',str(a.output/'devices.json'),str(a.output/'automation.json'),str(a.output/'state'),'forever','--coexistence-confirmed','--recoveries','1','--write-interval-ms','20','--web-listen',f'127.0.0.1:{web}','--web-origin',origin,'--web-token',str(a.output/'token'),'--web-cert',str(cert),'--web-key',str(key),'--web-role','administrator']
result={'passed':False,'binary_sha256_before':sha(a.binary),'input_sha256':input_hashes,'all_connections_loopback':True,'App_controls_invoked':0,'phone_or_live_service_operations':0,'development_or_release_role':os.environ.get('HOMESENSE_FULL_CATALOG_QUALIFICATION_ROLE','unspecified')}
try:
 worker=threading.Thread(target=gateway,daemon=True);worker.start()
 with (a.output/'server.log').open('xb') as log:
  process=subprocess.Popen(args,stdout=log,stderr=subprocess.STDOUT)
  deadline=time.monotonic()+45
  while True:
   assert process.poll() is None,'CLI exited before ready'
   assert not errors,errors
   try:
    ready=get('/api/v1/modules')
    if ready['status']==200 and ready['body'].get('ready'):break
   except (OSError,ssl.SSLError,http.client.HTTPException):pass
   if time.monotonic()>=deadline:raise AssertionError('96-port loopback startup readiness timeout')
   time.sleep(.05)
  save('initial-ready.json',ready);before=variables();save('mode-variables-before.json',before)
  assert all(x['value'] is None for x in before.values()),'mode initial state must remain unknown; never seed SQL'
  env={k:v for k,v in os.environ.items() if not k.startswith('HOMESENSE_')};env['HOMESENSE_FULL_CATALOG_ACTUAL_FIXTURE']=str(a.output.resolve())
  test=['swift','test','--package-path',str(package),'--scratch-path',str(a.scratch),'--filter','HomeFullCatalogActualIntegrationTests']
  save('command.json',{'CLI_argv':args,'test_argv':test,'test_environment':{'HOMESENSE_FULL_CATALOG_ACTUAL_FIXTURE':env['HOMESENSE_FULL_CATALOG_ACTUAL_FIXTURE']},'private_input_paths':{n:str(x) for n,x in inputs.items()},'input_sha256':input_hashes})
  with (a.output/'swift-test.log').open('xb') as log:r=subprocess.run(test,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=150)
  result['test_exit_code']=r.returncode
  assert r.returncode==0,'native catalog test failed; preserved swift-test.log'
  text=(a.output/'swift-test.log').read_text();assert 'skipped' not in text and re.search(r'Test run with 1 test in 1 suite passed',text)
  http=json.loads((a.output/'native-http-readonly.json').read_text());assert http['rejected_non_GET']==0 and all(x['method']=='GET' for x in http['requests'])
  phases=json.loads((a.output/'native-readonly-phases.json').read_text());assert [x['stage'] for x in phases]==['HTTP','WSS']
  assert all(x['scenes']==130 and x['panels']==28 and x['keys']==168 and x['floors']==3 and x['unknown_scene_execute_keys_available']==46 for x in phases)
  wss=json.loads((a.output/'native-initial-wss.json').read_text());assert wss and len(wss[0]['scenes'])==130
  after=variables();save('mode-variables-after.json',after);assert before==after,'read-only native access must not alter mode variables'
  assert not errors,errors
  result.update(passed=True,executed_tests=1,executed_suites=1,HTTP_and_WSS_scenes=130,typed_panels=28,legal_keys=168,floors=3,rooms=phases[0]['rooms'],unknown_scene_execute_keys_available=46,mode_variables_unknown=len(before),native_GETs=len(http['requests']),native_POSTs=0,native_button_clicks=0,WSS_frames=len(wss),simulated_modules=12,simulated_ports=96,startup_LED_frames=sum(x.get('kind')=='startup_LED' for x in frames))
except BaseException as e:result.update(error=repr(e),traceback=traceback.format_exc())
finally:
 if process is not None and process.poll() is None:
  process.send_signal(signal.SIGTERM)
  try:result['server_exit']=process.wait(timeout=8)
  except subprocess.TimeoutExpired:process.kill();result['server_exit']=process.wait();result['forced_kill']=True
 stop.set()
 if worker is not None:worker.join(timeout=2)
 save('frames.json',frames);result['worker_errors']=errors;result['binary_sha256_after']=sha(a.binary);result['private_inputs_unchanged']=all(sha(v)==input_hashes[n] for n,v in inputs.items());result['passed']=result['passed'] and not errors and result['binary_sha256_after']==a.sha256 and result['private_inputs_unchanged'];save('result.json',result)
 save('evidence-manifest.json',{str(x.relative_to(a.output)):sha(x) for x in a.output.rglob('*') if x.is_file() and x.name not in ['token','key.pem','evidence-manifest.json'] and 'state' not in x.relative_to(a.output).parts})
 print(json.dumps({k:v for k,v in result.items() if k not in ['input_sha256','traceback','error']},ensure_ascii=False));print('manifest_sha256='+sha(a.output/'evidence-manifest.json'))
 if not result['passed']:raise SystemExit(2)

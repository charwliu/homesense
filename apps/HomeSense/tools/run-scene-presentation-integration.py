"""Qualify scene presentation through actual native URLSession and WSS.
All endpoints and inputs are synthetic loopback fixtures, never live config.
"""
from pathlib import Path
import argparse,hashlib,http.client,json,os,re,signal,socket,ssl,subprocess,threading,time,traceback
parser=argparse.ArgumentParser()
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--sha256',required=True)
parser.add_argument('--output',type=Path,required=True)
parser.add_argument('--scratch',type=Path,required=True)
a=parser.parse_args();os.umask(0o077)
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert a.binary.is_absolute() and sha(a.binary)==a.sha256
package=Path(__file__).resolve().parent.parent
repository=package.parent.parent
fixture=repository/'config/test/native-app-scene-presentation'
a.output.mkdir(parents=True,exist_ok=False)
save=lambda n,d:(a.output/n).write_text(json.dumps(d,ensure_ascii=False,indent=2)+'\n')
stop=threading.Event();frames=[];errors=[];process=None;started=time.monotonic()
listener=socket.socket();listener.bind(('127.0.0.1',0));listener.listen(4);listener.settimeout(.1)
with socket.socket() as reservation:
 reservation.bind(('127.0.0.1',0));web=reservation.getsockname()[1]
origin=f'https://localhost:{web}';token=os.urandom(32).hex();(a.output/'token').write_text(token)
cert=a.output/'cert.pem';key=a.output/'key.pem'
subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=localhost',
 '-addext','subjectAltName=DNS:localhost,IP:127.0.0.1','-addext','extendedKeyUsage=serverAuth',
 '-addext','basicConstraints=critical,CA:TRUE','-addext','keyUsage=critical,digitalSignature,keyEncipherment,keyCertSign',
 '-keyout',str(key),'-out',str(cert)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
ctx=ssl.create_default_context(cafile=str(cert))
def request(path):
 c=http.client.HTTPSConnection('localhost',web,context=ctx,timeout=3)
 try:
  c.request('GET',path,headers={'Origin':origin,'Authorization':'Bearer '+token})
  r=c.getresponse();return {'status':r.status,'body':json.loads(r.read())}
 finally:c.close()
def gateway():
 levels={i:100 if i==1 else 0 for i in range(1,9)}
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
     except (ConnectionResetError,BrokenPipeError):break
     if not data:break
     pending+=data
     while len(pending)>=5:
      f,pending=pending[:5],pending[5:];assert f[0]==0xae and f[-1]==0xee,f.hex()
      address,command,value=f[1:4];record={'at_ms':round((time.monotonic()-started)*1000,3),'raw':f.hex()}
      if address==0x15 and 0xa0<=command<=0xa8:
       assert value==0xf2;record['kind']='query';indices=range(1,9)if command==0xa0 else[command-0xa0]
       for i in indices:
        time.sleep(.023);c.sendall(bytes([0xae,0x15,0xd0+i,levels[i],0xee]))
      elif address==0x15:
       assert 1<=command<=8 and (value<=100 or value in[0xf1,0xf2]),f.hex()
       levels[command]=value if value<=100 else(100 if value==0xf1 else 0)
       record.update(kind='level',channel=command,level=levels[command])
      elif address==0x41:
       assert 1<=command<=2 and value in[0xf1,0xf2],f.hex();record['kind']='LED'
      else:raise AssertionError('unexpected frame '+f.hex())
      frames.append(record)
 except BaseException as e:errors.append(repr(e))
 finally:listener.close()
config=json.loads((fixture/'devices.template.json').read_text());assert all(g['endpoint'].startswith('127.0.0.1:') for g in config['gateways'])
assert len(config['gateways'])==1
config['gateways'][0]['endpoint']=f'127.0.0.1:{listener.getsockname()[1]}'
save('devices.json',config);save('automation.json',json.loads((fixture/'automation.json').read_text()))
save('service.json',{'fixture_kind':'synthetic-loopback-scene-presentation','origin':origin,'binary_sha256':a.sha256})
cmd=[str(a.binary),'serve-automation-tcp',str(a.output/'devices.json'),str(a.output/'automation.json'),str(a.output/'state'),'forever',
 '--coexistence-confirmed','--recoveries','1','--write-interval-ms','20','--web-listen',f'127.0.0.1:{web}',
 '--web-origin',origin,'--web-token',str(a.output/'token'),'--web-cert',str(cert),'--web-key',str(key),'--web-role','administrator']
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
    ready=request('/api/v1/modules')
    if ready['status']==200 and ready['body'].get('ready'):break
   except (OSError,ssl.SSLError,http.client.HTTPException):pass
   if time.monotonic()>=deadline:raise AssertionError('fixture readiness timeout')
   time.sleep(.05)
  save('initial-ready.json',ready);save('actual-initial-dto.json',request('/api/v1/devices'))
  env=os.environ.copy();env['HOMESENSE_SCENE_PRESENTATION_ACTUAL_FIXTURE']=str(a.output.resolve())
  test=['swift','test','--package-path',str(package),'--scratch-path',str(a.scratch)]
  test+=['--filter','HomeScenePresentationActualIntegrationTests']
  save('command.json',{'CLI_argv':cmd,'test_argv':test,'fixture_environment':{k:v for k,v in env.items() if k.startswith('HOMESENSE_')},'no_live_inputs_or_real_credentials':True})
  teststart=time.monotonic()
  with (a.output/'swift-test.log').open('xb') as testlog:
   child=subprocess.Popen(test,env=env,stdout=testlog,stderr=subprocess.STDOUT)
   try:
    while child.poll() is None:
     if time.monotonic()-teststart>120:raise subprocess.TimeoutExpired(test,120)
     time.sleep(.01)
    code=child.wait()
   finally:
    if child.poll() is None:child.terminate();child.wait(timeout=5)
  result['swift_test_exit']=code
  assert code==0,'native actual integration/fullsuite failed; see preserved swift-test.log'
  testlog=(a.output/'swift-test.log').read_text()
  summary=re.search(r'Test run with (\d+) tests? in (\d+) suites? passed',testlog)
  assert summary is not None and 'skipped' not in testlog
  count,suites=map(int,summary.groups())
  assert (count,suites)==(1,1)
  result.update(executed_App_tests=count,executed_App_suites=suites)
  phases=json.loads((a.output/'actual-client-phases.json').read_text());assert len(phases)==7
  outputs=[f for f in frames if f.get('kind')=='level']
  assert [f['level']for f in outputs]==[0,100,100,100,0,100,0],outputs
  assert all(f['channel']==1 for f in outputs),'unrelated channel changed'
  result.update(passed=True,actual_App_operations=7,light_writes=len(outputs),no_App_polling=True,
    public_scene_ids=['action-1','action-2','off','light-activity'],hidden_profile_ids=['profile-1','profile-2'],read_only_activity_checked_each_phase=True)
except BaseException as e:result.update(error=repr(e),traceback=traceback.format_exc())
finally:
 if process is not None and process.poll() is None:
  process.send_signal(signal.SIGTERM)
  try:result['server_exit']=process.wait(timeout=5)
  except subprocess.TimeoutExpired:process.kill();result['server_exit']=process.wait();result['forced_kill']=True
 stop.set();worker.join(timeout=2);save('frames.json',frames)
 result['worker_errors']=errors;result['binary_sha256_after']=sha(a.binary)
 result['passed']=result['passed'] and not errors and result['binary_sha256_after']==a.sha256
 save('result.json',result)
 save('evidence-manifest.json',{p.name:sha(p) for p in a.output.iterdir() if p.is_file() and p.name not in ['token','key.pem','evidence-manifest.json']})
 print(json.dumps(result,ensure_ascii=False))
 if not result['passed']:raise SystemExit(2)

"""Native scene_execute URLSession/WSS against a pinned CLI and synthetic bus.
All controls target the newly generated private fixture. Never use a live config.
"""
from pathlib import Path
import argparse,hashlib,http.client,json,os,re,signal,socket,sqlite3,ssl,subprocess,threading,time,traceback,tempfile
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
fixture=repository/'config/test/native-app-keypad-scene-execute'
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
def variables_at_wire():
 with sqlite3.connect('file:'+str(a.output/'state/automation-tcp.sqlite')+'?mode=ro',uri=True) as db:
  row=db.execute("SELECT value,version FROM logical_variable WHERE revision=1 AND name='branch'").fetchone()
  assert row is not None,'declared variable missing before physical write'
  return {'value':row[0],'version':row[1]}
def gateway():
 on=True
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
       if address==3 and 0xa1<=command<=0xa8:
        assert value==0xf2;record['kind']='query'
        indices=[command-0xa0]
        c.sendall(b''.join(bytes([0xae,3,0xd0+i,0xf1 if i==1 and on else 0xf2,0xee]) for i in indices))
       elif address==3:
        assert command==1 and value in [0xf1,0xf2],f.hex();on=value==0xf1;record['kind']='switch'
        record['variable_at_wire']=variables_at_wire();c.sendall(bytes([0xae,3,0xd1,value,0xee]))
       elif address==0x41:
        assert command==1 and value in [0xf1,0xf2],f.hex();record['kind']='LED';record['variable_at_wire']=variables_at_wire()
       else:raise AssertionError('unexpected frame '+f.hex())
       frames.append(record)
    except (ConnectionResetError,BrokenPipeError):pass
 except BaseException as e:errors.append(repr(e))
 finally:listener.close()
config=json.loads((fixture/'devices.template.json').read_text());assert all(g['endpoint'].startswith('127.0.0.1:') for g in config['gateways'])
assert len(config['gateways'])==1
config['gateways'][0]['endpoint']=f'127.0.0.1:{listener.getsockname()[1]}'
save('devices.json',config);save('automation.json',json.loads((fixture/'automation.json').read_text()))
save('service.json',{'fixture_kind':'synthetic-loopback-keypad-scene-execute','origin':origin,'binary_sha256':a.sha256})
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
  save('initial-ready.json',ready);save('actual-initial-dto.json',request('/api/v1/devices'));save('initial-variable.json',variables_at_wire())
  env=os.environ.copy();env['HOMESENSE_SCENE_EXECUTE_ACTUAL_FIXTURE']=str(a.output.resolve())
  test=['swift','test','--package-path',str(package),'--scratch-path',str(a.scratch)]
  test+=['--filter','HomeKeypadSceneExecuteActualIntegrationTests']
  save('command.json',{'CLI_argv':cmd,'test_argv':test,'fixture_environment':{k:v for k,v in env.items() if k.startswith('HOMESENSE_')},'no_live_inputs_or_real_credentials':True})
  opened={};teststart=time.time()
  with (a.output/'swift-test.log').open('xb') as testlog:
   child=subprocess.Popen(test,env=env,stdout=testlog,stderr=subprocess.STDOUT)
   try:
    while child.poll() is None:
     if time.time()-teststart>120:raise subprocess.TimeoutExpired(test,120)
     time.sleep(.01)
    code=child.wait()
   finally:
    if child.poll() is None:child.terminate();child.wait(timeout=5)
  logs=[]
  for name,handle in opened.items():
   handle.seek(0);data=handle.read();handle.close();out=Path(name).parent.name+'.log'
   (a.output/out).write_bytes(data);logs.append({'source':name,'saved':out,'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()})
  save('legacy-actual-server-logs.json',logs)
  result['swift_test_exit']=code
  assert code==0,'native actual integration/fullsuite failed; see preserved swift-test.log'
  testlog=(a.output/'swift-test.log').read_text()
  summary=re.search(r'Test run with (\d+) tests? in (\d+) suites? passed',testlog)
  assert summary is not None and 'skipped' not in testlog
  count,suites=map(int,summary.groups())
  assert (count,suites)==(1,1)
  result.update(executed_App_tests=count,executed_App_suites=suites)
  phases=json.loads((a.output/'actual-client-phases.json').read_text());assert len(phases)==2
  outputs=[f for f in frames if f.get('kind') in ['switch','LED']]
  expected=['ae0301f2ee','ae4101f2ee','ae0301f1ee','ae4101f1ee']
  assert [f['raw'] for f in outputs]==expected,{'actual':outputs,'expected':expected}
  assert [f['variable_at_wire'] for f in outputs]==[{'value':0,'version':1}]*2+[{'value':1,'version':2}]*2
  assert json.loads((a.output/'initial-variable.json').read_text())=={'value':None,'version':0}
  assert variables_at_wire()=={'value':1,'version':2}
  with sqlite3.connect('file:'+str(a.output/'state/automation-tcp.sqlite')+'?mode=ro',uri=True) as db:
   db.row_factory=sqlite3.Row
   journal=[dict(r) for r in db.execute("SELECT j.boot_id,j.command_id,j.device_id,hex(j.intent) AS intent,j.status,j.updated_ms,r.source,r.correlation,r.event_id FROM command_journal j JOIN rule_command r ON r.boot_id=j.boot_id AND r.command_id=j.command_id WHERE j.device_id IN ('light','key:1') ORDER BY j.command_id")]
   admissions=[dict(r) for r in db.execute('SELECT boot_id,correlation,event_id,parent_id,rule_id,admitted_ms FROM rule_admission ORDER BY admitted_ms,event_id')]
   variables=[dict(r) for r in db.execute("SELECT revision,name,kind,value,version FROM logical_variable ORDER BY revision,name")]
  save('journal-with-sources.json',journal);save('rule-admissions.json',admissions);save('final-variables.json',variables)
  assert len(journal)==4 and [x['device_id'] for x in journal]==['light','key:1','light','key:1']
  assert all(x['status'] in ['written','observed'] if x['device_id']=='light' else x['status']=='written' for x in journal)
  suffixes=['conditional/then/light','conditional/then/LED','conditional/else/light','conditional/else/LED']
  assert all(x['source'] and x['source'].endswith(suffix) for x,suffix in zip(journal,suffixes))
  ids=[]
  for i,p in enumerate(phases):
   assert p['request']['on'] is True and p['http_status']==202 and p['receipt']['outcome']=='accepted'
   assert p['receipt']['execution_kind']=='immediate' and p['request']['request_id']==p['receipt']['request_id']
   receipt=p['receipt']['receipt'];commands=[int(x) for x in receipt['commands']];ids+=commands
   assert len(commands)==2 and set(commands)=={x['command_id'] for x in journal[i*2:i*2+2]}
   assert all(x['correlation']==receipt['correlation'] and x['event_id']==int(receipt['event']) for x in journal[i*2:i*2+2])
   assert p['light']['observed']['on']==bool(i) and p['light']['quality']=='Confirmed'
   assert p['LED']['last_written']['color']==['green','red'][i]
   assert p['scene']['selected'] is None
  assert len(set(ids))==4 and len({p['request']['request_id'] for p in phases})==2
  result.update(passed=True,actual_App_operations=2,wire_outputs=4,journal_outputs=4,exact_sources_and_receipt_command_join=True,
   SQL_variable_at_each_wire_real=True,variable_transition=[{'value':None,'version':0},{'value':0,'version':1},{'value':1,'version':2}],
   client_both_clicks_explicit_on_true=True,physical_panel_events_sent=0,actual_WSS_frames=len(json.loads((a.output/'actual-client-wss.json').read_text())))
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

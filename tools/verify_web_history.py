#!/usr/bin/env python3
"""Real HTTP scene-history/restart regression using only synthetic loopback devices."""
import json,pathlib,secrets,socket,sqlite3,subprocess,sys,tempfile,time,urllib.request,urllib.error
repo=pathlib.Path(__file__).resolve().parents[1]
binary=pathlib.Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='homesense-history-http-') as directory:
 root=pathlib.Path(directory);state=root/'state';token=root/'token';secret=secrets.token_urlsafe(32);token.write_text(secret);token.chmod(0o600)
 devices=root/'devices.json';devices.write_bytes((repo/'config/simulation.json').read_bytes())
 def scene(name,offset):return {'id':name,'enabled':True,'review_evidence':'synthetic','max_lateness_ms':1000,'steps':[{'source':'private-source-must-not-leak','offset_ms':offset,'device':'simulation:light','command':{'kind':'level','value':30,'ramp':False}}]}
 rules=root/'rules.json';rules.write_text(json.dumps({'schema_version':1,'configuration_revision':1,'variables':[],'rules':[],'timed_scenes':[scene('delayed',60000),scene('quick',0)]}))
 with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
 origin=f'http://127.0.0.1:{port}'
 def request(path,body=None):
  req=urllib.request.Request(origin+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+secret,'Origin':origin,'Content-Type':'application/json'})
  try:
   with urllib.request.urlopen(req,timeout=10) as response:return response.status,json.load(response)
  except urllib.error.HTTPError as error:return error.code,json.load(error)
 def history(before=None):
  path='/api/v1/scene-history?scene=delayed&revision=1&limit=20'
  if before:path+=f'&before_boot={before["boot"]}&before_run={before["run"]}'
  code,page=request(path);assert code==200,(code,page);return page
 first=None;command_statuses=[]
 for attempt in range(2):
  with (root/f'run-{attempt}.log').open('w') as log:
   process=subprocess.Popen([str(binary),'serve-web-simulation',str(devices),str(rules),str(state),'20',f'127.0.0.1:{port}',str(token),'--administrator'],stdout=log,stderr=log)
   try:
    for _ in range(250):
     if process.poll() is not None:raise AssertionError((root/f'run-{attempt}.log').read_text()[-2000:])
     try:
      if request('/health/ready')==(200,{'ready':True}):break
     except (urllib.error.URLError,ConnectionError):pass
     time.sleep(.02)
    else:raise AssertionError('startup timeout')
    if attempt==0:
     for n in range(21):
      code,result=request('/api/v1/scene-actions',{'scene':'delayed','request_id':f'start-{n}'});assert code==202 and result['outcome']=='accepted',(code,result)
      code,result=request('/api/v1/scene-cancellations',{'start_request_id':f'start-{n}','request_id':f'cancel-{n}'});assert result['outcome']=='cancelled',(code,result)
     code,result=request('/api/v1/scene-actions',{'scene':'quick','request_id':'quick'});assert code==202 and result['outcome']=='accepted',(code,result)
    page=history();assert len(page['runs'])==20 and page['next_before'] is not None
    older=history(page['next_before']);assert len(older['runs'])==1 and older['next_before'] is None
    identities={(r['boot'],r['run']) for r in page['runs']+older['runs']};assert len(identities)==21
    assert all(r['status']=='Cancelled' for r in page['runs']+older['runs'])
    if first is None:first=(page,older)
    else:assert first==(page,older),'restart changed persisted history'
    code,quick=request('/api/v1/scene-history?scene=quick&revision=1');assert code==200 and len(quick['runs'])==1
    run=quick['runs'][0];path=f'/api/v1/runs/{run["boot"]}/{run["run"]}/details?revision=1&kind=commands'
    for _ in range(100):
     code,detail=request(path)
     if code==200 and detail['commands'] and detail['commands'][0]['status'] in (('Written','Observed') if attempt==0 else (('Observed',) if command_statuses[0]=='Observed' else ('Uncertain',))):break
     time.sleep(.02)
    else:raise AssertionError((code,detail))
    assert len(detail['commands'])==1 and detail['redacted'] is False
    command_statuses.append(detail['commands'][0]['status'])
    assert 'private-source-must-not-leak' not in json.dumps(detail)
    assert request('/api/v1/scene-history?scene=delayed&revision=2')[0]==409
    process.wait(timeout=25);assert process.returncode==0
   finally:
    if process.poll() is None:process.terminate();process.wait(timeout=5)
  with sqlite3.connect(state/'automation-simulation.sqlite') as db:
   assert db.execute('SELECT count(*) FROM scene_run').fetchone()[0]==22
   assert db.execute('SELECT count(*) FROM command_journal').fetchone()[0]==1
   assert db.execute('SELECT status FROM command_journal').fetchone()[0] ==command_statuses[-1].lower()
 print(json.dumps({'history_pages':[20,1],'cancelled_runs':21,'written_commands':1,'restart_replay':'none','private_source':'omitted','command_statuses':command_statuses}))

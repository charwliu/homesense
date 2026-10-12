#!/usr/bin/env python3
"""Real loopback HTTP publication regression; synthetic devices only."""
import hashlib, json, pathlib, secrets, socket, sqlite3, subprocess, sys, tempfile, time, urllib.request, urllib.error, uuid
repo=pathlib.Path(__file__).resolve().parents[1]
binary=pathlib.Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='homesense-publication-') as directory:
 root=pathlib.Path(directory);state=root/'state';secret=secrets.token_urlsafe(32)
 token=root/'token';token.write_text(secret);token.chmod(0o600)
 devices=json.loads((repo/'config/simulation.json').read_text())
 original_name=devices['devices'][0]['name']
 rules={'schema_version':1,'configuration_revision':1,'variables':[],'rules':[],'timed_scenes':[{'id':'demo','enabled':True,'review_evidence':'synthetic test','max_lateness_ms':1000,'steps':[{'source':'fixture','offset_ms':60000,'device':'simulation:light','command':{'kind':'level','value':30,'ramp':False}}]}]}
 device_file=root/'devices.json';rule_file=root/'rules.json'
 device_file.write_text(json.dumps(devices));rule_file.write_text(json.dumps(rules))
 with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
 origin=f'http://127.0.0.1:{port}'
 def request(path,body=None):
  req=urllib.request.Request(origin+path,data=None if body is None else json.dumps(body).encode(),headers={'Authorization':'Bearer '+secret,'Origin':origin,'Content-Type':'application/json'})
  try:
   with urllib.request.urlopen(req,timeout=25) as response:return response.status,json.load(response)
  except urllib.error.HTTPError as e:return e.code,json.load(e)
 activation=None;draft_id=str(uuid.uuid4());restore_id=str(uuid.uuid4());restore_activation=None
 for attempt in range(2):
  command=[str(binary),'serve-web-simulation',str(device_file),str(rule_file),str(state),'15',f'127.0.0.1:{port}',str(token),'--administrator']
  with (root/f'run-{attempt}.log').open('w') as log:
   process=subprocess.Popen(command,stdout=log,stderr=log)
   try:
    for _ in range(250):
     if process.poll() is not None:raise AssertionError((root/f'run-{attempt}.log').read_text()[-1500:])
     try:
      if request('/api/v1/devices')[0]==200:break
     except (urllib.error.URLError,ConnectionError):pass
     time.sleep(.02)
    else:raise AssertionError('listener startup timeout')
    if attempt==0:
     assert request('/api/v1/scene-actions',{'scene':'demo','request_id':'before-publication'})[1]['outcome']=='accepted'
     devices['revision']=2;devices['devices'][0]['name']='Renamed simulation light';rules['configuration_revision']=2
     assert request('/api/v1/config/drafts/'+draft_id,{'expected_revision':1,'revision':2,'devices':devices,'automation':rules})[0]==200
     code,preview=request('/api/v1/config/previews/'+draft_id);assert code==200
     assert len(preview['impact']['interrupted_runs'])==1
     activation={'request_id':'activate-once','boot':preview['impact']['boot']}
     blocker=state/'publication-backups';blocker.write_text('block backup directory')
     failed={'request_id':'failed-backup','boot':activation['boot']}
     assert request('/api/v1/config/activations/'+draft_id,failed)[1]['outcome']=='unknown'
     assert request('/api/v1/devices')[1]['revision']==1
     blocker.unlink()
     code,result=request('/api/v1/config/activations/'+draft_id,activation);assert code==202 and result['outcome']=='activated',(code,result)
     code,snapshot=request('/api/v1/devices');assert code==200 and snapshot['revision']==2 and snapshot['devices'][0]['name']=='Renamed simulation light'
     assert request('/api/v1/config/activations/'+draft_id,{'request_id':'different-key','boot':activation['boot']})[0]==409
    code,duplicate=request('/api/v1/config/activations/'+draft_id,activation);assert code==200 and duplicate['revision']==2,(code,duplicate)
    if attempt==0:
     restore={'id':restore_id,'expected_revision':'2','revision':'3'}
     for _ in range(2):
      code,result=request('/api/v1/config/versions/1/restore-draft',restore);assert code==200 and result['active'] is False,(code,result)
     assert request('/api/v1/devices')[1]['revision']==2
     code,source=request('/api/v1/config/drafts/'+restore_id+'/source');assert code==200
     assert source['expected_revision']=='2' and source['revision']=='3'
     assert json.loads(source['devices'])['devices'][0]['name']==original_name
     code,preview=request('/api/v1/config/previews/'+restore_id);assert code==200
     restore_activation={'request_id':'restore-once','boot':preview['impact']['boot']}
     code,result=request('/api/v1/config/activations/'+restore_id,restore_activation);assert code==202 and result['outcome']=='activated',(code,result)
    code,snapshot=request('/api/v1/devices');assert code==200 and snapshot['revision']==3 and snapshot['devices'][0]['name']==original_name,(code,snapshot)
    code,duplicate=request('/api/v1/config/activations/'+restore_id,restore_activation);assert code==200 and duplicate['revision']==3,(code,duplicate)
    # Exercise the actual management endpoint against daily and publication files.
    for _ in range(100):
     code,catalog=request('/api/v1/backups');assert code==200,(code,catalog)
     if all(group['entries'] for group in catalog['groups']):break
     time.sleep(.02)
    else:raise AssertionError('daily or publication snapshot missing')
    assert catalog['verified'] is False and str(state) not in json.dumps(catalog)
    inspected=[]
    for group in catalog['groups']:
     for item in group['entries']:
      assert item['regular_file'] is True
      folder=state/'publication-backups' if group['kind']=='publication' else state/'daily-backups'/'automation-simulation.sqlite'
      snapshot=folder/item['name'];before=hashlib.sha256(snapshot.read_bytes()).hexdigest()
      code,checked=request('/api/v1/backups/inspect',{'kind':group['kind'],'name':item['name']})
      assert code==200 and checked['verified'] is True,(code,checked)
      assert checked['active_revision'] in ('1','2')
      assert hashlib.sha256(snapshot.read_bytes()).hexdigest()==before,'inspection changed snapshot'
      inspected.append((group['kind'],item['name']))
    if attempt==0:first_inspected=inspected
    else:assert inspected==first_inspected,'restart replaced same-day backup catalog'
    assert request('/api/v1/backups/inspect',{'kind':'daily','name':'../automation-simulation.sqlite'})[0]==400
    assert request('/api/v1/backups/inspect',{'kind':'daily','name':'missing.sqlite'})[0]==503
    assert request('/api/v1/devices')[1]['revision']==3,'inspection activated old configuration'
    if attempt==1:
     edit={'id':str(uuid.uuid4()),'expected_revision':'3','revision':'4','device':'simulation:light','name':'Edited through metadata form','room':None}
     code,result=request('/api/v1/config/drafts/'+restore_id+'/device-metadata',edit);assert code==200 and result['active'] is False,(code,result)
     code,edited=request('/api/v1/config/drafts/'+edit['id']+'/source');assert code==200
     assert json.loads(edited['devices'])['devices'][0]['name']==edit['name']
     assert request('/api/v1/devices')[1]['devices'][0]['name']==original_name
     edit['expected_revision']='2';assert request('/api/v1/config/drafts/'+restore_id+'/device-metadata',edit)[0]==409
     archive_path='/api/v1/config/drafts/'+restore_id+'/archive'
     assert request(archive_path, {})[0]==400
     # Archive requires an empty POST body, not a JSON object.
     req=urllib.request.Request(origin+archive_path,data=b'',headers={'Authorization':'Bearer '+secret,'Origin':origin})
     for _ in range(2):
      with urllib.request.urlopen(req,timeout=10) as response:assert response.status==200 and json.load(response)['archived'] is True
     assert request('/api/v1/config/drafts/'+restore_id+'/source')[0]==404
     assert request('/api/v1/config/previews/'+restore_id)[0]==404
     assert all(d['id']!=restore_id for d in request('/api/v1/config/drafts')[1]['drafts'])
     assert request('/api/v1/devices')[1]['revision']==3
    assert request('/health/ready')==(200,{'ready':True})
    process.wait(timeout=20);assert process.returncode==0
   finally:
    if process.poll() is None:process.terminate();process.wait(timeout=5)
  with sqlite3.connect(state/'automation-simulation.sqlite') as db:
   assert db.execute('SELECT count(*) FROM scene_run').fetchone()[0]==1
   assert db.execute('SELECT status FROM scene_run').fetchone()[0]=='cancelled'
   for table,field in [('configuration_revision','revision'),('automation_revision','configuration_revision')]:
    old=json.loads(db.execute(f'SELECT payload FROM {table} WHERE revision=1').fetchone()[0])
    restored=json.loads(db.execute(f'SELECT payload FROM {table} WHERE revision=3').fetchone()[0])
    old[field]=3;assert restored==old

   # Read authoritative published bytes for explicit restart; do not replace or replay them.
   device_file.write_bytes(db.execute('SELECT payload FROM configuration_revision WHERE revision=3').fetchone()[0])
   rule_file.write_bytes(db.execute('SELECT payload FROM automation_revision WHERE revision=3').fetchone()[0])
  backups=list((state/'publication-backups').glob('*.sqlite'));assert len(backups)==2
  backup_versions=[]
  for backup in backups:
   with sqlite3.connect(backup) as db:
    backup_versions.append(db.execute('SELECT max(revision) FROM configuration_revision').fetchone()[0])
  assert sorted(backup_versions)==[1,2]

 print(json.dumps({'publication':'passed','restart_replay':'none','backups':2,'scene_runs':1,'historical_restore':'passed','restored_revision':3,'http_backup_inspections':len(first_inspected)*2,'snapshot_bytes':'unchanged'}))

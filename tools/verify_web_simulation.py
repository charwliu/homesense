#!/usr/bin/env python3
"""Loopback-only management restart regression; no physical gateway access."""
import tempfile,pathlib,json,subprocess,socket,time,urllib.request,urllib.error,sqlite3,secrets,sys,shutil
repo=pathlib.Path(__file__).resolve().parents[1]
binary=pathlib.Path(sys.argv[1]).resolve() if len(sys.argv)==2 else repo/"target/debug/homesensed"
root=pathlib.Path(tempfile.mkdtemp(prefix='homesense-web-smoke-'))
token=secrets.token_urlsafe(32); tokenfile=root/'token';tokenfile.write_text(token);tokenfile.chmod(0o600)
rules={'schema_version':1,'configuration_revision':1,'variables':[],'rules':[],'timed_scenes':[{'id':'demo','enabled':True,'review_evidence':'isolated simulation','max_lateness_ms':1000,'steps':[{'source':'test','offset_ms':60000,'device':'simulation:light','command':{'kind':'level','value':30,'ramp':False}}]}]}
(root/'rules.json').write_text(json.dumps(rules))
with socket.socket() as sock:
 sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
origin=f'http://127.0.0.1:{port}'
cmd=[str(binary),'serve-web-simulation',str(repo/'config/simulation.json'),str(root/'rules.json'),str(root/'state'),'8',f'127.0.0.1:{port}',str(tokenfile)]
def get(path, data=None, auth=True, cross=False):
 req=urllib.request.Request(origin+path,data=json.dumps(data).encode() if data else None,headers={'Authorization':f'Bearer {token}' if auth else 'Bearer invalid','Origin':'http://evil.invalid' if cross else origin,'Content-Type':'application/json'})
 try:
  with urllib.request.urlopen(req,timeout=2) as r:return r.status,r.read()
 except urllib.error.HTTPError as e:return e.code,e.read()
results=[]
for attempt in range(2):
 with open(root/f'run{attempt}.log','w') as log:
  p=subprocess.Popen(cmd,stdout=log,stderr=log)
  try:
   for _ in range(200):
    if p.poll() is not None:raise RuntimeError((root/f'run{attempt}.log').read_text()[-1000:])
    try:
     code,html=get('/');assert code==200 and b'HomeSense' in html;break
    except (urllib.error.URLError,ConnectionError) as error:
     last_error=repr(error);time.sleep(.03)
   else:raise RuntimeError('listener startup timeout: '+last_error)
   assert get('/api/v1/scenes',auth=False)[0]==401
   assert get('/api/v1/scene-actions',{'scene':'demo','request_id':'x'*5000})[0]==413
   assert get('/api/v1/scene-actions',{'scene':'demo','request_id':'same'},cross=True)[0]==403
   code,body=get('/api/v1/scene-actions',{'scene':'demo','request_id':'same'})
   result=json.loads(body);assert result['outcome']=='accepted',(code,result)
   assert code==(202 if attempt==0 else 200)
   results.append(result)
   assert get('/api/v1/scenes')[0]==200
   assert p.wait(timeout=12)==0
  finally:
   if p.poll() is None:p.terminate();p.wait()
assert results[0]==results[1]
tokenfile.chmod(0o644)
assert subprocess.run(cmd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=5).returncode != 0
tokenfile.chmod(0o600)
nonlocal_cmd=cmd.copy();nonlocal_cmd[-2]='192.0.2.1:8088'
assert subprocess.run(nonlocal_cmd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=5).returncode != 0
(root/'state'/'web-controller-id').unlink()
assert subprocess.run(cmd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=5).returncode != 0
with sqlite3.connect(root/'state'/'automation-simulation.sqlite') as db:
 assert db.execute('select count(*) from scene_run').fetchone()[0]==1
print(json.dumps({'http_restart_replay':'passed','runs':1,'unauthorized':401,'cross_origin':403,'static_ui':'served'}))

shutil.rmtree(root)

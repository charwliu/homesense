#!/usr/bin/env python3
"""Local HTTPS/WSS regression using an ephemeral CA; never installs trust globally."""
import threading
import base64, hashlib, http.client, json, os, pathlib, secrets, socket, ssl, struct, subprocess, sys, tempfile, time
repo=pathlib.Path(__file__).resolve().parents[1];binary=pathlib.Path(sys.argv[1]).resolve()
combined=len(sys.argv)==3 and sys.argv[2]=='--tcp-combined'
if len(sys.argv)>3 or (len(sys.argv)==3 and not combined):raise SystemExit('usage: verify_web_tls.py BINARY [--tcp-combined]')
with tempfile.TemporaryDirectory(prefix='homesense-tls-') as directory:
 root=pathlib.Path(directory)
 def openssl(*args):subprocess.run(['openssl',*map(str,args)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 config=root/'ca.cnf';config.write_text('[req]\ndistinguished_name=dn\n[dn]\nCN=HomeSense test CA\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n')
 openssl('req','-x509','-newkey','rsa:2048','-nodes','-days','1','-subj','/CN=HomeSense test CA','-config',config,'-extensions','ca','-keyout',root/'ca.key','-out',root/'ca.pem')
 openssl('req','-newkey','rsa:2048','-nodes','-subj','/CN=localhost','-keyout',root/'server.key','-out',root/'server.csr')
 ext=root/'server.ext';ext.write_text('basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n')
 openssl('x509','-req','-in',root/'server.csr','-CA',root/'ca.pem','-CAkey',root/'ca.key','-CAcreateserial','-days','1','-extfile',ext,'-out',root/'server.pem')
 for name in ['server.key','ca.key']:(root/name).chmod(0o600)
 token=secrets.token_urlsafe(32);(root/'token').write_text(token);(root/'token').chmod(0o600)
 with socket.socket() as listener:listener.bind(('127.0.0.1',0));port=listener.getsockname()[1]
 origin=f'https://localhost:{port}'
 command=[str(binary),'serve-web-tls-simulation',str(repo/'config/simulation.json'),str(repo/'config/automation.simulation.json'),str(root/'state'),'12',f'127.0.0.1:{port}',str(root/'token'),origin,str(root/'server.pem'),str(root/'server.key')]
 if combined:
  bus=socket.socket();bus.bind(('127.0.0.1',0));bus.listen();bus.settimeout(8)
  errors=[];queries=[]
  def gateway():
   try:
    connection,_=bus.accept()
    with connection:
     connection.settimeout(16);buffer=b''
     while True:
      chunk=connection.recv(1024)
      if not chunk:break
      buffer+=chunk
      while len(buffer)>=5:
       frame,buffer=buffer[:5],buffer[5:]
       assert frame==bytes.fromhex('ae15a0f2ee'),('only whole-module Query allowed',frame.hex())
       queries.append(frame)
       # Supply every registered physical channel, even the seven without aliases.
       connection.sendall(b''.join(bytes([0xae,0x15,0xd0+channel,42,0xee]) for channel in range(1,9)))
   except BaseException as error:errors.append(error)
   finally:bus.close()
  worker=threading.Thread(target=gateway,daemon=True);worker.start()
  devices=json.loads((repo/'config/simulation.json').read_text())
  devices['gateways'][0]['endpoint']='127.0.0.1:'+str(bus.getsockname()[1])
  devices['gateways'][0]['modules'][0]['verification']={'status':'verified','evidence':'synthetic loopback fixture','verified_at_unix_ms':1}
  (root/'devices.json').write_text(json.dumps(devices));(root/'rules.json').write_text(json.dumps({'schema_version':1,'configuration_revision':1,'variables':[],'rules':[]}))
  with socket.socket() as listener:listener.bind(('127.0.0.1',0));hap_port=listener.getsockname()[1]
  command=[str(binary),'serve-automation-tcp',str(root/'devices.json'),str(root/'rules.json'),str(root/'state'),'12','--coexistence-confirmed','--hap-listen',f'127.0.0.1:{hap_port}','--web-listen',f'127.0.0.1:{port}','--web-origin',origin,'--web-token',str(root/'token'),'--web-cert',str(root/'server.pem'),'--web-key',str(root/'server.key')]
 context=ssl.create_default_context(cafile=str(root/'ca.pem'))
 def request(method,path,headers):
  connection=http.client.HTTPSConnection('localhost',port,context=context,timeout=3)
  connection.request(method,path,headers={'Origin':origin,**headers});response=connection.getresponse();result=(response.status,dict(response.getheaders()),response.read());connection.close();return result
 with (root/'server.log').open('w') as log:
  process=subprocess.Popen(command,stdout=log,stderr=log)
  try:
   for _ in range(200):
    if process.poll() is not None:raise AssertionError((root/'server.log').read_text()[-1500:])
    try:
     received=request('GET','/',{});last_error=repr(received);
     if received[0]==200:break
     time.sleep(.03)
    except (OSError,ssl.SSLError) as error:last_error=repr(error);time.sleep(.03)
   else:raise AssertionError('TLS startup timeout: '+last_error+' log: '+(root/'server.log').read_text()[-1000:])
   for trust,name in [(ssl.create_default_context(),'localhost'),(context,'wrong.invalid')]:
    try:
     with socket.create_connection(('127.0.0.1',port),timeout=3) as raw:
      with trust.wrap_socket(raw,server_hostname=name):pass
     raise AssertionError('invalid peer certificate accepted')
    except ssl.SSLCertVerificationError:pass
   with socket.create_connection(('127.0.0.1',port),timeout=4) as plain:
    plain.sendall(f'GET / HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n'.encode())
    try:assert not plain.recv(1024).startswith(b'HTTP/')
    except (ConnectionResetError,TimeoutError):pass
   code,headers,body=request('POST','/api/v1/session',{'Authorization':'Bearer '+token});assert code==200
   csrf=json.loads(body)['csrf']
   cookie=headers['set-cookie'];assert 'Secure' in cookie and 'HttpOnly' in cookie;cookie=cookie.split(';')[0]
   code,_,state=request('GET','/api/v1/devices',{'Cookie':cookie});assert code==200
   if combined:
    devices=json.loads(state)['devices'];assert len(devices)==1
    assert devices[0]['quality']=='Confirmed' and devices[0]['observed']['level']==42
    assert request('GET','/health/ready',{'Cookie':cookie})[0]==403  # Member cannot inspect administrator readiness
   key=base64.b64encode(os.urandom(16)).decode()
   with context.wrap_socket(socket.create_connection(('127.0.0.1',port),timeout=3),server_hostname='localhost') as ws:
    ws.sendall(f'GET /api/v1/events HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n'.encode())
    header=b''
    while not header.endswith(b'\r\n\r\n'):header+=ws.recv(1)
    assert header.startswith(b'HTTP/1.1 101')
    accept=base64.b64encode(hashlib.sha1((key+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest());assert accept in header
    payload=b'{"cursor":null}';mask=os.urandom(4);ws.sendall(bytes([0x81,0x80|len(payload)])+mask+bytes(b^mask[i%4] for i,b in enumerate(payload)))
    def exact(count):
     data=b''
     while len(data)<count:
      chunk=ws.recv(count-len(data));assert chunk;data+=chunk
     return data
    head=exact(2);assert head[0]==0x81 and head[1]&0x80==0;size=head[1]&127
    if size==126:size=struct.unpack('!H',exact(2))[0]
    elif size==127:size=struct.unpack('!Q',exact(8))[0]
    assert size<=512*1024
    frame=json.loads(exact(size));assert frame['type']=='state' and frame['reset']
    # Cookie writes require CSRF. A rejected logout must keep this session alive.
    assert request('DELETE','/api/v1/session',{'Cookie':cookie})[0]==401
    assert request('GET','/api/v1/devices',{'Cookie':cookie})[0]==200
    assert request('DELETE','/api/v1/session',{'Cookie':cookie,'X-CSRF-Token':csrf})[0]==200
    assert request('GET','/api/v1/devices',{'Cookie':cookie})[0]==401
    # A pre-authenticated WSS subscription must also observe revocation.
    ws.settimeout(3);deadline=time.monotonic()+3;closed=False
    while time.monotonic()<deadline:
     try:
      header=ws.recv(2)
      if not header:closed=True;break
      if len(header)<2:header+=exact(2-len(header))
      size=header[1]&127
      if size==126:size=struct.unpack('!H',exact(2))[0]
      elif size==127:size=struct.unpack('!Q',exact(8))[0]
      assert size<=512*1024
      if size:exact(size)
      if header[0]&15==8:closed=True;break
     except (ConnectionResetError,ssl.SSLEOFError):closed=True;break
    assert closed,'revoked WSS session remained open'
    assert request('GET','/api/v1/events',{'Cookie':cookie,'Upgrade':'websocket','Connection':'Upgrade','Sec-WebSocket-Version':'13','Sec-WebSocket-Key':key})[0]==401

   if combined:
    hap=http.client.HTTPConnection('127.0.0.1',hap_port,timeout=3);hap.request('GET','/accessories');response=hap.getresponse();assert response.status==401;response.read();hap.close()
   process.wait(timeout=15);assert process.returncode==0
   if combined:
    worker.join(timeout=3);assert not worker.is_alive();assert not errors,errors;assert queries

  finally:
   if process.poll() is None:process.terminate();process.wait(timeout=5)
 (root/'server.key').chmod(0o644)
 assert subprocess.run(command,capture_output=True,timeout=5).returncode!=0
 (root/'server.key').chmod(0o600)
 command[-1]=str(root/'ca.key');assert subprocess.run(command,capture_output=True,timeout=5).returncode!=0
 print(json.dumps({'https':'passed','wss':'passed','csrf_and_session_revocation':'passed','revoked_wss':'closed','untrusted_and_wrong_name':'rejected','plaintext':'rejected','unsafe_key_permissions_and_mismatch':'rejected','mode':'tcp-combined' if combined else 'simulation','whole_module_query_frames':len(queries) if combined else None,'synthetic_query_channels':8 if combined else None,'actuator_frames':0 if combined else None,'readiness_role_boundary':'member_403' if combined else None}))

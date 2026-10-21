"""Verify the single simulation's PTR/SRV/TXT/A records from a VLAN router."""
import socket,struct,time,sys,json
local=sys.argv[1]; instance='HomeSense Simulation._hap._tcp.local'; hostname='homesense-de78bfc478f8.local'
def name(data,offset):
    parts=[]; end=None
    for _ in range(100):
        n=data[offset]; offset+=1
        if n==0: return '.'.join(parts), end or offset
        if n&192==192:
            if end is None: end=offset+1
            offset=((n&63)<<8)|data[offset]; continue
        parts.append(data[offset:offset+n].decode()); offset+=n
    raise ValueError('name pointer loop')
def records(data):
    _,_,qd,an,ns,ar=struct.unpack_from('!6H',data); pos=12
    for _ in range(qd): _,pos=name(data,pos);pos+=4
    for _ in range(an+ns+ar):
        owner,pos=name(data,pos); kind,cls,ttl,length=struct.unpack_from('!HHIH',data,pos);pos+=10
        start=pos;pos+=length
        if kind==12: value=name(data,start)[0]
        elif kind==33: value=(struct.unpack_from('!HHH',data,start)[2],name(data,start+6)[0])
        elif kind==1: value=socket.inet_ntoa(data[start:pos])
        elif kind==16:
            value=[];i=start
            while i<pos: n=data[i];i+=1;value.append(data[i:i+n].decode());i+=n
        else: continue
        yield owner,kind,value
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind((local,0));s.settimeout(1)
s.setsockopt(socket.IPPROTO_IP,socket.IP_MULTICAST_IF,socket.inet_aton(local))
seen={}; end=time.monotonic()+15
while time.monotonic()<end:
    for query,kind in [('_hap._tcp.local',12),(instance,33),(instance,16),(hostname,1)]:
        wire=b''.join(bytes([len(p.encode())])+p.encode() for p in query.split('.'))+b'\0'
        s.sendto(struct.pack('!6H',0,0,1,0,0,0)+wire+struct.pack('!HH',kind,0x8001),('224.0.0.251',5353))
    try:
        packet,_=s.recvfrom(65535)
        for owner,kind,value in records(packet):
            if (kind==12 and value==instance) or owner in (instance,hostname): seen[kind]=value
    except socket.timeout: pass
    if seen.get(12)==instance and seen.get(33)==(51828,hostname) and seen.get(1)=='192.168.3.2' and 'id=DE:78:BF:C4:78:F8' in seen.get(16,[]):break
else: raise AssertionError(seen)
print(json.dumps({'source':local,'instance':seen[12],'address':seen[1],'srv':seen[33],'identity':'DE:78:BF:C4:78:F8','dns_sd_complete':True}))

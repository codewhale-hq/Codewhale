#!/usr/bin/env python3
"""Black-box local ownership contract; only its unique fixture process/files change."""
import json, os, pathlib, signal, subprocess, sys, tempfile, time, urllib.request, urllib.error, uuid
binary = pathlib.Path(sys.argv[1]).resolve()
root = pathlib.Path(tempfile.mkdtemp(prefix='codewhale-shared-contract-'))
env = dict(os.environ, CODEWHALE_PET_HOME=str(root), CODEWHALE_PET_PORT='0')
log = (root/'owner.log').open('ab'); child = None; checks=[]
def start():
    global child
    child = subprocess.Popen([str(binary),'pet','serve'],env=env,stdout=log,stderr=log)
    deadline=time.monotonic()+15
    while time.monotonic()<deadline:
        try:
            d=json.loads((root/'connection.json').read_text())
            value=request(d,'/v1/frame')
            return d,value
        except (OSError,ValueError):
            if child.poll() is not None: raise RuntimeError((root/'owner.log').read_text())
            time.sleep(.05)
    raise RuntimeError('Owner did not become ready')
def request(d,path,body=None,headers=None):
    h={'Authorization':'Bearer '+d['token'],'Content-Type':'application/json'};h.update(headers or {})
    r=urllib.request.Request(f"http://127.0.0.1:{d['port']}{path}",data=None if body is None else json.dumps(body).encode(),headers=h)
    with urllib.request.urlopen(r,timeout=3) as response: return json.load(response)
def frame_when(d,predicate):
    deadline=time.monotonic()+5
    while time.monotonic()<deadline:
        frame=request(d,'/v1/frame')
        if predicate(frame): return frame
        time.sleep(.03)
    raise AssertionError('Accepted state did not reach the frame projection')
def reject(d,path,body=None,headers=None):
    try: request(d,path,body,headers)
    except urllib.error.HTTPError as e:
        assert e.code in (401,409,422),e.code
        return
    raise AssertionError('Invalid request was accepted')
def passed(name): checks.append(name);print('PASS '+name,flush=True)
try:
    d,a=start(); assert len(a['points'])==980
    b=request(d,'/v1/frame?tick='+str(a['tick']))
    assert (a['identity'],a['epoch'],a['tick'],a['digest'])==(b['identity'],b['epoch'],b['tick'],b['digest'])
    passed('two attachments match identity, epoch, tick and digest at one retained frame')
    competing=subprocess.run([str(binary),'pet','serve'],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
    assert competing.returncode!=0 and b'Another pet owner' in competing.stderr
    passed('competing owner cannot open the same world')
    reject(d,'/v1/frame',headers={'Authorization':'Bearer invalid'})
    reject(d,'/v1/frame',headers={'Origin':'https://example.invalid'})
    passed('loopback bearer and browser origin checks reject foreign access')
    client=str(uuid.uuid4()); action={'identity':d['identity'],'client':client,'seq':1,'source_revision':a['sourceRevision'],'action':{'kind':'interact','food':True,'x':.2,'y':-.15}}
    receipt=request(d,'/v1/action',action); repeated=request(d,'/v1/action',action)
    assert repeated['duplicate'] and repeated['cursor']==receipt['cursor']
    reject(d,'/v1/action',dict(action,seq=3));reject(d,'/v1/action',dict(action,action=dict(action['action'],food=False)))
    passed('interaction receipt is durable and idempotent; changed duplicates and gaps rejected')
    select=dict(action,seq=2,action={'kind':'select','source':'contract-source'})
    request(d,'/v1/action',select);a=frame_when(d,lambda f:f['source']=='contract-source')
    producer={'identity':d['identity'],'epoch':a['epoch'],'client':str(uuid.uuid4()),'source':a['source'],'source_revision':a['sourceRevision'],'seq':0,'waiting':False,'events':[]}
    request(d,'/v1/producer',producer)
    # The macOS/Unix owner may miss a 250 ms clock deadline under CI load.
    # Coverage becomes unknown, but a delay shorter than the 2 s producer
    # lease must not discard its sequence and reject the next valid packet.
    child.send_signal(signal.SIGSTOP)
    try: time.sleep(.4)
    finally: child.send_signal(signal.SIGCONT)
    packet=dict(producer,seq=1,events=[{'event':'thinking_started','index':1}]);r=request(d,'/v1/producer',packet)
    assert not r.get('duplicate',False) and r['seq']==1
    passed('a scheduling pause inside the lease preserves the producer sequence')
    assert request(d,'/v1/producer',packet)['duplicate']
    reject(d,'/v1/producer',dict(producer,seq=3));request(d,'/v1/producer',producer)
    reject(d,'/v1/producer',dict(producer,seq=1,events=[{'event':'thinking_started','index':2},{'event':'response_delta','index':2,'content':'PRIVATE'}]))
    request(d,'/v1/producer',dict(producer,seq=1,events=[]))
    time.sleep(.1);assert request(d,'/v1/frame')['producerConnected']
    passed('producer duplicates, gaps and atomic metadata rejection preserve a usable owner')
    request(d,'/v1/action',dict(select,seq=3,source_revision=a['sourceRevision'],action={'kind':'select','source':'other-source'}))
    reject(d,'/v1/producer',dict(producer,seq=2));reject(d,'/v1/action',dict(action,seq=4))
    a=frame_when(d,lambda f:f['source']=='other-source');time.sleep(.4);b=request(d,'/v1/frame')
    assert b['tick']>a['tick'] and b['identity']==a['identity'] and not b['producerConnected']
    passed('source changes reject old producers and actions; clock continues without a view')
    audio_a=str(uuid.uuid4());audio_b=str(uuid.uuid4())
    assert request(d,'/v1/audio',{'client':audio_a,'enabled':True})['granted']
    competitor=request(d,'/v1/audio',{'client':audio_b,'enabled':True})
    if competitor['granted']:
        # Headless hosts can lose the sink and release the first lease before
        # this request. A second grant is valid only if the owner reports the
        # failed device and also releases that lease; it is no audio proof.
        frame_when(d,lambda f:f['audioUnavailable'] and not f['audioOwner'])
        passed('unavailable audio releases its lease and reports device failure (no live audio proof)')
    else:
        passed('only one view can lease the companion audio device')
    request(d,'/v1/audio',{'client':audio_a,'enabled':False})
    request(d,'/v1/audio',{'client':audio_b,'enabled':False})
    before_style=request(d,'/v1/export')
    appearance={'background':[238,239,235],'backgroundTop':[255,255,250],'particle':[32,79,83],'eventColors':False,'brightness':1.4,'dotScale':1.2,'glow':.25,'environment':False,'bodyStyle':'pixel','textLanguage':'ja'}
    style_action={'identity':d['identity'],'client':str(uuid.uuid4()),'seq':1,'source_revision':b['sourceRevision'],'action':{'kind':'appearance','appearance':appearance}}
    reject(d,'/v1/action',dict(style_action,action={'kind':'appearance','appearance':dict(appearance,brightness=999)}))
    request(d,'/v1/action',style_action)
    styled=frame_when(d,lambda f:f['appearance']==appearance)
    assert [styled['style'][k] for k in ['r','g','b']]==appearance['particle']
    assert request(d,'/v1/export')['interactions']==before_style['interactions']
    passed('appearance is validated and durable without adding simulation interactions')
    recording=request(d,'/v1/export');assert 'checkpoint' in recording and 'token' not in recording
    checkpoint=json.loads((root/'habitat.json').read_text());epoch=b['epoch']
    child.kill();child.wait(5);d2,a=start()
    assert d2==d and a['epoch']!=epoch and a['identity']==d['identity']
    assert a['cursor']>=checkpoint['cursor'] and a['tick']>=checkpoint['recording']['checkpoint']['tick']
    assert not a['producerConnected'] and not a['audioOwner']
    assert a['appearance']==appearance
    assert request(d,'/v1/action',dict(select,seq=3,source_revision=b['sourceRevision']-1,action={'kind':'select','source':'other-source'}))['duplicate']
    passed('crash restart restores identity, checkpoint and durable action receipt with fresh unobserved leases')
    habitat=root/'habitat.json';original=habitat.read_bytes();foreign=b'{"external":"fixture writer"}'
    habitat.write_bytes(foreign);time.sleep(1.2);a=request(d,'/v1/frame');assert not a['storageAvailable']
    reject(d,'/v1/action',{'identity':d['identity'],'client':str(uuid.uuid4()),'seq':1,'source_revision':a['sourceRevision'],'action':action['action']})
    assert habitat.read_bytes()==foreign
    habitat.write_bytes(original)
    passed('storage conflict refuses interaction and preserves the external writer')
    print(json.dumps({'checks':len(checks),'passed':checks,'fixture':str(root),'binary':str(binary)},indent=2))
finally:
    if child and child.poll() is None:
        child.send_signal(signal.SIGINT)
        try: child.wait(8)
        except subprocess.TimeoutExpired: child.kill();child.wait()
    log.close()

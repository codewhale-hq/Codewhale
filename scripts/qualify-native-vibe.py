import hashlib,http.client,json,os,secrets,signal,socket,subprocess,sys,time,urllib.parse,platform
from pathlib import Path
out = Path(sys.argv[1]).resolve()
bin = Path(sys.argv[2]).resolve()
out.mkdir(mode=0o700)
root = out
for name in ("work", "runtime", "home", "tmp", "config", "cache", "data", "state"):
    (root / name).mkdir(mode=0o700)
keep = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "SYSTEMDRIVE", "PROCESSOR_ARCHITECTURE"}
env = {name: value for name, value in os.environ.items() if name.upper() in keep}
env.update({
    "PATH": str(bin.parent / "venv" / "bin") + os.pathsep + os.environ["PATH"],
    "HOME": str(out / "home"), "USERPROFILE": str(out / "home"),
    "TMP": str(out / "tmp"), "TEMP": str(out / "tmp"), "TMPDIR": str(out / "tmp"),
    "APPDATA": str(out / "config"), "LOCALAPPDATA": str(out / "data"),
    "XDG_CONFIG_HOME": str(out / "config"), "XDG_CACHE_HOME": str(out / "cache"),
    "XDG_DATA_HOME": str(out / "data"), "XDG_STATE_HOME": str(out / "state"),
    "CODEWHALE_HOME": str(out / "runtime"), "CODEWHALE_TELEMETRY": "0",
    "CODEWHALE_NO_UPDATE_CHECK": "1", "CODEWHALE_DISABLE_MODELS_DEV_FETCH": "1",
    "OPENCODE_DISABLE_AUTOUPDATE": "true", "OPENCODE_DISABLE_LSP_DOWNLOAD": "true",
    "OPENCODE_DISABLE_MODELS_FETCH": "true", "HTTP_PROXY": "http://127.0.0.1:9",
    "HTTPS_PROXY": "http://127.0.0.1:9", "NO_PROXY": "localhost,127.0.0.1",
})
os.environ.clear()
os.environ.update(env)

with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
token=secrets.token_hex(24)
def api(method,path,body=None):
    c=http.client.HTTPConnection('127.0.0.1',port,timeout=100)
    try:
        c.request(method,path,body=json.dumps(body) if body is not None else None,headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        r=c.getresponse();data=r.read()
        return r.status,json.loads(data) if data else None
    finally:c.close()
def stats(path):
    with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
info={'engine':str(bin),'version':subprocess.check_output([bin,'--version'],text=True).strip(),'sha256':stats(bin),'workspace':str(root/'work'),'scratch':str(root),'network':'loopback only','isolatedProfile':True,'platform':platform.platform(),'frozenSha':'555322f9cbbd2c2fa197abe3263dc43f9e6f2532','promptSent':False}
assert info['version']=='codewhale 0.10.3 (555322f9cbbd)'
from http.server import ThreadingHTTPServer,BaseHTTPRequestHandler
import threading
requested=[]
calls=set()
class Handler(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        row=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        content=json.dumps(row.get('messages',[]))
        import re
        phases=re.findall(r'CHECK_(READ|ALLOW|DENY|SECOND|FIRST)',content)
        phase=phases[-1] if phases else 'UNKNOWN'
        marker='reply-to-'+phase
        requested.append({'route':self.path,'model':row.get('model'),'phase':phase,'stream':row.get('stream'),'tools':[x.get('function',{}).get('name') for x in row.get('tools',[])]})
        def packet(delta,finish=None):
            return {'id':'local-qwen-reply','object':'chat.completion.chunk','created':1,'model':row.get('model'),'choices':[{'index':0,'delta':delta,'finish_reason':finish}]}
        first_tool=phase in ('READ','ALLOW','DENY') and phase not in calls
        finish='stop'
        answer=packet({'role':'assistant','content':marker})
        if first_tool:
            func=next(t['function']['name'] for t in row.get('tools',[]) if t['function']['name'].lower().replace('_','') in (('read','readfile') if phase=='READ' else ('write','writefile')))
            tool=next(t['function'] for t in row.get('tools',[]) if t['function']['name']==func)
            properties=tool['parameters']['properties']
            path_key=next(k for k in properties if k in ('path','filePath','file_path','filename','file'))
            target='note.txt' if phase=='READ' else phase.lower()+'.txt'
            kwargs={path_key:str(root/'work'/target)}
            if phase!='READ':kwargs['content']='marker from approved Vibe write'
            requested[-1]['selectedTool']={'tool':func,'arguments':kwargs}
            answer=packet({'role':'assistant','tool_calls':[{'index':0,'id':'call_'+phase,'type':'function','function':{'name':func,'arguments':json.dumps(kwargs)}}]})
            finish='tool_calls'
            calls.add(phase)
        elif phase=='READ':
            observed=any('VIBE-LOCAL-FIXTURE' in str(m.get('content','')) for m in row.get('messages',[]) if m.get('role')=='tool')
            requested[-1]['readResultSeen']=observed
        if row.get('stream'):
            data=''.join('data: '+json.dumps(v)+'\n\n' for v in [answer,packet({},finish),{**packet({},None),'choices':[],'usage':{'prompt_tokens':10,'completion_tokens':4,'total_tokens':14}}])+'data: [DONE]\n\n';mime='text/event-stream'
        else:
            data=json.dumps({'id':'local-qwen-reply','object':'chat.completion','created':1,'model':row.get('model'),'choices':[{'index':0,'message':{'role':'assistant','content':marker},'finish_reason':'stop'}],'usage':{'prompt_tokens':5,'completion_tokens':3,'total_tokens':8}});mime='application/json'
        body=data.encode();self.send_response(200);self.send_header('Content-Type',mime);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
model=ThreadingHTTPServer(('127.0.0.1',0),Handler)
threading.Thread(target=model.serve_forever,daemon=True).start()
cfg=root/'home/.vibe';cfg.mkdir()
endpoint=f'http://127.0.0.1:{model.server_port}/v1'
(cfg/'config.toml').write_text(f'active_model="loopback"\nenable_telemetry=false\n[[providers]]\nname="local"\napi_base="{endpoint}"\napi_style="openai"\nbackend="generic"\n[[models]]\nname="vibe-qualification"\nalias="loopback"\nprovider="local"\n')
def launch(log):
    proc=subprocess.Popen([bin,'app-server','--http','--host','127.0.0.1','--port',str(port),'--auth-token',token],cwd=root/'work',env=env,stdout=(out/log).open('w'),stderr=subprocess.STDOUT,start_new_session=os.name != 'nt')
    for _ in range(100):
        try:
            if api('GET','/health')[0]==200:return proc
        except OSError:time.sleep(.2)
    stop(proc)
    raise RuntimeError('runtime did not start')
def stop(proc):
    if proc and proc.poll() is None:
        proc.terminate()
        try:proc.wait(timeout=8)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()
def ok(method,path,data=None):
    status,msg=api(method,path,data)
    if not 200<=status<300:raise RuntimeError((path,status,msg))
    return msg
def turn(thread,phase):
    for _ in range(80):
        code,msg=api('POST',thread+'/turns',{'prompt':'CHECK_'+phase+' Please reply with the test marker.'})
        if code==409 and 'active turn' in str(msg):
            time.sleep(.2)
            continue
        assert 200<=code<300,(code,msg)
        turn=msg['turn']['id']
        break
    else:raise RuntimeError('previous turn did not release admission')
    for _ in range(180):
        state=ok('GET',thread)
        rows=[t for t in state['turns'] if t['id']==turn]
        if phase in ('READ','ALLOW','DENY') and state['pending_approvals']:
            approval=state['pending_approvals'][0]
            if phase in ('ALLOW','DENY'):assert not (root/'work'/(phase.lower()+'.txt')).exists()
            action='deny' if phase=='DENY' else 'allow'
            delivered=ok('POST','/v1/approvals/'+approval['id'],{'decision':action})
            assert delivered['delivered']
            info.setdefault('approvals',[]).append({'phase':phase,'decision':action,'id':approval['id']})
        if rows and rows[0]['status']!='in_progress':break
        time.sleep(.25)
    else:raise RuntimeError('pending '+phase)
    (out/(phase+'.json')).write_text(json.dumps(state,indent=2)+'\n')
    assert rows[0]['status']=='completed',rows[0]
    if phase!='DENY':assert any(i['kind']=='agent_message' and 'reply-to-'+phase in str(i.get('detail','')) for i in state['items']),[(i['kind'],str(i.get('detail',''))[:50]) for i in state['items']]
    return state['thread']['executor'].get('foreign_session_id')
proc=None
try:
    proc=launch('runtime.log')
    roster=ok('GET','/v1/agents/external')['agents']
    info['agent']=next(r for r in roster if r['id']=='mistral-vibe')
    assert info['agent']['version']=='3.1.4' and info['agent']['runnable']
    info['options']=ok('GET','/v1/agents/external/mistral-vibe/options?'+urllib.parse.urlencode({'cwd':str(root/'work')}))
    assert info['options']['agent']['protocol_version']==1
    t=ok('POST','/v1/threads',{'workspace':str(root/'work'),'permission_posture':'ask','executor':{'kind':'acp','agent_id':'mistral-vibe'}})
    thread='/v1/threads/'+t['id'];info['thread']=t['id']
    (root/'work/note.txt').write_text('VIBE-LOCAL-FIXTURE')
    info['promptSent']=True
    first=turn(thread,'FIRST')
    info['firstForeignSession']=first
    for phase in ['READ','ALLOW','DENY']:
        assert turn(thread,phase)==first
        info.setdefault('completedPhases',[]).append(phase)
    assert any(r.get('readResultSeen') for r in requested)
    assert (root/'work/allow.txt').read_text()=='marker from approved Vibe write'
    assert not (root/'work/deny.txt').exists()
    assert any(a['phase']=='ALLOW' and a['decision']=='allow' for a in info.get('approvals',[]))
    assert any(a['phase']=='DENY' and a['decision']=='deny' for a in info.get('approvals',[]))
    stop(proc);proc=launch('reopen.log')
    ok('POST',thread+'/resume',{})
    second=turn(thread,'SECOND')
    assert first and first==second and {r['phase'] for r in requested}=={'FIRST','READ','ALLOW','DENY','SECOND'}
    info.update(passed=True,promptSent=True,upstream='isolated local Chat Completions fixture',foreignSession=first,foreignSessionRestored=first==second)
except Exception as exc:
    info.update(passed=False,error=str(exc))
    info['readObserved']=any(r.get('readResultSeen') for r in requested)
    info['approvedFileExists']=(root/'work/allow.txt').exists()
    info['deniedFileExists']=(root/'work/deny.txt').exists()
    for name in ('runtime.log','reopen.log'):
        file=out/name
        if file.is_file(): info[name]=file.read_text(errors='replace')[-1400:]
    raise
finally:
    stop(proc);model.shutdown();model.server_close()
    info['modelRequests']=requested
    (out/'receipt.json').write_text(json.dumps(info,indent=2)+'\n')
    print('Vibe Code',info.get('passed'),info.get('foreignSessionRestored'),len(requested),info.get('error',''),flush=True)

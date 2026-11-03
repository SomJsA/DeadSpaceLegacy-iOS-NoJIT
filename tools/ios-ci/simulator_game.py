"""Bounded original-game run using the previously compiled Android-based iOS host."""
from pathlib import Path
import base64,hashlib,json,os,re,shutil,signal,subprocess,sys,time,zipfile
from cryptography.hazmat.primitives.ciphers import Cipher,algorithms,modes
from PIL import Image
from package_host import inspect_macho

ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'apple-build-results';OUT.mkdir(exist_ok=True)
PRIVATE=ROOT/'private-game-input';PRIVATE.mkdir(exist_ok=False)
REPO='SomJsA/DeadSpaceLegacy-iOS-NoJIT'
TAG=os.environ['GAME_RELEASE_TAG'];assert re.fullmatch(r'ios-sim-game-\d{8}-[0-9a-f]{8}',TAG)
RUN=os.environ.get('NATIVE_ARTIFACT_RUN','41614604627');assert RUN.isdigit()
BUNDLE='dev.somjs.deadspacelegacy.nojithost'
EXPECTED='1381cbd7de15bcaf8085edf8f191cc54261c00376087d4893ac62fb9b9f2da1c'
stop=False
def stopping(*_):
    global stop
    stop=True
signal.signal(signal.SIGTERM,stopping);signal.signal(signal.SIGINT,stopping)
# The guest process receives no GitHub token, decryption key or CI credentials.
safe_env={k:v for k,v in os.environ.items() if not any(x in k.upper() for x in ['TOKEN','SECRET','GUEST_KEY','PASSWORD']) and not k.startswith(('GITHUB_','ACTIONS_','RUNNER_'))}
def call(args,timeout=90,apple=False,check=True):
    r=subprocess.run(args,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=timeout,env=safe_env if apple else None)
    if check and r.returncode:raise RuntimeError('Command failed: '+args[0]+' '+args[1]+'\n'+re.sub(r'https?://[^\s)]+','<redacted-download-url>',r.stdout[-1800:]))
    return r.stdout.strip()
def gh(*args,**kw):return call(['gh',*args],**kw)
def sha(path):
    h=hashlib.sha256()
    with Path(path).open('rb') as stream:
        for data in iter(lambda:stream.read(1024*1024),b''):h.update(data)
    return h.hexdigest()
def sim(*args,**kw):return call(['xcrun','simctl',*args],apple=True,**kw)
def tail(path,size=65536):
    if not path.is_file():return ''
    with path.open('rb') as f:f.seek(max(0,path.stat().st_size-size));return f.read().decode('utf-8',errors='replace')
def valid_command(text):
    words=text.split()
    if words in [['release'],['mark'],['probe']]:return True
    if len(words)==4 and words[0]=='touch' and words[1] in ['down','move','up']:
        try:return all(0<=float(x)<=1 for x in words[2:])
        except ValueError:return False
    if not words or words[0] not in ['tap','down','move','up','swipe']:return False
    if len(words)!=(5 if words[0]=='swipe' else 3):return False
    try:
        return all(0<=float(x)<=(480 if i%2==0 else 320) for i,x in enumerate(words[1:]))
    except ValueError:return False

record={'schema':1,'ci_run_id':os.environ.get('GITHUB_RUN_ID','local'),'native_build_run':RUN,'baseline':'android 9cb5f507','native_code_rebuilt':False,'game_present':False,'original_game_test':True,'test_scope':'startup, main menu and first controllable scene; not whole-game acceptance','physical_device_acceptance':False,'session':TAG,'phase':'preparing','observed_stage':'starting','last_control_sequence':0,'last_guest_sequence':0,'errors':[]}
udid=None;container=None;log=None;inbox=None;started=time.time();snapshot=0;pid=None;log_offset=0;dump_dir=None
recorder=None;recorder_log=None;events=[];frame_records=[];saved_frames=set();last_screen_touch=(0.5,0.5)
record['profile']='android-default';record['input_ready']=False
def new_log():
    global log_offset
    if not log or not log.is_file():return ''
    with log.open('rb') as stream:
        stream.seek(log_offset);data=stream.read(4*1024*1024);log_offset=stream.tell()
    text=data.decode('utf-8',errors='replace')
    record['input_ready']=record['input_ready'] or '[DScontrol] ready' in text
    events.extend(line for line in text.splitlines() if any(key in line for key in ['[DScontrol]','[DStouch]','[DSscript]','[DSwindow]','[IOSHost]','[DSnative','panicked at']))
    del events[:-350]
    return text
def collect_drawables():
    if not dump_dir:return
    folder=OUT/'native-frames';folder.mkdir(exist_ok=True)
    dumps=sorted(dump_dir.glob('*.ppm'),key=lambda p:p.stat().st_mtime)
    for ppm in dumps:
        identity=(record['profile'],ppm.name)
        if identity in saved_frames:continue
        if len(frame_records)>=512:continue
        if time.time()-ppm.stat().st_mtime<.2:continue
        name=f"{record['profile']}-{ppm.stem}.png";target=folder/name
        try:
            with Image.open(ppm) as image:image.save(target)
        except OSError:continue # The guest may still be writing its newest dump.
        saved_frames.add(identity)
        frame_records.append({'file':name,'source':ppm.name,'wall_seconds':round(time.time()-started,3),'sha256':sha(target),'bytes':target.stat().st_size})
    for old in dumps[:-2]:
        assert old.resolve().parent==dump_dir.resolve();old.unlink()
def deliver(command):
    global last_screen_touch
    record['last_guest_sequence']+=1;number=record['last_guest_sequence']
    temp=inbox.with_suffix('.tmp');temp.write_text(f'{number} {command}\n',encoding='utf-8');os.replace(temp,inbox)
    deadline=time.monotonic()+8
    while time.monotonic()<deadline:
        text=new_log()
        if f'[DScontrol] sequence={number} queued' in text:
            record['last_guest_ack_sequence']=number
            if command.startswith('touch '):last_screen_touch=tuple(float(v) for v in command.split()[2:])
            time.sleep(.2);return
        if '[DScontrol] normalized touch rejected:' in text:raise RuntimeError('SDL rejected diagnostic Finger event')
        time.sleep(.08)
    raise RuntimeError(f'Diagnostic command {number} was not acknowledged; do not overwrite the mailbox')
def launch_profile(name):
    global pid
    assert name in ['android-default','direct-present']
    options='--fast-present' if name=='direct-present' else ''
    (container/'Documents/touchHLE_options.txt').write_text('# Isolated graphics diagnostic only\ncom.ea.deadspace.bv: '+options+'\n',encoding='utf-8')
    record['profile']=name;record['input_ready']=False
    launched=sim('launch',udid,BUNDLE);record['launch']=launched
    try:pid=int(launched.rsplit(':',1)[1].strip())
    except (ValueError,IndexError):pid=None
def atomic_json(path,obj):
    part=path.with_suffix(path.suffix+'.partial');part.write_text(json.dumps(obj,indent=2)+'\n',encoding='utf-8');os.replace(part,path)
def publish(label):
    global snapshot
    snapshot+=1
    png=OUT/f'game-{snapshot:03d}.png';jpg=OUT/'latest-preview.jpg'
    sim('io',udid,'screenshot',str(png),timeout=30)
    with Image.open(png) as im:
        im=im.convert('RGB');im.thumbnail((1568,1568));im.save(jpg,quality=80)
        if jpg.stat().st_size>350000:im.thumbnail((1200,1200));im.save(jpg,quality=70)
    collect_drawables();optional=[]
    if dump_dir:
        dumps=sorted(dump_dir.glob('*.ppm'),key=lambda p:p.stat().st_mtime)
        if dumps:
            latest=dumps[-1]
            with Image.open(latest) as image:
                rgb=image.convert('RGB');sample=rgb.copy();sample.thumbnail((96,96));values=list(sample.getdata())
                stats={'source':latest.name,'profile':record['profile'],'size':rgb.size,'nonblack_fraction':sum(max(p)>8 for p in values)/len(values),'distinct_sample_colors':len(set(values)),'source_sha256':sha(latest)}
                rgb.thumbnail((1568,1568));rgb.save(OUT/'drawable-preview.jpg',quality=80)
            atomic_json(OUT/'drawable-stats.json',stats);record['drawable']=stats
            optional=[OUT/'drawable-preview.jpg',OUT/'drawable-stats.json']
    record.update(snapshot=snapshot,label=label,wall_seconds=round(time.time()-started,2),screenshot_sha256=sha(png),preview_sha256=sha(jpg),preview_bytes=jpg.stat().st_size,log_bytes=log.stat().st_size if log and log.is_file() else 0)
    processes=sim('spawn',udid,'launchctl','list',timeout=20)
    record['process_alive']=any(BUNDLE in line and line.split()[0].isdigit() for line in processes.splitlines() if line.split())
    if pid:
        value=call(['ps','-p',str(pid),'-o','rss='],check=False)
        record['simulator_process_rss_kib']=value.strip()
    (OUT/'host-log-tail.txt').write_text(tail(log),encoding='utf-8')
    new_log();(OUT/'diagnostic-events.txt').write_text('\n'.join(events)+'\n',encoding='utf-8')
    optional.append(OUT/'diagnostic-events.txt')
    atomic_json(OUT/'game-status.json',record)
    evidence=OUT/'latest-evidence.zip'
    with zipfile.ZipFile(evidence,'w',zipfile.ZIP_DEFLATED) as z:
        for p in [jpg,OUT/'game-status.json',OUT/'host-log-tail.txt',*optional]:z.write(p,p.name)
    gh('release','upload',TAG,str(evidence),'--repo',REPO,'--clobber',timeout=60)
    if label=='periodic':png.unlink()
    print(json.dumps({k:record.get(k) for k in ['snapshot','label','wall_seconds','process_alive','observed_stage','last_control_sequence','phase']}),flush=True)
try:
    gh('run','download',RUN,'--repo',REPO,'--name','apple-nojit-build-evidence','--dir',str(PRIVATE/'native'),timeout=180)
    native=PRIVATE/'native';manifest=json.loads((native/'simulator-manifest.json').read_text(encoding='utf-8'))
    assert manifest['baseline_platform']=='android' and manifest['android_baseline_commit']=='9cb5f50775a590c639aabf47907845f06b300e88'
    assert manifest['cpu_features']==['cpu-interpreter']
    record['native_source_commit']=manifest['source_origin']['source_head'];record['host_version']=manifest['version']
    app_root=PRIVATE/'simulator-app';app_root.mkdir()
    with zipfile.ZipFile(native/'simulator-app.zip') as z:
        assert len(z.infolist())<100
        for entry in z.infolist():
            p=Path(entry.filename);assert not p.is_absolute() and '..' not in p.parts
            assert not ((entry.external_attr>>16)&0o170000)==0o120000
        z.extractall(app_root)
    apps=list(app_root.glob('*.app'));assert len(apps)==1;app=apps[0];binary=app/'DSNoJITHost';binary.chmod(0o755)
    record['simulator_macho']=inspect_macho(binary,simulator=True)
    gh('release','download',TAG,'--repo',REPO,'--pattern','guest-input.json','--dir',str(PRIVATE),timeout=60)
    meta=json.loads((PRIVATE/'guest-input.json').read_text(encoding='utf-8'))
    assert meta['plaintext_sha256']==EXPECTED and meta['plaintext_bytes']==532322054
    gh('release','download',TAG,'--repo',REPO,'--pattern',meta['cipher_file'],'--dir',str(PRIVATE),timeout=180)
    encrypted=PRIVATE/meta['cipher_file'];assert sha(encrypted)==meta['cipher_sha256']
    key=base64.b64decode(os.environ.pop('IOS_SIM_GUEST_KEY_B64'));assert len(key)==32
    game=PRIVATE/'DeadSpace.ipa'
    with encrypted.open('rb') as source,game.open('xb') as dest:
        assert source.read(8)==b'DSGUEST1';nonce=source.read(12);source.seek(-16,2);auth=source.read(16);source.seek(20)
        decoder=Cipher(algorithms.AES(key),modes.GCM(nonce,auth)).decryptor();remaining=meta['plaintext_bytes']
        while remaining:
            block=source.read(min(1024*1024,remaining));assert block;remaining-=len(block);dest.write(decoder.update(block))
        dest.write(decoder.finalize())
    del key
    assert sha(game)==EXPECTED;record['game_sha256']=EXPECTED;record['game_present']=True
    runtimes=json.loads(sim('list','runtimes','-j'))['runtimes'];runtimes=[r for r in runtimes if r.get('isAvailable') and 'iOS' in r['identifier']];assert runtimes
    runtime=max(runtimes,key=lambda r:tuple(int(x) for x in r['version'].split('.')))
    types=json.loads(sim('list','devicetypes','-j'))['devicetypes'];pads=[d for d in types if 'iPad' in d['name']]
    preferred=[d for d in pads if '9th generation' in d['name']] or [d for d in pads if '10th generation' in d['name']]
    device=(preferred or pads)[0]
    record.update(simulator_runtime=runtime['name'],simulator_type=device['name'])
    udid=sim('create','DS-Original-Game-Test',device['identifier'],runtime['identifier'])
    sim('boot',udid);sim('bootstatus',udid,'-b',timeout=180)
    call(['codesign','--force','--sign','-','--timestamp=none',str(app)],apple=True)
    sim('install',udid,str(app),timeout=60)
    container=Path(sim('get_app_container',udid,BUNDLE,'data'));docs=container/'Documents';docs.mkdir(exist_ok=True)
    target=docs/'DeadSpace.ipa';assert not target.exists();shutil.copy2(game,target);assert sha(target)==EXPECTED
    inbox=docs/'simulator-touch-inbox.txt';inbox.write_text('0 mark\n',encoding='utf-8');log=docs/'host-log.txt'
    dump_dir=docs/'drawable-dumps';dump_dir.mkdir()
    safe_env['SIMCTL_CHILD_TOUCHHLE_DS_CONTROL_INBOX']=str(inbox)
    safe_env['SIMCTL_CHILD_TOUCHHLE_DUMP_FRAMES']=str(dump_dir)
    safe_env['SIMCTL_CHILD_TOUCHHLE_DS_TOUCH_LOG']='1'
    record['diagnostic_readback_enabled']=True
    video=OUT/f'game-video-{record["ci_run_id"]}.mp4';recorder_log=(OUT/'video-recorder.txt').open('wb')
    recorder=subprocess.Popen(['xcrun','simctl','io',udid,'recordVideo','--codec=h264','--force',str(video)],stdout=recorder_log,stderr=subprocess.STDOUT,env=safe_env)
    time.sleep(1);record['video_started_before_guest']=recorder.poll() is None
    if not record['video_started_before_guest']:raise RuntimeError('Simulator video recorder failed before guest launch')
    launch_profile('android-default')
    record['phase']='guest-running';deadline=time.monotonic()+600;next_capture=time.monotonic()+8
    while not stop and time.monotonic()<deadline:
        collect_drawables()
        text=new_log();record['input_ready']=record['input_ready'] or '[DScontrol] ready' in text
        fatal=next((m for m in ['[IOSHost] core failed:','[IOSHost] panic caught','[INTERP-UNSUPPORTED]','panicked at'] if m in text),None)
        if fatal:
            record['phase']='runtime-failure';record['errors'].append(fatal);publish('runtime-failure');break
        control_dir=PRIVATE/'control';control_dir.mkdir(exist_ok=True)
        try:
            releases=json.loads(gh('api','--method','GET',f'repos/{REPO}/releases','-f','per_page=100','-f','_='+str(time.time_ns()),timeout=20))
            release=next(r for r in releases if r['draft'] and r['tag_name']==TAG)
            prefix='control-'+record['ci_run_id']+'-'
            candidates=[a for a in release['assets'] if a['name'].startswith(prefix) and a['name'].endswith('.json') and a['size']<=4096]
            if candidates:
                newest=max(candidates,key=lambda a:a['name'])
                if newest['name']!=record.get('control_asset'):
                    gh('release','download',TAG,'--repo',REPO,'--pattern',newest['name'],'--dir',str(control_dir),'--clobber',timeout=20)
                    record['control_asset']=newest['name']
                control=json.loads((control_dir/newest['name']).read_text(encoding='utf-8'))
            else:
                control={'schema':1,'session':TAG,'run_id':record['ci_run_id'],'sequence':0,'commands':[]}
            assert control.get('run_id')==record['ci_run_id']
            assert control.get('schema')==1 and control.get('session')==TAG
            sequence=control.get('sequence');assert isinstance(sequence,int) and 0<=sequence<10000
            if sequence>record['last_control_sequence']:
                commands=control.get('commands',[]);assert isinstance(commands,list) and len(commands)<=16
                assert all(isinstance(c,str) and len(c)<100 and valid_command(c) for c in commands)
                if commands and not record['input_ready']:
                    record['input_waiting_for_core_ready']=True
                else:
                    restart=control.get('restart_profile')
                    if restart is not None:
                        assert restart in ['android-default','direct-present']
                        sim('terminate',udid,BUNDLE,timeout=20,check=False)
                        if log.is_file():log_offset=log.stat().st_size
                        launch_profile(restart);record['observed_stage']='starting'
                    for command in commands:
                        deliver(command)
                    record['last_control_sequence']=sequence
                    observed=control.get('observed_stage')
                    if observed in ['starting','headphone-screen','main-menu','chapter-intro','first-controllable-scene','runtime-failure']:record['observed_stage']=observed
                    if control.get('finish') is True:record['phase']='operator-finished';stop=True
                    publish('control-'+str(sequence));next_capture=time.monotonic()+15
        except (RuntimeError,ValueError,AssertionError) as error:
            record['control_transport_last_error']=str(error)[:300]
        if time.monotonic()>=next_capture:
            publish('periodic');next_capture=time.monotonic()+15
            if not record['process_alive']:record['phase']='process-exited';break
        time.sleep(3)
    if record['phase']=='guest-running':record['phase']='bounded-window-ended'
except Exception as error:
    record['phase']='harness-or-runtime-failure';record['errors'].append(str(error)[:2200]);raise
finally:
    if inbox:
        try:
            deliver(f'touch up {last_screen_touch[0]} {last_screen_touch[1]}');deliver('release')
        except (OSError,RuntimeError):pass
    if udid:
        try:publish('final')
        except Exception as error:record['final_snapshot_error']=str(error)[:500]
        if recorder:
            if recorder.poll() is None:
                recorder.send_signal(signal.SIGINT)
                try:recorder.wait(timeout=20)
                except subprocess.TimeoutExpired:recorder.terminate();recorder.wait(timeout=5)
            if recorder_log:recorder_log.close()
            if video.is_file() and video.stat().st_size:
                record['video']={'asset':video.name,'bytes':video.stat().st_size,'sha256':sha(video),'audio':False,'timing':'simulator wall clock; not a physical-device performance measurement'}
                try:gh('release','upload',TAG,str(video),'--repo',REPO,timeout=90)
                except Exception as error:record['video_upload_error']=str(error)[:400]
        if frame_records:
            frames_zip=OUT/f'native-frames-{record["ci_run_id"]}.zip'
            with zipfile.ZipFile(frames_zip,'w',zipfile.ZIP_STORED) as archive:
                archive.writestr('frames.json',json.dumps({'native_size':'guest drawable; orientation preserved','sample_interval_native_presents':120,'frames':frame_records},indent=2))
                for frame in frame_records:archive.write(OUT/'native-frames'/frame['file'],frame['file'])
            record['native_frames']={'asset':frames_zip.name,'count':len(frame_records),'bytes':frames_zip.stat().st_size,'sha256':sha(frames_zip)}
            try:gh('release','upload',TAG,str(frames_zip),'--repo',REPO,timeout=90)
            except Exception as error:record['frames_upload_error']=str(error)[:400]
        # Save diagnostics before deleting the ephemeral simulator's container.
        if log and log.is_file():
            (OUT/'simulator-game-host-log.txt').write_text(tail(log,8*1024*1024),encoding='utf-8')
            with log.open('rb') as stream:head=stream.read(512*1024)
            (OUT/'host-log-head.txt').write_bytes(head)
        if container:
            for ppm in sorted((container/'Documents/color-probes').glob('*.ppm'))[:30]:
                with Image.open(ppm) as image:image.save(OUT/('probe-'+ppm.stem+'.png'))
        sim('terminate',udid,BUNDLE,timeout=20,check=False)
        sim('shutdown',udid,timeout=30,check=False);sim('delete',udid,timeout=30,check=False)
    if log and log.is_file():
        text=tail(log,8*1024*1024);(OUT/'simulator-game-host-log.txt').write_text(text,encoding='utf-8');record['host_log_tail_limit_bytes']=8*1024*1024
    crash_dir=Path.home()/'Library/Logs/DiagnosticReports'
    for p in crash_dir.glob('DSNoJITHost*.ips'):
        if p.stat().st_mtime>=started-1:shutil.copy2(p,OUT/p.name)
    record['wall_seconds']=round(time.time()-started,2);atomic_json(OUT/'game-status.json',record)
    print(json.dumps({k:record.get(k) for k in ['phase','observed_stage','wall_seconds','errors']}),flush=True)
if record['errors'] or record['observed_stage']!='first-controllable-scene':
    raise SystemExit(1)

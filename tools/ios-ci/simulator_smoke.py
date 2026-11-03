"""Launch the complete HLE host in an isolated ephemeral iPad simulator."""
import json
from pathlib import Path
import subprocess
import time
import shutil

def smoke(root,app):
    root=Path(root);out=root/'apple-build-results'
    def cmd(args):
        result=subprocess.run(args,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
        if result.returncode:raise RuntimeError(' '.join(args)+'\n'+result.stdout[-3000:])
        return result.stdout.strip()
    runtimes=json.loads(cmd(['xcrun','simctl','list','runtimes','-j']))['runtimes']
    available=[r for r in runtimes if r.get('isAvailable') and 'iOS' in r['identifier']]
    if not available:raise RuntimeError('No installed iOS simulator runtime; no paid/download fallback was started')
    runtime=max(available,key=lambda r:tuple(int(n) for n in r['version'].split('.')))
    types=json.loads(cmd(['xcrun','simctl','list','devicetypes','-j']))['devicetypes']
    devices=[d for d in types if 'iPad' in d['name']]
    preferred=[d for d in devices if '10th generation' in d['name']]
    device=(preferred or devices)[0]
    udid=cmd(['xcrun','simctl','create','DS-NoJIT-CI',device['identifier'],runtime['identifier']])
    result={'simulator_runtime':runtime['name'],'simulator_type':device['name'],'game_present':False,'full_game_test':False}
    started=time.time()
    try:
        cmd(['xcrun','simctl','boot',udid]);cmd(['xcrun','simctl','bootstatus',udid,'-b'])
        cmd(['codesign','--force','--sign','-','--timestamp=none',str(app)])
        cmd(['xcrun','simctl','install',udid,str(app)])
        result['launch']=cmd(['xcrun','simctl','launch',udid,'dev.somjs.deadspacelegacy.nojithost'])
        time.sleep(12)
        processes=cmd(['xcrun','simctl','spawn',udid,'launchctl','list'])
        result['process_alive']=any('dev.somjs.deadspacelegacy.nojithost' in line and line.split()[0].isdigit() for line in processes.splitlines() if line.split())
        container=Path(cmd(['xcrun','simctl','get_app_container',udid,'dev.somjs.deadspacelegacy.nojithost','data']))
        log=container/'Documents/host-log.txt'
        text=log.read_text(encoding='utf-8',errors='replace') if log.is_file() else ''
        (out/'simulator-host-log.txt').write_text(text,encoding='utf-8')
        print('=== SIMULATOR HOST LOG (bounded tail) ===',flush=True)
        print(text[-16000:],flush=True)
        print('=== END SIMULATOR HOST LOG ===',flush=True)
        cmd(['xcrun','simctl','io',udid,'screenshot',str(out/'simulator-app-picker.png')])
        try:
            from PIL import Image
        except ImportError:
            import sys
            target=root/'.ci-image-qa'
            cmd([sys.executable,'-m','pip','install','--quiet','--target',str(target),'Pillow==12.0.0'])
            sys.path.insert(0,str(target))
            from PIL import Image
        with Image.open(out/'simulator-app-picker.png') as screenshot:
            pixels=screenshot.convert('RGB');pixels.thumbnail((96,144))
            samples=list(pixels.getdata());count=len(samples)
            result['image_nonblack_fraction']=sum(max(p)>8 for p in samples)/count
            result['image_bright_fraction']=sum(max(p)>96 for p in samples)/count
            result['image_distinct_colors']=len(set(samples))
            result['image_chromatic_fraction']=sum(max(p)-min(p)>24 and max(p)>64 for p in samples)/count
            result['visible_ui']=result['image_nonblack_fraction']>0.005 and result['image_bright_fraction']>0.0005 and result['image_distinct_colors']>=8 and result['image_chromatic_fraction']>0.0005
        result['host_log_written']=bool(text)
        result['interpreter_selected']='CPU backend: interpreter' in text
        result['error_in_log']=any(word in text for word in ['[IOSHost] core failed:','[IOSHost] panic caught','[IOSHost] setup failed:','panicked at'])
        if not result['host_log_written'] or result['error_in_log'] or not result['process_alive'] or not result['visible_ui']:
            raise RuntimeError('Simulator host reported a startup failure; see simulator-host-log.txt')
    finally:
        crash_dir=Path.home()/'Library/Logs/DiagnosticReports'
        for crash in crash_dir.glob('DSNoJITHost*.ips'):
            if crash.stat().st_mtime>=started-1:
                shutil.copy2(crash,out/crash.name)
        (out/'simulator-smoke.json').write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
        subprocess.run(['xcrun','simctl','shutdown',udid],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        subprocess.run(['xcrun','simctl','delete',udid],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)

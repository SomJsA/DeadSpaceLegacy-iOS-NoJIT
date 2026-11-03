"""Exercise the real harness mailbox helpers without starting or importing CI code."""
import ast
from pathlib import Path
import tempfile

source=Path(__file__).with_name('simulator_game.py').read_text(encoding='utf-8')
tree=ast.parse(source)
helpers=ast.Module(body=[node for node in tree.body if isinstance(node,ast.FunctionDef) and node.name in ['new_log','deliver']],type_ignores=[])
class Clock:
    def __init__(self,inbox,log):self.now=0.;self.inbox=inbox;self.log=log;self.acked=[];self.enabled=True
    def monotonic(self):return self.now
    def sleep(self,seconds):
        self.now+=seconds
        if not self.enabled or not self.inbox.exists():return
        number=int(self.inbox.read_text().split()[0])
        if number in self.acked:return
        self.acked.append(number)
        with self.log.open('a') as f:f.write(f'[DScontrol] sequence={number} queued frame=12\n')
with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as folder:
    base=Path(folder);log=base/'log.txt';inbox=base/'inbox.txt'
    log.write_text('[DScontrol] ready; existing command ignored; publish a new sequence\n')
    record={'input_ready':False,'last_guest_sequence':0};clock=Clock(inbox,log)
    import os
    scope=dict(log=log,inbox=inbox,log_offset=0,events=[],record=record,time=clock,os=os,last_screen_touch=(.5,.5))
    exec(compile(helpers,'harness-helpers','exec'),scope)
    scope['new_log']();assert record['input_ready'] # A snapshot must not consume readiness invisibly.
    scope['deliver']('touch down 0.88 0.885');assert record['last_guest_ack_sequence']==1
    scope['deliver']('touch up 0.88 0.885');assert clock.acked==[1,2]
    assert scope['last_screen_touch']==(.88,.885)
    clock.enabled=False
    try:scope['deliver']('probe')
    except RuntimeError as error:assert 'not acknowledged' in str(error)
    else:raise AssertionError('Unacknowledged command silently accepted')
    assert record['last_guest_ack_sequence']==2
    assert inbox.read_text().startswith('3 probe')
print('PASS: readiness survives snapshot reads; ordered down/up ACKs; timeout is explicit and does not overwrite an unacknowledged mailbox')

# Capture must retain a file that the guest may still be writing.
import hashlib,time
from PIL import Image
capture=ast.Module(body=[node for node in tree.body if isinstance(node,ast.FunctionDef) and node.name=='collect_drawables'],type_ignores=[])
with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as folder:
    base=Path(folder);dump=base/'reference';full=base/'full';out=base/'out'
    for p in [dump,full,out]:p.mkdir()
    for number in range(1,5):
        target=full/f'present_{number:05d}_f{number:06d}.ppm'
        Image.new('RGB',(16,12),(number,40,80)).save(target)
        os.utime(target,(time.time()-2,time.time()-2) if number==1 else (time.time()+10,time.time()+10))
    record={'profile':'android-default'};frames=[];saved=set()
    scope=dict(OUT=out,dump_dir=dump,all_dump_dir=full,frame_bytes=0,record=record,frame_records=frames,saved_frames=saved,time=time,started=time.time(),Image=Image,sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest())
    exec(compile(capture,'capture-helpers','exec'),scope)
    scope['collect_drawables']();assert len(frames)==1
    assert len(list(full.glob('*.ppm')))==3,'Unfinished files were discarded'
    for p in full.glob('*.ppm'):os.utime(p,(time.time()-2,time.time()-2))
    scope['collect_drawables']();assert len(frames)==4
    assert {p['kind'] for p in frames}=={'native-present-with-corner-counter'}
    for row in frames:
        number=int(row['source'].split('_')[1])
        with Image.open(out/'native-frames'/row['file']) as image:assert image.getpixel((0,0))==(number,40,80)
print('PASS: full native-frame collection preserves pixels and retains in-progress files until complete')

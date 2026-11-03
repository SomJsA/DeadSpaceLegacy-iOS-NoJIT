"""Central Apple-toolchain checks for the complete no-JIT HLE core."""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'apple-build-results'
OUT.mkdir(exist_ok=True)

def run(args, label, env=None, cwd=ROOT):
    started = time.time()
    with (OUT / (label + '.log')).open('w', encoding='utf-8') as log:
        p = subprocess.Popen(args, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in p.stdout:
            log.write(line)
            print(line, end='', flush=True)
        code = p.wait()
    result = dict(label=label, command=args, exit_code=code, seconds=round(time.time()-started, 2))
    (OUT / (label + '.json')).write_text(json.dumps(result, indent=2)+'\n', encoding='utf-8')
    if code:
        raise SystemExit(code)
    return result

parser = argparse.ArgumentParser()
parser.add_argument('--stage', choices=['check', 'device-library', 'integrated', 'simulator'], default='check')
args = parser.parse_args()
if sys.platform != 'darwin':
    raise SystemExit('This stage requires real macOS with Xcode and the Apple iOS SDK.')
sdk = subprocess.check_output(['xcrun','--sdk','iphoneos','--show-sdk-path'], text=True).strip()
clang = subprocess.check_output(['xcrun','--sdk','iphoneos','--find','clang'], text=True).strip()
clangxx = subprocess.check_output(['xcrun','--sdk','iphoneos','--find','clang++'], text=True).strip()
assert (Path(sdk) / 'usr/include/stdio.h').is_file(), 'Real SDK headers unavailable'
env = os.environ.copy()
env.update(SDKROOT=sdk, IPHONEOS_DEPLOYMENT_TARGET='15.0', CMAKE_GENERATOR='Ninja',
           CMAKE_TOOLCHAIN_FILE=str(ROOT/'tools/ios-ci/Toolchain.cmake'),
           CARGO_TARGET_DIR=str(ROOT/'apple-target'),
           CC_aarch64_apple_ios=clang, CXX_aarch64_apple_ios=clangxx,
           CMAKE_POLICY_VERSION_MINIMUM='3.5')
frameworks = ['AudioToolbox','AVFoundation','CoreAudio','CoreBluetooth','CoreFoundation',
              'CoreGraphics','CoreHaptics','CoreMotion','Foundation','GameController',
              'Metal','OpenGLES','QuartzCore','UIKit']
flags = []
for framework in frameworks:
    flags += ['-C','link-arg=-framework','-C','link-arg='+framework]
env.pop('RUSTFLAGS', None)
env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(flags)
metadata = {'host':platform.platform(),'sdk':sdk,'deployment_target':'15.0',
            'cpu_backend':'interpreter','jit':False,'game_data_uploaded':False,
            'stage':args.stage,'device_runtime':'not tested'}
(OUT/'environment.json').write_text(json.dumps(metadata, indent=2)+'\n', encoding='utf-8')
run(['rustc','-vV'], 'rust-version')
run(['xcodebuild','-version'], 'xcode-version')
if args.stage != 'simulator':
    run(['cargo','test','--release','--manifest-path','src/cpu/arm_interpreter/Cargo.toml'], 'portable-core-tests')
if args.stage in ('integrated','simulator'):
    from package_host import package, audit_resources
    from simulator_smoke import smoke
    audit_resources(ROOT)
    if args.stage == 'integrated':
        run(['cargo','test','--locked','--manifest-path','tools/ios-ci/resource-cache-tests/Cargo.toml'], 'resource-cache-tests')
    env['CARGO_BUILD_JOBS']='2'
    for target, sdk_name in [('aarch64-apple-ios','iphoneos'),('aarch64-apple-ios-sim','iphonesimulator')]:
        if args.stage=='simulator' and sdk_name=='iphoneos':
            continue
        active=env.copy()
        active['SDKROOT']=subprocess.check_output(['xcrun','--sdk',sdk_name,'--show-sdk-path'],text=True).strip()
        compiler=subprocess.check_output(['xcrun','--sdk',sdk_name,'--find','clang'],text=True).strip()
        compilerxx=subprocess.check_output(['xcrun','--sdk',sdk_name,'--find','clang++'],text=True).strip()
        key=target.replace('-','_')
        active['CC_'+key]=compiler;active['CXX_'+key]=compilerxx
        run(['cargo','build','--locked','--bin','touchHLE','--profile','ios-integration','--target',target,
             '--no-default-features','--features','static,cpu-interpreter'],target+'-full-host',active)
        app=package(ROOT,ROOT/'apple-target'/target/'ios-integration/touchHLE',target.endswith('-sim'))
        if target.endswith('-sim'):
            try:
                smoke(ROOT,app)
            finally:
                import shutil
                shutil.make_archive(str(OUT/'simulator-app'),'zip',root_dir=app.parent,base_dir=app.name)
    print('Integrated device package and simulator launch checks complete; real iPad game execution is still pending.')
    raise SystemExit(0)
cmd = ['cargo', 'check' if args.stage=='check' else 'build', '--locked', '--lib',
       '--target','aarch64-apple-ios','--no-default-features','--features','static,cpu-interpreter']
if args.stage != 'check':
    cmd += ['--release']
run(cmd, 'complete-ios-core-'+args.stage, env)
if args.stage == 'device-library':
    library = ROOT/'apple-target/aarch64-apple-ios/release/libtouchHLE.dylib'
    if library.exists():
        import shutil
        shutil.copy2(library, OUT/library.name)
        run(['xcrun','otool','-L',str(library)], 'device-library-dependencies')
print('Complete requested Apple-toolchain stage passed. This is not a device or game acceptance result.')

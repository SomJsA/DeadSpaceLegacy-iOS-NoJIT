"""Package the complete native host, validate its binary and expose limitations."""
from pathlib import Path
import hashlib
import json
import plistlib
import re
import shutil
import stat
import struct
import zipfile

VERSION='0.3.0'
BUILD='4'
BUNDLE_ID='dev.somjs.deadspacelegacy.nojithost'

def audit_resources(root):
    root=Path(root)
    font_names=sorted(set(re.findall(r'Self::from_resource_file\("([^"\n]+)"\)',(root/'src/font.rs').read_text(encoding='utf-8'))))
    for name in font_names:
        file=root/'touchHLE_fonts'/name
        assert file.is_file(), 'Required HLE font missing: '+name
        with file.open('rb') as stream:magic=stream.read(4)
        assert magic in (b'\x00\x01\x00\x00',b'OTTO',b'ttcf'), 'Invalid font header: '+name
    dylibs=sorted((root/'touchHLE_dylibs').glob('*.dylib'))
    assert dylibs
    for file in dylibs:
        with file.open('rb') as stream:magic=stream.read(4)
        assert magic in (b'\xce\xfa\xed\xfe',b'\xca\xfe\xba\xbe',b'\xbe\xba\xfe\xca'), 'Invalid guest support library: '+file.name
    result={'required_fonts':font_names,'fonts_checked':len(font_names),'support_libraries_checked':len(dylibs),'status':'passed'}
    out=root/'apple-build-results';out.mkdir(exist_ok=True)
    (out/'resource-audit.json').write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    return result

def inspect_macho(path, simulator=False):
    b=Path(path).read_bytes()
    h=struct.unpack_from('<8I',b)
    assert h[0]==0xfeedfacf and h[1]==0x0100000c and h[3]==2, 'Expected ARM64 executable'
    assert not h[6]&0x20000, 'Executable stack flag is forbidden'
    offset=32;segments=[];libraries=[];build=None;signature=False
    for _ in range(h[4]):
        cmd,size=struct.unpack_from('<II',b,offset)
        assert size>=8 and offset+size<=32+h[5]
        if cmd==0x19:
            name=b[offset+8:offset+24].split(b'\0')[0].decode()
            vm,vmbytes,start,length,maxprot,initprot,sections,flags=struct.unpack_from('<4Q4I',b,offset+24)
            assert not (maxprot&2 and maxprot&4) and not (initprot&2 and initprot&4), 'W+X segment'
            assert start+length<=len(b)
            segments.append(dict(name=name,maximum_protection=maxprot,initial_protection=initprot))
        elif cmd==0x32:
            platform,minimum,sdk,tools=struct.unpack_from('<4I',b,offset+8)
            assert platform==(7 if simulator else 2), 'Wrong Apple platform'
            assert minimum==0xf0000, 'Minimum OS differs from 15.0'
            build=dict(platform=platform,minimum_os='15.0',sdk=f'{sdk>>16}.{(sdk>>8)&255}.{sdk&255}')
        elif cmd in (0xc,0x80000018,0x8000001f,0x80000023):
            start=struct.unpack_from('<I',b,offset+8)[0]
            name=b[offset+start:offset+size].split(b'\0')[0].decode()
            assert name.startswith(('/System/Library/','/usr/lib/')), 'Unbundled library: '+name
            libraries.append(name)
        elif cmd==0x1d:signature=True
        offset+=size
    assert build and offset==32+h[5]
    return dict(architectures=['arm64'],segments=segments,build=build,system_libraries=libraries,code_signature_load_command=signature,sha256=hashlib.sha256(b).hexdigest(),bytes=len(b))

def add_file(archive,source,name,executable=False):
    item=zipfile.ZipInfo(name,date_time=(2026,11,3,0,0,0));item.create_system=3
    item.external_attr=(stat.S_IFREG|(0o755 if executable else 0o644))<<16
    item.compress_type=zipfile.ZIP_DEFLATED
    archive.writestr(item,Path(source).read_bytes())

def package(root,binary,simulator=False):
    root=Path(root);out=root/'apple-build-results'
    kind='simulator' if simulator else 'device'
    app=out/kind/'Payload/DSNoJITHost.app';app.mkdir(parents=True)
    executable=app/'DSNoJITHost';shutil.copy2(binary,executable);executable.chmod(0o755)
    macho=inspect_macho(executable,simulator)
    info={'CFBundleDevelopmentRegion':'en','CFBundleExecutable':executable.name,
          'CFBundleIdentifier':BUNDLE_ID,'CFBundleInfoDictionaryVersion':'6.0',
          'CFBundleName':'DSNoJITHost','CFBundleDisplayName':'DS NoJIT Host',
          'CFBundlePackageType':'APPL','CFBundleShortVersionString':VERSION,'CFBundleVersion':BUILD,
          'CFBundleSupportedPlatforms':['iPhoneSimulator' if simulator else 'iPhoneOS'],
          'LSRequiresIPhoneOS':True,'MinimumOSVersion':'15.0','UIDeviceFamily':[2],
          'UIFileSharingEnabled':True,'LSSupportsOpeningDocumentsInPlace':True,
          'NSMotionUsageDescription':'Use device tilt as accelerometer input for the legacy game.',
          'UIApplicationSupportsIndirectInputEvents':True,
          'UILaunchScreen':{},'UIRequiresFullScreen':True,
          'UISupportedInterfaceOrientations~ipad':['UIInterfaceOrientationPortrait','UIInterfaceOrientationPortraitUpsideDown','UIInterfaceOrientationLandscapeLeft','UIInterfaceOrientationLandscapeRight']}
    (app/'Info.plist').write_bytes(plistlib.dumps(info,sort_keys=True))
    resources=[]
    for directory in ['touchHLE_fonts','touchHLE_dylibs']:
        resources += [f for f in (root/directory).rglob('*') if f.is_file()]
    resources += [root/'touchHLE_default_options.txt']
    assert len(resources)<=128 and sum(f.stat().st_size for f in resources)<=64*1024*1024
    with zipfile.ZipFile(app/'host-resources.zip','x') as archive:
        for f in sorted(resources):add_file(archive,f,f.relative_to(root).as_posix())
    with zipfile.ZipFile(app/'host-resources.zip') as archive:assert archive.testzip() is None
    ipa=None
    if not simulator:
        ipa=out/f'DS-NoJIT-iPad-Host-{VERSION}-unsigned.ipa'
        with zipfile.ZipFile(ipa,'x') as archive:
            for f in sorted(app.iterdir()):add_file(archive,f,'Payload/DSNoJITHost.app/'+f.name,f.name==executable.name)
        with zipfile.ZipFile(ipa) as archive:
            assert archive.testzip() is None and len(archive.namelist())==3
            assert plistlib.loads(archive.read('Payload/DSNoJITHost.app/Info.plist'))==info
    manifest={'version':VERSION,'build':BUILD,'bundle_id':BUNDLE_ID,'device_family':[2],
              'minimum_os_version':'15.0','developer_signed':False,'requires_local_signing':not simulator,
              'provisioning_profile':False,'entitlements':{},'jit':False,'wx_segments':False,
              'embedded_native_frameworks':[],'embedded_native_dylibs':[],
              'hle_support_libraries':'ARM32 guest support data in host-resources.zip, never native executable code',
              'cpu_features':['cpu-interpreter'],'game_ipa_bundled':False,
              'macho':macho,'runtime_validation':'pending','known_limitations':[
              'First complete iOS host attempt; actual iPad game execution unverified',
              'Simulator app-picker smoke is not full-game or real-device acceptance',
              'No claims for game graphics, audio, touch, lifecycle, saves or performance'],
              'acceptance_steps':['Sign this host IPA once on remote Windows','Copy unchanged DeadSpace.ipa into host Documents without replacing existing files','Open DS NoJIT Host; inspect first game screen and sound/touch','Collect host-log.txt and any crash log; keep original IPA and saves']}
    if ipa:manifest.update(file_name=ipa.name,file_bytes=ipa.stat().st_size,sha256=hashlib.sha256(ipa.read_bytes()).hexdigest())
    (out/(kind+'-manifest.json')).write_text(json.dumps(manifest,indent=2)+'\n',encoding='utf-8')
    return app

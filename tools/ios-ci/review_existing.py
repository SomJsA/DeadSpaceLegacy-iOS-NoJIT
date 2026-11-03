"""Requalify the exact previously compiled artifact after a validation-only fix."""
from pathlib import Path
import hashlib,json,os,shutil,subprocess,sys,tempfile,zipfile
from setup_image_qa import assess
from package_host import inspect_macho

root=Path(__file__).resolve().parents[2];out=root/'apple-build-results';out.mkdir(exist_ok=True)
run=os.environ['NATIVE_ARTIFACT_RUN'];assert run.isdigit()
prior=root/'prior-native-artifact';assert not prior.exists()
subprocess.run(['gh','run','download',run,'--repo','SomJsA/DeadSpaceLegacy-iOS-NoJIT','--name','apple-nojit-build-evidence','--dir',str(prior)],check=True)
def read(p):return json.loads(p.read_text(encoding='utf-8'))
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
for p in prior.iterdir():
    assert p.is_file()
    shutil.copy2(p,out/p.name)
manifest=read(prior/'device-manifest.json')
assert manifest['baseline_platform']=='android' and manifest['version']=='0.4.0'
assert manifest['source_origin']['source_head']=='ab82323bee2802e6216089c885d5a128ad533444'
# Every source/resource input that could affect native code must be identical.
checked=0
for relative,expected in manifest['source_origin']['sha256'].items():
    if relative.startswith(('tools/ios-ci/','.github/')):continue
    assert sha(root/relative)==expected,relative
    checked+=1
for label in ['portable-core-tests','resource-cache-tests','aarch64-apple-ios-full-host','aarch64-apple-ios-sim-full-host']:
    assert read(prior/(label+'.json'))['exit_code']==0,label
assert '45 passed' in (prior/'portable-core-tests.log').read_text(encoding='utf-8')
assert '7 passed' in (prior/'resource-cache-tests.log').read_text(encoding='utf-8')
ipa=prior/manifest['file_name'];assert sha(ipa)==manifest['sha256']
with zipfile.ZipFile(ipa) as z:
    assert z.testzip() is None
    with tempfile.TemporaryDirectory(prefix='ds-native-review-') as temp:
        binary=Path(temp)/'DSNoJITHost';binary.write_bytes(z.read('Payload/DSNoJITHost.app/DSNoJITHost'))
        actual=inspect_macho(binary)
    assert actual['sha256']==manifest['macho']['sha256']
smoke=read(prior/'simulator-smoke.json')
assert all(smoke[k] for k in ['process_alive','mobile_setup_ready','host_log_written'])
assert not smoke['error_in_log'] and not smoke['full_game_test'] and not smoke['game_present']
image=prior/'simulator-mobile-setup.png';qa=assess(image);assert qa['accepted'],qa
# Keep the original failed heuristic result intact and disclose the correction.
shutil.copy2(prior/'simulator-smoke.json',out/'simulator-smoke-original.json')
smoke.update(visible_ui=True,image_contract=qa,original_ci_image_gate=False,requalified_without_native_rebuild=True)
(out/'simulator-smoke.json').write_text(json.dumps(smoke,indent=2)+'\n',encoding='utf-8')
proof={'status':'validation-only-requalification-passed','native_build_run':run,'native_source_commit':manifest['source_origin']['source_head'],'native_inputs_rechecked':checked,'native_code_rebuilt':False,'same_ipa_sha256':sha(ipa),'same_screenshot_sha256':sha(image),'image_contract':qa,'reason':'Original PC-picker bright-chromatic threshold rejected the valid dark-blue white-text UIKit setup. Both native builds, process lifetime and logs had passed.','scope':'No actual game was uploaded or run; no physical-device/game acceptance inferred.'}
(out/'VALIDATION-REQUALIFICATION.json').write_text(json.dumps(proof,indent=2)+'\n',encoding='utf-8')
print(json.dumps(proof))

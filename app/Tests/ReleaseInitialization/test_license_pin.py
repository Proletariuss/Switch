#!/usr/bin/env python3
"""Permanent synthetic package trust-root regressions. No real image operation."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

SOURCE=Path(__file__).resolve().parents[3]
BUILD=Path(os.environ['SWITCH_BUILD_DIR']).resolve()
ROOT=Path(os.environ['SWITCH_RELEASE_INIT_TEST_ROOT']).resolve()
assert ROOT.is_relative_to(BUILD) and ROOT!=BUILD
DIGEST='5df07007198989c622f5d41de8d703e7bef3d0e79d62e24332ee739a452af62a'
# Reuse only the synthetic fixture factory, not a fabricated gate result.
spec=importlib.util.spec_from_file_location('bundle_cases',SOURCE/'scripts/test-release-initialization-bundle.py');suite=importlib.util.module_from_spec(spec);spec.loader.exec_module(suite)
factory=suite.GateTests('test_clean');factory.setUp()
helper_spec=importlib.util.spec_from_file_location('bundle_factory',SOURCE/'app/Tests/ReleaseInitialization/make_bundle_fixture.py');helper=importlib.util.module_from_spec(helper_spec);helper_spec.loader.exec_module(helper)
wire=json.dumps({'enabled':True,'telegram':{'enabled':True,'bot_token':'bare-synthetic-telegram','chat_id':'synthetic-chat'},'bark':{'enabled':True,'device_key':'bare-synthetic-bark','encryption_key':'bare-synthetic-encryption'},'message':'synthetic-SMS-body'}).encode()
records=[]
for name in ['foreign-env-legitimate-bundle','foreign-env-matches-resigned-bundle','source-tampered','source-bundle-tampered-resigned']:
    case=ROOT/'license-pin'/name;repo=case/'src';app=case/'Switch.app';repo.parent.mkdir(parents=True,exist_ok=True)
    shutil.copytree(factory.source,repo)
    (repo/'scripts').mkdir()
    for script in ['package-dmg.sh','audit-release-bundle.py','workspace-env.sh']:
        shutil.copy2(SOURCE/'scripts'/script,repo/'scripts'/script)
    helper.build_fixture(repo,app,ROOT/'license-pin/tiny')
    foreign=case/'external-COPYING';foreign.write_bytes(wire)
    notice=app/'Contents/Resources/licenses/libusb-COPYING';fixed=repo/'licenses/libusb-COPYING'
    if name in ['foreign-env-matches-resigned-bundle','source-bundle-tampered-resigned']:
        notice.write_bytes(wire)
        subprocess.run(['/usr/bin/codesign','--force','--sign','-','--timestamp=none',str(app)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    if name.startswith('source-'):fixed.write_bytes(wire)
    signature=subprocess.run(['/usr/bin/codesign','--verify','--deep','--strict',str(app)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    assert signature.returncode==0
    bin=case/'bin';bin.mkdir();marker=case/'hdiutil-called.json'
    (bin/'hdiutil').write_text('''#!/usr/bin/env python3
import hashlib,json,os,pathlib,sys
assert sys.argv[1]=='create'
stage=pathlib.Path(sys.argv[sys.argv.index('-srcfolder')+1])
p=stage/'Switch.app/Contents/Resources/licenses/libusb-COPYING'
record={'notice_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'notice_bytes':p.stat().st_size,'image_operation':False}
pathlib.Path(os.environ['LICENSE_PIN_HDIUTIL_MARKER']).write_text(json.dumps(record))
sys.exit(97)
''');(bin/'hdiutil').chmod(0o755)
    output=case/'never-a-real-image.dmg'
    env=dict(os.environ,PATH=str(bin)+':'+os.environ['PATH'],LIBUSB_LICENSE_FILE=str(foreign),LIBUSB_DYLIB_ARM64=str(case/'foreign/lib/libusb.dylib'),LICENSE_PIN_HDIUTIL_MARKER=str(marker))
    result=subprocess.run(['sh',str(repo/'scripts/package-dmg.sh'),str(app),str(output),'Switch'],capture_output=True,text=True,env=env,timeout=30)
    assert not output.exists() and 'bare-synthetic' not in result.stdout+result.stderr and 'synthetic-SMS-body' not in result.stdout+result.stderr
    if name=='foreign-env-legitimate-bundle':
        assert result.returncode==97 and marker.exists()
        proof=json.loads(marker.read_text());assert proof['notice_sha256']==DIGEST and proof['notice_bytes']==26436 and not proof['image_operation']
        assert hashlib.sha256(notice.read_bytes()).hexdigest()==DIGEST
    else:assert result.returncode==1 and not marker.exists()
    records.append({'case':name,'package_exit':result.returncode,'signature_before_package':0,'hdiutil_stub_called':marker.exists(),'actual_image_operation':False,'output_exists':False})
    print('PASS license pin '+name+' (synthetic; no real image)')
(ROOT/'license-pin/results.json').write_text(json.dumps(records,indent=2)+'\n')

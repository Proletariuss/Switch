#!/usr/bin/env python3
"""Test-only tiny signed App with public source resources; never an actual release."""
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys

def build_fixture(source, app, tiny):
    source=Path(source);app=Path(app);tiny=Path(tiny)
    if not tiny.exists():
        tiny.parent.mkdir(parents=True,exist_ok=True)
        subprocess.run(['/usr/bin/xcrun','clang','-target','arm64-apple-macos27.0','-x','c','-','-o',str(tiny)],input=b'int main(void) { return 0; }\n',check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,timeout=30)
    def copy(src,dst):
        dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,dst)
    files={'AppIcon.icns':'app/Resources/AppIcon.icns','licenses/THIRD_PARTY_NOTICES.md':'THIRD_PARTY_NOTICES.md','licenses/EVS-NOTICE.md':'third_party/evs/NOTICE.md','licenses/EVS-reference-readme.txt':'third_party/evs/readme.txt','licenses/Proletarius-LICENSE':'LICENSE','licenses/ios-carrier-profiles-MIT.txt':'data/carriers/ios-carrier-profiles-MIT.txt','licenses/euicc-go-bertlv-LICENSE':'licenses/euicc-go-bertlv-LICENSE','licenses/opencore-amr-NOTICE':'licenses/opencore-amr-NOTICE'}
    for n in ['LICENSE-MIT','LICENSE-APACHE','COPYRIGHT','COPYRIGHT-library.html']:files['licenses/rust-std-'+n]='licenses/rust-std-'+n
    for n in ['euicc-go','uicc-go','quectel-qmi-go','swu-go','opencore-amr']:files['licenses/'+n+'-LICENSE']='licenses/'+n+'-LICENSE'
    for folder in ['Brand','Ringtones']:
        for p in (source/'app/Resources'/folder).rglob('*'):
            if p.is_file():files[str(p.relative_to(source/'app/Resources'))]=str(p.relative_to(source))
    for p in (source/'licenses/module-voice-supervisor').rglob('*'):
        if p.is_file():files['licenses/module-voice-supervisor/'+str(p.relative_to(source/'licenses/module-voice-supervisor'))]=str(p.relative_to(source))
    for name,src in files.items():copy(source/src,app/'Contents/Resources'/name)
    notice=source/'licenses/libusb-COPYING'
    copy(notice,app/'Contents/Resources/licenses/libusb-COPYING')
    (app/'Contents/Resources/Assets.car').write_bytes(b'public-test-generated-assets')
    info=plistlib.loads((source/'app/Resources/Info.plist').read_bytes())
    deployment=re.findall(r'^    MACOSX_DEPLOYMENT_TARGET: "([0-9]+\.[0-9]+(?:\.[0-9]+)?)"$',(source/'project.yml').read_text(),re.M)[0]
    bindings={'$(EXECUTABLE_NAME)':'Switch','$(PRODUCT_BUNDLE_IDENTIFIER)':'com.proletarius.switch','$(DISPLAY_NAME)':'Switch','$(MARKETING_VERSION)':(source/'VERSION').read_text().strip(),'$(CURRENT_PROJECT_VERSION)':(source/'BUILD_NUMBER').read_text().strip(),'$(MACOSX_DEPLOYMENT_TARGET)':deployment}
    info={k:bindings.get(v,v) if type(v) is str else v for k,v in info.items()}
    info.update({'BuildMachineOSBuild':'26B5112a','CFBundleSupportedPlatforms':['MacOSX'],'DTCompiler':'com.apple.compilers.llvm.clang.1_0','DTPlatformBuild':'26A425','DTPlatformName':'macosx','DTPlatformVersion':'27.0','DTSDKBuild':'26A425','DTSDKName':'macosx27.0','DTXcode':'2710','DTXcodeBuild':'27A9275'})
    (app/'Contents/Info.plist').write_bytes(plistlib.dumps(info));(app/'Contents/PkgInfo').write_bytes(b'APPL????')
    for name in ['Contents/MacOS/Switch','Contents/Resources/backend/switchd-rs','Contents/Resources/backend/libusb-1.0.0.dylib']:
        dst=app/name;copy(tiny,dst);dst.chmod(0o755)
        subprocess.run(['/usr/bin/codesign','--force','--sign','-','--timestamp=none',str(dst)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=20)
    subprocess.run(['/usr/bin/codesign','--force','--sign','-','--timestamp=none',str(app)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=20)
if __name__=='__main__':build_fixture(*sys.argv[1:])

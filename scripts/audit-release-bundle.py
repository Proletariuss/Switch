#!/usr/bin/env python3
"""Fail-closed release bundle inventory. Never reads installed state or Keychain.
Diagnostics contain categories/path digests/counts, never matched bytes.
Identity/signature and mounted-DMG equality remain separate mandatory release gates.
"""
import argparse
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import stat
import sys
import subprocess

class Refusal(Exception):
    def __init__(self, category, relative=''):
        self.category = category
        self.path_sha256 = hashlib.sha256(relative.encode()).hexdigest() if relative else None

def sha(path):
    h=hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda:f.read(1024*1024),b''): h.update(b)
    return h.hexdigest()

def no_metadata(path, label):
    # Darwin query-size calls enumerate presence only, never retrieve values.
    before=path.lstat()
    libc=ctypes.CDLL(None,use_errno=True)
    libc.listxattr.argtypes=[ctypes.c_char_p,ctypes.c_void_p,ctypes.c_size_t,ctypes.c_int]
    libc.listxattr.restype=ctypes.c_ssize_t
    libc.getxattr.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_void_p,ctypes.c_size_t,ctypes.c_uint32,ctypes.c_int]
    libc.getxattr.restype=ctypes.c_ssize_t
    if libc.listxattr(os.fsencode(path),None,0,1) != 0:
        raise Refusal('extended_metadata_present_or_unreadable',label)
    size=libc.getxattr(os.fsencode(path),b'com.apple.ResourceFork',None,0,0,1)
    if size >= 0 or ctypes.get_errno() != 93:  # Darwin ENOATTR only proves absent.
        raise Refusal('resource_fork_present_or_unreadable',label)
    libc.acl_get_link_np.argtypes=[ctypes.c_char_p,ctypes.c_int];libc.acl_get_link_np.restype=ctypes.c_void_p
    libc.acl_valid.argtypes=[ctypes.c_void_p];libc.acl_valid.restype=ctypes.c_int
    libc.acl_get_entry.argtypes=[ctypes.c_void_p,ctypes.c_int,ctypes.POINTER(ctypes.c_void_p)];libc.acl_get_entry.restype=ctypes.c_int
    libc.acl_free.argtypes=[ctypes.c_void_p];libc.acl_free.restype=ctypes.c_int
    acl=libc.acl_get_link_np(os.fsencode(path),0x100)  # Darwin ACL_TYPE_EXTENDED, no symlink following.
    if not acl:
        # Darwin APFS reports ENOENT for an absent extended ACL even on an
        # existing inode. Require the same named inode to remain present.
        acl_error=ctypes.get_errno();after=path.lstat()
        if acl_error==errno.ENOENT and (before.st_dev,before.st_ino,before.st_mode)==(after.st_dev,after.st_ino,after.st_mode): return
        raise Refusal('acl_unreadable',label)
    try:
        if libc.acl_valid(acl)!=0: raise Refusal('acl_invalid',label)
        entry=ctypes.c_void_p();ctypes.set_errno(0)
        result=libc.acl_get_entry(acl,0,ctypes.byref(entry))  # ACL_FIRST_ENTRY
        if result==0: raise Refusal('acl_present',label)
        if result!=-1 or ctypes.get_errno()!=errno.EINVAL: raise Refusal('acl_unreadable',label)
    finally: libc.acl_free(acl)

def libusb_notice(source):
    # A build caller's environment is not a license trust root.
    path=source/'licenses/libusb-COPYING'
    metadata=regular(path,'<source-libusb-notice>')
    if metadata.st_size!=26436 or sha(path)!='5df07007198989c622f5d41de8d703e7bef3d0e79d62e24332ee739a452af62a':
        raise Refusal('source_libusb_notice_pin_mismatch')
    return path

def generated_info(info, original, source):
    def string(name,pattern):
        value=info.get(name)
        if type(value) is not str or re.fullmatch(pattern,value) is None:
            raise Refusal('invalid_generated_info_value')
        return value
    version=(source/'VERSION').read_text().strip()
    build=(source/'BUILD_NUMBER').read_text().strip()
    project=(source/'project.yml').read_text()
    deployment=re.findall(r'^    MACOSX_DEPLOYMENT_TARGET: "([0-9]+\.[0-9]+(?:\.[0-9]+)?)"$',project,re.M)
    if len(deployment)!=1: raise Refusal('unknown_deployment_binding')
    bindings={'$(EXECUTABLE_NAME)':'Switch','$(PRODUCT_BUNDLE_IDENTIFIER)':'com.proletarius.switch','$(DISPLAY_NAME)':'Switch','$(MARKETING_VERSION)':version,'$(CURRENT_PROJECT_VERSION)':build,'$(MACOSX_DEPLOYMENT_TARGET)':deployment[0]}
    for key,value in original.items():
        expected=bindings.get(value,value) if type(value) is str else value
        if type(info.get(key)) is not type(expected) or info.get(key)!=expected:
            raise Refusal('default_info_plist_changed')
    expected={'BuildMachineOSBuild','CFBundleSupportedPlatforms','DTCompiler','DTPlatformBuild','DTPlatformName','DTPlatformVersion','DTSDKBuild','DTSDKName','DTXcode','DTXcodeBuild'}
    if set(info)!=set(original)|expected: raise Refusal('unexpected_or_missing_info_plist_key')
    if info['CFBundleSupportedPlatforms']!=['MacOSX'] or type(info['CFBundleSupportedPlatforms']) is not list:
        raise Refusal('invalid_generated_info_value')
    if info['DTCompiler']!='com.apple.compilers.llvm.clang.1_0' or info['DTPlatformName']!='macosx':
        raise Refusal('invalid_generated_info_value')
    for key in ['BuildMachineOSBuild','DTPlatformBuild','DTSDKBuild','DTXcodeBuild']:
        string(key,r'[0-9]{2,3}[A-Z][0-9]{1,6}[a-z]?')
    platform=string('DTPlatformVersion',r'[0-9]{1,2}\.[0-9]{1,2}(?:\.[0-9]{1,2})?')
    string('DTSDKName',r'macosx[0-9]{1,2}\.[0-9]{1,2}(?:\.[0-9]{1,2})?')
    string('DTXcode',r'[0-9]{4,5}')
    if info['DTSDKName']!='macosx'+platform or info['DTSDKBuild']!=info['DTPlatformBuild']:
        raise Refusal('inconsistent_generated_info_value')

def regular(path, label):
    s=path.lstat()
    if not stat.S_ISREG(s.st_mode) or s.st_nlink != 1:
        raise Refusal('unsafe_file_shape',label)
    return s

def audit(app, source, stage_root=None):
    app=Path(app).absolute(); source=Path(source).absolute()
    # Only explicit caller paths. Never resolve/inspect Application Support or Keychain.
    if app.is_symlink() or not app.is_dir(): raise Refusal('unsafe_app_root')
    no_metadata(app,'<app-root>')
    if stage_root is not None:
        stage=Path(stage_root).absolute()
        if stage.is_symlink() or app.parent!=stage or set(os.listdir(stage))!={'Switch.app','Applications'}:
            raise Refusal('invalid_stage_top_level')
        no_metadata(stage,'<stage-root>')
        link=stage/'Applications'
        if not link.is_symlink() or os.readlink(link)!='/Applications': raise Refusal('invalid_applications_link')
        no_metadata(link,'<applications-link>')
    text=(source/'app/Sources/SMSForwardingStore.swift').read_text()
    for name,literal in [('telegramEnabled','false'),('barkEnabled','false'),('telegramBotToken','""'),('telegramChatID','""'),('telegramProxyURL','""'),('barkDeviceKey','""'),('barkEncryptionKey','""')]:
        if len(re.findall(r'^    @Published var '+name+r' = '+re.escape(literal)+r'\s*$',text,re.M)) != 1:
            raise Refusal('provider_default_not_blank_or_disabled')
    expected={
        'Contents/Info.plist':None,'Contents/PkgInfo':None,
        'Contents/MacOS/Switch':None,'Contents/_CodeSignature/CodeResources':None,
        'Contents/Resources/Assets.car':None,
        'Contents/Resources/backend/switchd-rs':None,
        'Contents/Resources/backend/libusb-1.0.0.dylib':None,
        'Contents/Resources/AppIcon.icns':source/'app/Resources/AppIcon.icns',
    }
    for relative in ['Brand/SwitchBrand.gif'] + ['Ringtones/'+name+'.wav' for name in ['Buoyant','Dreamer','LittleBird','Pond','Pop','Reflected','Surge']]:
        expected['Contents/Resources/'+relative]=source/'app/Resources'/relative
    notices={
        'THIRD_PARTY_NOTICES.md':'THIRD_PARTY_NOTICES.md',
        'EVS-NOTICE.md':'third_party/evs/NOTICE.md',
        'EVS-reference-readme.txt':'third_party/evs/readme.txt',
        'Proletarius-LICENSE':'LICENSE',
        'ios-carrier-profiles-MIT.txt':'data/carriers/ios-carrier-profiles-MIT.txt',
        'euicc-go-bertlv-LICENSE':'licenses/euicc-go-bertlv-LICENSE',
        'opencore-amr-NOTICE':'licenses/opencore-amr-NOTICE',
    }
    for n in ['LICENSE-MIT','LICENSE-APACHE','COPYRIGHT','COPYRIGHT-library.html']:
        notices['rust-std-'+n]='licenses/rust-std-'+n
    for n in ['euicc-go','uicc-go','quectel-qmi-go','swu-go','opencore-amr']:
        notices[n+'-LICENSE']='licenses/'+n+'-LICENSE'
    for name,rel in notices.items(): expected['Contents/Resources/licenses/'+name]=source/rel
    module=source/'licenses/module-voice-supervisor'
    module_files=['block-buffer-0.10.4/LICENSE-APACHE', 'block-buffer-0.10.4/LICENSE-MIT', 'cfg-if-1.0.5/LICENSE-APACHE', 'cfg-if-1.0.5/LICENSE-MIT', 'crypto-common-0.1.7/LICENSE-APACHE', 'crypto-common-0.1.7/LICENSE-MIT', 'digest-0.10.7/LICENSE-APACHE', 'digest-0.10.7/LICENSE-MIT', 'generic-array-0.14.7/LICENSE', 'hmac-0.12.1/LICENSE-APACHE', 'hmac-0.12.1/LICENSE-MIT', 'libc-0.2.190/LICENSE-APACHE', 'libc-0.2.190/LICENSE-MIT', 'llvm-libunwind-LICENSE.TXT', 'manifest.json', 'musl-1.2.5-COPYRIGHT', 'native-runtime-provenance.json', 'sha2-0.10.9/LICENSE-APACHE', 'sha2-0.10.9/LICENSE-MIT', 'subtle-2.6.1/LICENSE', 'typenum-1.20.1/LICENSE', 'typenum-1.20.1/LICENSE-APACHE', 'typenum-1.20.1/LICENSE-MIT', 'version_check-0.9.5/LICENSE-APACHE', 'version_check-0.9.5/LICENSE-MIT']
    for name in module_files:
        p=module/name;regular(p,'<public-module-license>')
        expected['Contents/Resources/licenses/module-voice-supervisor/'+name]=p
    # libusb notice is an explicitly audited build input; it is not user state.
    notice=libusb_notice(source)
    regular(notice,'<libusb-build-notice>')
    expected['Contents/Resources/licenses/libusb-COPYING']=notice
    directories=set()
    for name in expected:
        parent=Path(name).parent
        while str(parent)!='.': directories.add(str(parent)); parent=parent.parent
    seen=set(); count=0; total=0
    patterns=[('telegram_token',re.compile(rb'(?<![A-Za-z0-9_-])[0-9]{5,16}:[A-Za-z0-9_-]{30,128}(?![A-Za-z0-9_-])')),
              ('bark_key_url',re.compile(rb'https?://api\.day\.app/[A-Za-z0-9_-]{8,128}')),
              ('private_key',re.compile(rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----'))]
    for p in sorted(app.rglob('*')):
        rel=p.relative_to(app).as_posix(); metadata=p.lstat()
        no_metadata(p,rel)
        if stat.S_ISDIR(metadata.st_mode):
            if rel not in directories: raise Refusal('unexpected_directory',rel)
            continue
        regular(p,rel)
        if rel not in expected: raise Refusal('unexpected_bundle_file',rel)
        seen.add(rel); count+=1; total+=metadata.st_size
        original=expected[rel]
        if original is not None:
            regular(original,rel)
            if sha(p)!=sha(original): raise Refusal('default_resource_changed',rel)
        with p.open('rb') as f:
            prefix=f.read(4)
            if prefix in (b'SWF1',b'SWF2'): raise Refusal('runtime_queue_envelope',rel)
            f.seek(0); tail=b''
            for chunk in iter(lambda:f.read(1024*1024),b''):
                sample=tail+chunk
                for category,pattern in patterns:
                    if pattern.search(sample): raise Refusal(category,rel)
                tail=sample[-512:]
    if seen != set(expected): raise Refusal('required_bundle_file_missing')
    info=plistlib.loads((app/'Contents/Info.plist').read_bytes())
    original=plistlib.loads((source/'app/Resources/Info.plist').read_bytes())
    generated_info(info,original,source)
    if (app/'Contents/PkgInfo').read_bytes()!=b'APPL????': raise Refusal('invalid_pkginfo')
    resources=plistlib.loads((app/'Contents/_CodeSignature/CodeResources').read_bytes())
    if type(resources) is not dict or set(resources)!={'files','files2','rules','rules2'} or any(type(v) is not dict for v in resources.values()):
        raise Refusal('invalid_signature_resources_schema')
    # Absolute tool, fresh verification, bounded wait, no potentially secret diagnostic bytes.
    verify=subprocess.run(['/usr/bin/codesign','--verify','--deep','--strict',str(app)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=20)
    if verify.returncode!=0: raise Refusal('signature_verification_failed')
    # No arbitrary generated-file hash is represented as a data/credential proof.
    # Main/backend Mach-O and Assets.car require the separate release provenance
    # / attestation gate as well as the fresh signature check above.
    return {'result':'PASS_BUNDLE_INITIALIZATION_INVENTORY','regular_files':count,'regular_file_bytes':total,'provider_defaults':'blank_and_disabled','unexpected_files':0,'metadata_entries':0,'runtime_or_credentials_detected':0,'libusb_notice_sha256':sha(notice),'fresh_signature_verified':True,'scope':'Strict inventory/metadata/generated schema/public source binding plus fresh signature; compiled artifacts still require release identity/provenance/attestation and mounted equality. Pattern scan is not a universal secret detector.'}

def main():
    p=argparse.ArgumentParser();p.add_argument('app');p.add_argument('--source-root',default=str(Path(__file__).resolve().parents[1]));p.add_argument('--report');p.add_argument('--stage-root')
    args=p.parse_args()
    try: result=audit(args.app,args.source_root,args.stage_root); code=0
    except Refusal as e: result={'result':'REFUSED','category':e.category,'path_sha256':e.path_sha256};code=1
    except Exception: result={'result':'REFUSED','category':'audit_io_or_parse_failure'};code=1
    output=json.dumps(result,sort_keys=True)+'\n'
    if args.report: Path(args.report).write_text(output)
    sys.stdout.write(output);return code
if __name__=='__main__': sys.exit(main())

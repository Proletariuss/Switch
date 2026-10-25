import ctypes,importlib.util,json,os,plistlib,shutil,subprocess,sys,unittest
from pathlib import Path
R=Path(os.environ['SWITCH_RELEASE_INIT_TEST_ROOT']).resolve()
BUILD=Path(os.environ['SWITCH_BUILD_DIR']).resolve()
assert R.is_relative_to(BUILD) and R != BUILD, 'private workspace fixture root required'
GATE=Path(__file__).resolve().with_name('audit-release-bundle.py')
class GateTests(unittest.TestCase):
 def setUp(self):
  self.base=R/'fixtures/bundle-audit'/self._testMethodName
  if self.base.exists(): shutil.rmtree(self.base)
  self.source=self.base/'source'; self.app=self.base/'Switch.app'
  def put(path,data=b'default-public-resource'):
   path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(data)
  self.put=put
  for name,literal in [('telegramEnabled','false'),('barkEnabled','false'),('telegramBotToken','""'),('telegramChatID','""'),('telegramProxyURL','""'),('barkDeviceKey','""'),('barkEncryptionKey','""')]:
   p=self.source/'app/Sources/SMSForwardingStore.swift';p.parent.mkdir(parents=True,exist_ok=True)
   with p.open('a') as f:f.write('    @Published var '+name+' = '+literal+'\n')
  resources=['AppIcon.icns','Brand/SwitchBrand.gif']+['Ringtones/'+n+'.wav' for n in ['Buoyant','Dreamer','LittleBird','Pond','Pop','Reflected','Surge']]
  for n in resources:
   put(self.source/'app/Resources'/n);put(self.app/'Contents/Resources'/n)
  notices={'THIRD_PARTY_NOTICES.md':'THIRD_PARTY_NOTICES.md','EVS-NOTICE.md':'third_party/evs/NOTICE.md','EVS-reference-readme.txt':'third_party/evs/readme.txt','Proletarius-LICENSE':'LICENSE','ios-carrier-profiles-MIT.txt':'data/carriers/ios-carrier-profiles-MIT.txt','euicc-go-bertlv-LICENSE':'licenses/euicc-go-bertlv-LICENSE','opencore-amr-NOTICE':'licenses/opencore-amr-NOTICE'}
  for n in ['LICENSE-MIT','LICENSE-APACHE','COPYRIGHT','COPYRIGHT-library.html']:notices['rust-std-'+n]='licenses/rust-std-'+n
  for n in ['euicc-go','uicc-go','quectel-qmi-go','swu-go','opencore-amr']:notices[n+'-LICENSE']='licenses/'+n+'-LICENSE'
  for n,p in notices.items():put(self.source/p);put(self.app/'Contents/Resources/licenses'/n)
  for name in ['block-buffer-0.10.4/LICENSE-APACHE', 'block-buffer-0.10.4/LICENSE-MIT', 'cfg-if-1.0.5/LICENSE-APACHE', 'cfg-if-1.0.5/LICENSE-MIT', 'crypto-common-0.1.7/LICENSE-APACHE', 'crypto-common-0.1.7/LICENSE-MIT', 'digest-0.10.7/LICENSE-APACHE', 'digest-0.10.7/LICENSE-MIT', 'generic-array-0.14.7/LICENSE', 'hmac-0.12.1/LICENSE-APACHE', 'hmac-0.12.1/LICENSE-MIT', 'libc-0.2.190/LICENSE-APACHE', 'libc-0.2.190/LICENSE-MIT', 'llvm-libunwind-LICENSE.TXT', 'manifest.json', 'musl-1.2.5-COPYRIGHT', 'native-runtime-provenance.json', 'sha2-0.10.9/LICENSE-APACHE', 'sha2-0.10.9/LICENSE-MIT', 'subtle-2.6.1/LICENSE', 'typenum-1.20.1/LICENSE', 'typenum-1.20.1/LICENSE-APACHE', 'typenum-1.20.1/LICENSE-MIT', 'version_check-0.9.5/LICENSE-APACHE', 'version_check-0.9.5/LICENSE-MIT']:
   put(self.source/'licenses/module-voice-supervisor'/name);put(self.app/'Contents/Resources/licenses/module-voice-supervisor'/name)
  info=plistlib.dumps({'CFBundleIdentifier':'$(PRODUCT_BUNDLE_IDENTIFIER)','CFBundleExecutable':'$(EXECUTABLE_NAME)','CFBundlePackageType':'APPL','CFBundleVersion':'$(CURRENT_PROJECT_VERSION)','CFBundleShortVersionString':'$(MARKETING_VERSION)','LSMinimumSystemVersion':'$(MACOSX_DEPLOYMENT_TARGET)'})
  put(self.source/'app/Resources/Info.plist',info);put(self.app/'Contents/Info.plist',info)
  for n in ['Contents/PkgInfo','Contents/MacOS/Switch','Contents/Resources/Assets.car','Contents/Resources/backend/switchd-rs','Contents/Resources/backend/libusb-1.0.0.dylib','Contents/Resources/licenses/libusb-COPYING']:put(self.app/n,b'public-synthetic-build-artifact')
  put(self.app/'Contents/_CodeSignature/CodeResources',plistlib.dumps({}))
  shutil.copyfile(Path(__file__).resolve().parents[1]/'licenses/libusb-COPYING',self.source/'licenses/libusb-COPYING')
  put(self.source/'VERSION',b'3.50\n');put(self.source/'BUILD_NUMBER',b'350\n');put(self.source/'project.yml',b'    MACOSX_DEPLOYMENT_TARGET: "27.0"\n')
  helper=Path(__file__).resolve().parents[1]/'app/Tests/ReleaseInitialization/make_bundle_fixture.py'
  spec=importlib.util.spec_from_file_location('release_fixture',helper);module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
  old=os.environ.get('LIBUSB_LICENSE_FILE');os.environ['LIBUSB_LICENSE_FILE']=str(self.source/'COPYING')
  try:module.build_fixture(self.source,self.app,R/'tiny-fixture')
  finally:
   if old is None:os.environ.pop('LIBUSB_LICENSE_FILE',None)
   else:os.environ['LIBUSB_LICENSE_FILE']=old
 def run_gate(self,refuse=False,secret=None,stage=None,extra_env=None):
  p=subprocess.run([sys.executable,str(GATE),str(self.app),'--source-root',str(self.source)]+(['--stage-root',str(stage)] if stage else []),capture_output=True,text=True,timeout=30,env=dict(os.environ,**(extra_env or {})))
  self.assertEqual(p.returncode,1 if refuse else 0,p.stdout+p.stderr)
  if secret:self.assertNotIn(secret,p.stdout+p.stderr)
  return json.loads(p.stdout)
 def test_clean(self):self.run_gate()
 def test_forwarding_config(self):self.put(self.app/'Contents/Resources/forwarding.json',b'{"bot_token":"synthetic"}');self.run_gate(True,'synthetic')
 def test_queue(self):self.put(self.app/'Contents/Resources/state.enc',b'SWF1synthetic');self.run_gate(True,'synthetic')
 def test_keychain(self):self.put(self.app/'Contents/Resources/login.keychain-db');self.run_gate(True)
 def test_runtime_dir(self):(self.app/'Contents/Resources/runtime').mkdir();self.run_gate(True)
 def test_unknown_symlink(self):(self.app/'Contents/Resources/user-data').symlink_to(self.source);self.run_gate(True)
 def test_known_symlink(self):
  p=self.app/'Contents/Resources/AppIcon.icns';p.unlink();p.symlink_to(self.source/'app/Resources/AppIcon.icns');self.run_gate(True)
 def test_telegram_pattern(self):
  s='123456789:'+('Z'*36);self.put(self.app/'Contents/MacOS/Switch',s.encode());r=self.run_gate(True,s);self.assertEqual(r['category'],'telegram_token')
 def test_bark_pattern(self):
  s='https://api.day.app/'+('Q'*32);self.put(self.app/'Contents/Resources/Assets.car',s.encode());r=self.run_gate(True,s);self.assertEqual(r['category'],'bark_key_url')
 def test_private_key(self):
  s='-----BEGIN PRIVATE KEY-----';self.put(self.app/'Contents/MacOS/Switch',s.encode());self.run_gate(True,s)
 def test_enabled_default(self):
  p=self.source/'app/Sources/SMSForwardingStore.swift';p.write_text(p.read_text().replace('telegramEnabled = false','telegramEnabled = true'));self.run_gate(True)
 def test_token_default(self):
  p=self.source/'app/Sources/SMSForwardingStore.swift';p.write_text(p.read_text().replace('telegramBotToken = ""','telegramBotToken = "synthetic"'));self.run_gate(True,'synthetic')
 def test_resource_changed(self):self.put(self.app/'Contents/Resources/Brand/SwitchBrand.gif',b'changed');self.run_gate(True)
 def test_info_unknown_key(self):self.put(self.app/'Contents/Info.plist',plistlib.dumps({'CFBundleIdentifier':'com.proletarius.switch','telegram.botToken':'synthetic'}));self.run_gate(True,'synthetic')
 def mutate_info(self,key,value):
  p=self.app/'Contents/Info.plist';x=plistlib.loads(p.read_bytes());x[key]=value;p.write_bytes(plistlib.dumps(x))
 def metadata(self,path,name):
  lib=ctypes.CDLL(None,use_errno=True);lib.setxattr.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_void_p,ctypes.c_size_t,ctypes.c_uint32,ctypes.c_int];lib.setxattr.restype=ctypes.c_int
  secret=b'synthetic-bare-bark-key-and-sms-body'
  self.assertEqual(lib.setxattr(os.fsencode(path),name,secret,len(secret),0,0),0)
 def test_root_xattr(self):self.metadata(self.app,b'com.switch.synthetic');self.run_gate(True,'synthetic-bare-bark-key-and-sms-body')
 def test_directory_xattr(self):self.metadata(self.app/'Contents/Resources',b'com.switch.synthetic');self.run_gate(True)
 def test_regular_xattr(self):self.metadata(self.app/'Contents/MacOS/Switch',b'com.switch.synthetic');self.run_gate(True)
 def test_resource_fork(self):self.metadata(self.app/'Contents/MacOS/Switch',b'com.apple.ResourceFork');self.run_gate(True,'synthetic-bare-bark-key-and-sms-body')
 def test_acl_root(self):subprocess.run(['/bin/chmod','+a','everyone deny write',str(self.app)],check=True);self.run_gate(True)
 def test_acl_file(self):subprocess.run(['/bin/chmod','+a','everyone deny write',str(self.app/'Contents/MacOS/Switch')],check=True);self.run_gate(True)
 def test_libusb_unbound_config(self):self.put(self.app/'Contents/Resources/licenses/libusb-COPYING',b'{"bark":{"enabled":true,"device_key":"bare-synthetic-key"}}');self.run_gate(True,'bare-synthetic-key')
 def test_pkginfo(self):self.put(self.app/'Contents/PkgInfo',b'bare-synthetic-bark-key');self.run_gate(True,'bare-synthetic-bark-key')
 def test_generated_dicts(self):
  for key in ['BuildMachineOSBuild','CFBundleSupportedPlatforms','DTCompiler','DTPlatformBuild','DTPlatformName','DTPlatformVersion','DTSDKBuild','DTSDKName','DTXcode','DTXcodeBuild','LSMinimumSystemVersion']:
   with self.subTest(key=key):
    original=(self.app/'Contents/Info.plist').read_bytes();self.mutate_info(key,{'device_key':'bare-synthetic-key'});self.run_gate(True,'bare-synthetic-key');(self.app/'Contents/Info.plist').write_bytes(original)
 def test_generated_arrays(self):self.mutate_info('DTCompiler',['bare-synthetic-key']);self.run_gate(True,'bare-synthetic-key')
 def test_compiler_text(self):self.mutate_info('DTCompiler','bare-synthetic-key');self.run_gate(True,'bare-synthetic-key')
 def test_sdk_mismatch(self):self.mutate_info('DTSDKName','macosx26.0');self.run_gate(True)
 def test_signature_dictionary(self):self.put(self.app/'Contents/_CodeSignature/CodeResources',plistlib.dumps({'bark':{'device_key':'bare-synthetic-key'}}));self.run_gate(True,'bare-synthetic-key')
 def test_signature_content_tamper(self):self.put(self.app/'Contents/Resources/Assets.car',b'bare-synthetic-key');self.run_gate(True,'bare-synthetic-key')
 def test_brand_ignored_env(self):self.put(self.source/'app/Resources/Brand/.env',b'bare-synthetic-key');self.put(self.app/'Contents/Resources/Brand/.env',b'bare-synthetic-key');self.run_gate(True,'bare-synthetic-key')
 def test_ringtone_ignored_key(self):self.put(self.source/'app/Resources/Ringtones/private.key',b'bare-synthetic-key');self.put(self.app/'Contents/Resources/Ringtones/private.key',b'bare-synthetic-key');self.run_gate(True,'bare-synthetic-key')
 def module_extra(self,name):self.put(self.source/'licenses/module-voice-supervisor'/name,b'bare-synthetic-key');self.put(self.app/'Contents/Resources/licenses/module-voice-supervisor'/name,b'bare-synthetic-key');self.run_gate(True,'bare-synthetic-key')
 def test_module_extra_env(self):self.module_extra('.env')
 def test_module_extra_key(self):self.module_extra('private.key')
 def test_module_extra_json(self):self.module_extra('private.json')
 def stage(self):
  root=self.base/'stage';root.mkdir();shutil.move(str(self.app),str(root/'Switch.app'));self.app=root/'Switch.app';(root/'Applications').symlink_to('/Applications');return root
 def test_stage_positive(self):root=self.stage();self.run_gate(stage=root)
 def test_stage_extra_file(self):root=self.stage();self.put(root/'private.json',b'bare-synthetic-key');self.run_gate(True,'bare-synthetic-key',stage=root)
 def test_stage_wrong_link(self):root=self.stage();(root/'Applications').unlink();(root/'Applications').symlink_to('/tmp');self.run_gate(True,stage=root)
 def test_stage_root_xattr(self):root=self.stage();self.metadata(root,b'com.switch.synthetic');self.run_gate(True,stage=root)
 def sign_again(self):subprocess.run(['/usr/bin/codesign','--force','--sign','-','--timestamp=none',str(self.app)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 def malicious_notice(self):
  return json.dumps({'enabled':True,'telegram':{'enabled':True,'bot_token':'bare-synthetic-telegram','chat_id':'synthetic-chat'},'bark':{'enabled':True,'device_key':'bare-synthetic-bark','encryption_key':'bare-synthetic-encryption'},'message':'synthetic-SMS-body'}).encode()
 def test_external_notice_env_ignored(self):
  foreign=self.base/'external-COPYING';self.put(foreign,self.malicious_notice());self.run_gate(extra_env={'LIBUSB_LICENSE_FILE':str(foreign),'LIBUSB_DYLIB_ARM64':str(self.base/'external/lib/libusb.dylib')})
 def test_external_notice_matches_bundle_resigned(self):
  foreign=self.base/'external-COPYING';wire=self.malicious_notice();self.put(foreign,wire);self.put(self.app/'Contents/Resources/licenses/libusb-COPYING',wire);self.sign_again();self.run_gate(True,'bare-synthetic-bark',extra_env={'LIBUSB_LICENSE_FILE':str(foreign)})
 def test_source_notice_tamper(self):self.put(self.source/'licenses/libusb-COPYING',self.malicious_notice());self.run_gate(True,'bare-synthetic-bark')
 def test_source_bundle_notice_match_resigned(self):
  wire=self.malicious_notice();self.put(self.source/'licenses/libusb-COPYING',wire);self.put(self.app/'Contents/Resources/licenses/libusb-COPYING',wire);self.sign_again();self.run_gate(True,'bare-synthetic-bark')
 def test_source_notice_same_size_tamper(self):
  p=self.source/'licenses/libusb-COPYING';wire=bytearray(p.read_bytes());wire[0]^=1;self.put(p,bytes(wire));self.run_gate(True)
 def test_bundle_notice_same_size_tamper_resigned(self):
  p=self.app/'Contents/Resources/licenses/libusb-COPYING';wire=bytearray(p.read_bytes());wire[0]^=1;self.put(p,bytes(wire));self.sign_again();self.run_gate(True)
 def test_bundle_notice_tamper_resigned(self):self.put(self.app/'Contents/Resources/licenses/libusb-COPYING',self.malicious_notice());self.sign_again();self.run_gate(True,'bare-synthetic-bark')
 def test_missing(self):(self.app/'Contents/Resources/backend/switchd-rs').unlink();self.run_gate(True)
 def test_queue_in_known_path(self):self.put(self.app/'Contents/MacOS/Switch',b'SWF1synthetic');r=self.run_gate(True,'synthetic');self.assertEqual(r['category'],'runtime_queue_envelope')
if __name__=='__main__':unittest.main(verbosity=2)

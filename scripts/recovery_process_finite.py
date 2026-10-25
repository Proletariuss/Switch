#!/usr/bin/env python3
"""Root-only fixed13 caller-visible I/O error-return gate; never run on import.

Each real ignored worker returns normally after asserting its injected fault;
its separately reaped observer acquires a new real Lease without an injector.
This is inert byte-I/O software acceptance, not USB or power-loss acceptance.
"""
import argparse
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import queue
import re
import signal
import stat
import subprocess
import sys
import threading
import time
sys.dont_write_bytecode = True
# Promoted matrix from iteration-595-finite-io-gate.py; original SHA-256 bd39ae434880284328125564e231cf478c5bd7c33435fb3066d79f765cdc1c3e.
EXPECTED_MATRIX={'schema': 1, 'iteration': 595, 'cases': [{'case_id': 'A01', 'fixture': 'Fresh', 'operation': 'import', 'record': 'Stage', 'sync_point': None, 'action': 'ShortOnce', 'expected': 'committed_control', 'http_status': 201, 'target_json_complete': True, 'genuine_some_joined_before_arm': False}, {'case_id': 'A02', 'fixture': 'CompletedNone', 'operation': 'import', 'record': 'Stage', 'sync_point': None, 'action': 'PrefixThenEio', 'expected': 'pending', 'http_status': 400, 'target_json_complete': False, 'genuine_some_joined_before_arm': False}, {'case_id': 'A03', 'fixture': 'Fresh', 'operation': 'import', 'record': 'Ready', 'sync_point': None, 'action': 'PrefixThenEio', 'expected': 'pending', 'http_status': 400, 'target_json_complete': False, 'genuine_some_joined_before_arm': False}, {'case_id': 'A04', 'fixture': 'CompletedSome', 'operation': 'delete', 'record': 'Commit', 'sync_point': None, 'action': 'PrefixThenEio', 'expected': 'pending', 'http_status': 500, 'target_json_complete': False, 'genuine_some_joined_before_arm': False}, {'case_id': 'A05', 'fixture': 'CompletedNone', 'operation': 'import', 'record': 'Stage', 'sync_point': 'File', 'action': 'SyncEio', 'expected': 'pending', 'http_status': 400, 'target_json_complete': True, 'genuine_some_joined_before_arm': False}, {'case_id': 'A06', 'fixture': 'CompletedNone', 'operation': 'delete', 'record': 'Other', 'sync_point': 'BackupPreparedSource', 'action': 'SyncEio', 'expected': 'metadata_recovered', 'http_status': 500, 'target_json_complete': False, 'genuine_some_joined_before_arm': False}, {'case_id': 'A07', 'fixture': 'GenuinePending', 'operation': 'reconcile', 'record': 'Requested', 'sync_point': None, 'action': 'PrefixThenEio', 'expected': 'pending', 'http_status': None, 'target_json_complete': False, 'genuine_some_joined_before_arm': False}, {'case_id': 'A08', 'fixture': 'JoinedSome', 'operation': 'finalize', 'record': 'PublishedManifest', 'sync_point': None, 'action': 'PrefixThenEio', 'expected': 'pending', 'http_status': None, 'target_json_complete': False, 'genuine_some_joined_before_arm': True}, {'case_id': 'A09', 'fixture': 'JoinedSome', 'operation': 'finalize', 'record': 'CompletionReceipt', 'sync_point': None, 'action': 'PrefixThenEio', 'expected': 'pending', 'http_status': None, 'target_json_complete': False, 'genuine_some_joined_before_arm': True}, {'case_id': 'A10', 'fixture': 'JoinedSome', 'operation': 'finalize', 'record': 'JournalRetirement', 'sync_point': 'RecoveryArchive', 'action': 'SyncEio', 'expected': 'metadata_recovered', 'http_status': None, 'target_json_complete': False, 'genuine_some_joined_before_arm': True}, {'case_id': 'B01', 'fixture': 'CompletedNone', 'operation': 'import', 'record': 'Commit', 'sync_point': 'File', 'action': 'SyncEio', 'expected': 'metadata_recovered', 'http_status': 400, 'target_json_complete': True, 'genuine_some_joined_before_arm': False}, {'case_id': 'B02', 'fixture': 'JoinedSome', 'operation': 'finalize', 'record': 'CompletionReceipt', 'sync_point': 'File', 'action': 'SyncEio', 'expected': 'metadata_recovered', 'http_status': None, 'target_json_complete': True, 'genuine_some_joined_before_arm': True}, {'case_id': 'B03', 'fixture': 'JoinedSome', 'operation': 'finalize', 'record': 'CompletionReceipt', 'sync_point': 'AppendRoot', 'action': 'SyncEio', 'expected': 'metadata_recovered', 'http_status': None, 'target_json_complete': False, 'genuine_some_joined_before_arm': True}], 'scenario_count': 13, 'owned_case_children': 26, 'pending_is_not_recovered': True, 'io_error_return_only': True, 'power_loss_acceptance': False, 'hardware_acceptance': False}
WORKER='startup_recovery::reconcile::finite_io_tests::workers::finite_io_worker'
OBSERVER='startup_recovery::reconcile::finite_io_tests::workers::finite_io_observer'
EVENT='SWITCH_FINITE_IO_595_EVENT '
OUTER_SECONDS=900
SUMMARY=re.compile(r'^test result: ok\. 1 passed; 0 failed; 0 ignored; \d+ measured; \d+ filtered out; finished in [0-9.]+s$',re.M)
def require(v,m):
    if not v: raise AssertionError(m)
def sha(p): return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def module(p,name):
    spec=importlib.util.spec_from_file_location(name,p)
    result=importlib.util.module_from_spec(spec);spec.loader.exec_module(result);return result
def stamp(p):
    m=Path(p).lstat()
    return (m.st_dev,m.st_ino,m.st_mode,m.st_uid,m.st_gid,m.st_nlink,m.st_size,m.st_mtime_ns,m.st_ctime_ns)
def same_inode(a,b): return all(a[k]==b[k]for k in ('dev','ino','mode','uid','gid'))
def checked_sidecar(p):
    before=stamp(p)
    require(stat.S_ISREG(before[2]) and before[2]&0o7777==0o600 and before[3]==os.geteuid()
        and before[5]==1 and 0<before[6]<16*1024*1024,'unsafe sidecar')
    fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        m=os.fstat(fd);require((m.st_dev,m.st_ino)==before[:2],'sidecar FD replaced')
        with os.fdopen(fd,'rb',closefd=False)as f: raw=f.read(before[6]+1)
        require(stamp(p)==before and len(raw)==before[6],'sidecar changed while reading')
    finally: os.close(fd)
    return json.loads(raw),before,hashlib.sha256(raw).hexdigest()

def validate_sidecar(value,row,root,pid):
    case=row['case_id'];require(value['case']==case and value['root']==str(root)and value['pid']==pid,'wrong worker evidence lineage')
    require(value['worker_completed']is True and value['original_retained']is True,'worker did not complete preservation checks')
    require(value['genuine_some_joined_before_arm']is row['genuine_some_joined_before_arm'],'wrong late full-session provenance')
    require(value['http_status']==row['http_status']and value['target_json_complete']is row['target_json_complete'],'wrong fault result')
    require(type(value['native_before'])is int and value['native_after']==value['native_before'],'post-arm native entry')
    if row['genuine_some_joined_before_arm']: require(value['native_before']>0,'late case never joined genuine session')
    if case=='A07': require(value['native_before']==0,'Requested failure reached native')
    m=root.lstat();fp=value['root_fingerprint'];require((m.st_dev,m.st_ino,m.st_uid)==(fp['dev'],fp['ino'],fp['uid']),'root authority identity differs')
    trace=value['trace'];require(trace['case']==case and trace['hits']==1 and trace['duplicates']==trace['bad_contexts']==0 and len(trace['observations'])==1,'fault not exactly once')
    o=trace['observations'][0]
    require(o['mode']=='Mutation'and o['record']==row['record']and o['sync_point']==row['sync_point'],'wrong actual I/O selector')
    require(o['success']is(case=='A01')and o['delegated']is(row['action']!='SyncEio'),'injected fault falsely acknowledged')
    require(same_inode(o['root'],fp),'fault crossed root identity')
    if row['action']=='ShortOnce':require(o['short_calls']>=2 and o['bytes_written']==o['target_after']['size'],'write_all did not finish a genuine short write')
    if row['action']=='PrefixThenEio':require(0<o['bytes_written']==o['target_after']['size'],'prefix not actually written')
    for original in value['before'].values():
        matches=[e for e in value['after'].values()if e['sha256']==original['sha256']and same_inode(e['fingerprint'],original['fingerprint'])and all(e['fingerprint'][k]==original['fingerprint'][k]for k in ('size','nlink'))]
        require(len(matches)==1,'original raw/inode lost or duplicated')
    name=value['target_path']
    if name:
        path=Path(name);require(not path.is_absolute()and '..'not in path.parts,'target outside owned root')
        actual=root/path;entry=value['after'][name]
        require(sha(actual)==entry['sha256']==value['target_raw_sha256'],'target raw bytes differ')
        if case=='A01':
            # Successful control really renames stage to public. Only this
            # authorized rename ctime transition may differ from write trace.
            require(all(entry['fingerprint'][k]==v for k,v in o['target_after'].items()
                if k not in ('ctime','ctime_ns')),'successful control changed more than rename ctime')
        else:require(entry['fingerprint']==o['target_after'],'failed target fullstamp differs')
    else:require(not value['target_raw_sha256']and row['sync_point']in('BackupPreparedSource','RecoveryArchive','AppendRoot'),'missing actual regular target')

def validate_observer(event,row,worker,value):
    require(event.get('worker_pid')==worker.process.pid and event.get('original_inode_raw_retained')is True,'observer lost worker lineage/originals')
    pending=row['expected']=='pending'
    require(event.get('expected_pending')is pending and event.get('quarantined')is pending,'wrong fixed restart classification')
    require(event.get('instrumented_native_entry_count')==0 and event.get('native_counter_scope')=='production reconcile and shared execute_session only','observer crossed instrumented native/session entry')
    require(event.get('physical_acceptance')is False and event.get('power_loss_tested')is False,'wrong acceptance claim')
    observations=event.get('actual_sync_observations');require(isinstance(observations,list)and len(observations)<=16384,'bounded actual sync observations required')
    def ack(point,fp,record=None,exact=False):
        return any(o['mode']=='Resync'and o['sync_point']in point and o['delegated']is True and o['success']is True
            and (record is None or o['record']==record)and o['target_before']==o['target_after']
            and (o['target_before']==fp if exact else same_inode(o['target_before'],fp))for o in observations)
    if row['case_id']in('B01','B02','B03'):
        failed=value['trace']['observations'][0]
        require(ack(('ResyncRoot',),failed['root']),'missing actual same-root sync ACK')
        require(ack(('ResyncContainer',),failed['parent']),'missing actual exact parent sync ACK')
        if row['case_id']in('B01','B02'):
            require(ack(('ResyncRecord',),failed['target_after'],row['record'],True),'previously failed exact fullstamp record lacks real sync ACK')
        if row['case_id']=='B03':require(ack(('ResyncRoot',),failed['target_after']),'failed final-root sync was not actually retried')
        outer=Path(value['root'])/'module-recovery-v1';m=outer.stat();fp=dict(dev=m.st_dev,ino=m.st_ino,mode=m.st_mode,uid=m.st_uid,gid=m.st_gid)
        require(ack(('ResyncOuter','ResyncNamespace','ResyncContainer'),fp),'outer parent missing actual ACK')
        if row['case_id']=='B01':
            m=(outer/'backup-lifecycle-v1').stat();fp=dict(dev=m.st_dev,ino=m.st_ino,mode=m.st_mode,uid=m.st_uid,gid=m.st_gid)
            require(ack(('ResyncNamespace',),fp),'lifecycle namespace missing actual ACK')

def run_matrix(suite,report,crash,jobs,deadline,*,smoke=False):
    crash.EVENT=EVENT
    registered=[];registry_lock=threading.Lock();launch_lock=threading.Lock();stop=threading.Event()
    cases=EXPECTED_MATRIX['cases'][:1]if smoke else EXPECTED_MATRIX['cases']
    report['scenarios']=[dict(case_index=i,**row,root=str(suite.path/('finite-'+row['case_id'])),passed=False,status='not_launched')for i,row in enumerate(cases)]
    def stop_new():
        with launch_lock:stop.set()
    class Child(crash.Child):
        def __init__(self,test,root,evidence,row):
            self.log_path=suite.logs/(row['case_id']+('-worker.log'if test==WORKER else '-observer.log'))
            self.log=self.log_path.open('x',encoding='utf-8');self.events=queue.Queue();self.output_errors=[];self.lifecycle_lock=threading.Lock()
            env=crash.LIFETIME.BASE.environment(root)
            env.update(SWITCH_FINITE_IO_595_CASE=row['case_id'],SWITCH_FINITE_IO_595_ROOT=str(root),SWITCH_FINITE_IO_595_EVIDENCE=str(evidence))
            command=['/usr/bin/sandbox-exec','-f',str(suite.profile),str(suite.binary),'--exact',test,'--ignored','--nocapture','--test-threads=1']
            try:self.process=subprocess.Popen(command,env=env,cwd=root,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,encoding='utf-8',errors='strict',start_new_session=True)
            except BaseException:self.log.close();raise
            self.record=dict(pid=self.process.pid,test=test,case_id=row['case_id'],case_index=row['case_index'],role='worker'if test==WORKER else 'observer',root=str(root),evidence=str(evidence),log=str(self.log_path),command=command,intentional_sigkill=False,forced_cleanup=False,timeout=False,reaped=False)
            self.reader=threading.Thread(target=self._read,daemon=True)
            with registry_lock:registered.append(self);report['processes'].append(self.record)
            self.reader.start()
        def event(self,kind,seconds):
            until=min(time.monotonic()+seconds,deadline)
            while time.monotonic()<until:
                require(not self.output_errors,'invalid log/event output')
                try:value=self.events.get(timeout=min(0.1,max(0.001,until-time.monotonic())))
                except queue.Empty:
                    if self.process.poll()is None:continue
                    self.reader.join(timeout=3);require(not self.reader.is_alive(),'exited child retained log pipe')
                    try:value=self.events.get_nowait()
                    except queue.Empty:raise AssertionError('child exited without '+kind+'; '+str(self.log_path))
                require(value.get('kind')==kind and value.get('case')==self.record['case_id'],'wrong owned case event')
                return value
            self.record['timeout']=True;raise AssertionError('child/event or 900s outer budget exceeded')
        def reap(self,expected):
            with self.lifecycle_lock:
                super().reap(expected);self.record['owned_process_group_gone']=True
                require(self.events.empty(),'duplicate/unconsumed child event')
                raw=self.log_path.read_text();require(len(SUMMARY.findall(raw))==1 and not re.search(r'thread .* panicked|panicked at|test result: FAILED',raw),'exact libtest 1PASS/0FAIL/0IGNORED and no auxiliary panic required')
                self.record['libtest']=dict(passed=1,failed=0,ignored=0)
        def cleanup(self):
            with self.lifecycle_lock:
                super().cleanup()
                try:os.killpg(self.process.pid,0)
                except ProcessLookupError:self.record['owned_process_group_gone']=True;return
                self.record['forced_cleanup']=True;self.record['surviving_owned_descendants']=True
                try:os.killpg(self.process.pid,signal.SIGKILL)
                except ProcessLookupError:self.record['owned_process_group_gone']=True;return
                until=time.monotonic()+2
                while time.monotonic()<until:
                    try:os.killpg(self.process.pid,0)
                    except ProcessLookupError:self.record['owned_process_group_gone']=True;return
                    time.sleep(0.05)
                raise AssertionError('owned process group survived cleanup')
    def run_case(number):
        row=report['scenarios'][number];root=Path(row['root']);evidence=suite.path/(row['case_id']+'-evidence.json');local=[]
        try:
            with launch_lock:
                if stop.is_set()or time.monotonic()>=deadline:row['status']='not_launched_after_failure_or_budget';return
                root.mkdir(mode=0o700);require(root.resolve()==root and root.lstat().st_mode&0o7777==0o700,'actual private case root required')
                require(not evidence.exists()and not evidence.is_symlink(),'new sidecar required');row['status']='running'
                worker=Child(WORKER,root,evidence,row);local.append(worker)
            produced=worker.event('worker_complete',240)
            require(produced.get('evidence')==str(evidence)and produced.get('hits')==1 and produced.get('native_delta_after_arm')==0 and produced.get('genuine_some_joined_before_arm')is row['genuine_some_joined_before_arm']and produced.get('io_error_is_not_power_loss')is True,'worker did not reach fixed fault completion')
            worker.reap(0);value,side_stamp,side_sha=checked_sidecar(evidence);validate_sidecar(value,row,root,worker.process.pid)
            require(time.monotonic()<deadline,'outer budget expired before fresh observer')
            observer=Child(OBSERVER,root,evidence,row);local.append(observer)
            observed=observer.event('observer_complete',180);observer.reap(0);validate_observer(observed,row,worker,value)
            _,after_stamp,after_sha=checked_sidecar(evidence);require(after_stamp==side_stamp and after_sha==side_sha,'observer changed worker evidence')
            row.update(passed=True,status='completed',worker=produced,observer=observed,evidence=str(evidence),evidence_sha256=side_sha,evidence_stamp=side_stamp)
        except Exception as error:
            row.update(passed=False,status='failed',error=type(error).__name__+': '+str(error));stop_new()
        finally:
            for child in reversed(local):
                try:child.cleanup()
                except Exception as error:row.setdefault('cleanup_errors',[]).append(str(error));row.update(passed=False,status='failed');stop_new()
    executor=ThreadPoolExecutor(max_workers=jobs,thread_name_prefix='finite595');pending={};next_index=0;completed=0
    try:
        while next_index<len(cases) and len(pending)<jobs:pending[executor.submit(run_case,next_index)]=next_index;next_index+=1
        while pending:
            if time.monotonic()>=deadline:
                report['outer_budget_exceeded']=True;stop_new();raise AssertionError('900s finite gate budget exhausted; stop and review')
            done,_=wait(pending,timeout=min(0.2,max(0.001,deadline-time.monotonic())),return_when=FIRST_COMPLETED)
            for future in sorted(done,key=lambda f:pending[f]):
                number=pending.pop(future);future.result();completed+=1;row=report['scenarios'][number]
                print(json.dumps(dict(progress='finite_io_case_completed',completed=completed,total=len(cases),case_id=row['case_id'],passed=row['passed'],status=row['status'])),flush=True)
            while not stop.is_set()and next_index<len(cases) and len(pending)<jobs:pending[executor.submit(run_case,next_index)]=next_index;next_index+=1
    finally:
        stop_new()
        with registry_lock:children=list(registered)
        for child in reversed(children):
            try:child.cleanup()
            except Exception as error:report.setdefault('cleanup_errors',[]).append(str(error))
        executor.shutdown(wait=True,cancel_futures=True)
        for row in report['scenarios']:
            if row['status']=='not_launched':row.update(status='not_launched_after_failure_or_budget',error='unverified; not launched after another failure/budget')
        report['processes'].sort(key=lambda p:(p['case_index'],p['role']!='worker'))

def inventory(suite,report,crash):
    command=['/usr/bin/sandbox-exec','-f',str(suite.profile),str(suite.binary),'--list','--format','terse']
    p=subprocess.Popen(command,env=crash.LIFETIME.BASE.environment(suite.path),cwd=suite.path,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,start_new_session=True)
    row=dict(pid=p.pid,command=command,forced_cleanup=False,timeout=False,reaped=False);report['inventory_process']=row
    try:raw,_=p.communicate(timeout=20)
    except subprocess.TimeoutExpired:
        row.update(timeout=True,forced_cleanup=True);os.killpg(p.pid,signal.SIGKILL);raw,_=p.communicate(timeout=3)
    row.update(exit_code=p.returncode,reaped=True)
    try:os.killpg(p.pid,0)
    except ProcessLookupError:row['owned_process_group_gone']=True
    else:row['forced_cleanup']=True;os.killpg(p.pid,signal.SIGKILL)
    path=suite.logs/'finite-test-inventory.log';path.write_text(raw);row.update(log=str(path),log_sha256=sha(path))
    require(row.get('owned_process_group_gone')is True and p.returncode==0 and not row['forced_cleanup'],'test inventory process failed')
    require(all(t+': test'in raw.splitlines()for t in (WORKER,OBSERVER)),'both actual ignored child selectors required')

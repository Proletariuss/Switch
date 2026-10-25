#!/usr/bin/env python3
"""Root-run-only genuine backup lifecycle process-exit matrix.

Completed Some/None histories use the actual full native byte-I/O fixture and
original production finalizer; fresh-root cases use an actual new Lease/Gate. This is not a power-loss test or
real USB acceptance. Import/AST preparation never starts children.
"""
import argparse
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import threading
import time

sys.dont_write_bytecode = True
# Promoted matrix from iteration-594-backup-lifecycle-crash-gate.py; original SHA-256 d79c6aceeaaf3cbd8806f468ce7aa75d44ade2234139294a7606e5d16da97ec4.
WORKER = 'startup_recovery::reconcile::backup_lifecycle::process_tests::backup_lifecycle_crash_worker'
OBSERVER = 'startup_recovery::reconcile::backup_lifecycle::process_tests::backup_lifecycle_restart_observer'
EVENT = 'SWITCH_BACKUP_LIFECYCLE_EVENT '
POINTS = ('before-generation', 'after-generation', 'before-intent', 'after-intent',
    'before-temp', 'after-temp-allocated', 'after-temp-write', 'after-temp-sync', 'after-ready',
    'before-prepare', 'after-prepare', 'before-directory-rename', 'after-directory-rename',
    'before-rename', 'after-rename', 'before-source-sync', 'after-source-sync',
    'before-destination-sync', 'after-destination-sync', 'before-root-sync', 'after-root-sync',
    'before-commit', 'after-commit')
DELETE_POINTS = ('before-generation', 'after-generation', 'before-intent', 'after-intent',
    'before-prepare', 'after-prepare', 'before-rename', 'after-rename',
    'before-source-sync', 'after-source-sync', 'before-destination-sync', 'after-destination-sync',
    'before-root-sync', 'after-root-sync', 'before-commit', 'after-commit')
IMPORT_POINTS = tuple(point for point in POINTS if point not in
    ('before-directory-rename', 'after-directory-rename'))

def expected_classification(operation, phase, negative=''):
    if negative or phase in ('after-generation', 'before-intent',
            'after-temp-allocated', 'after-temp-write', 'after-temp-sync'):
        return 'pending'
    if phase == 'before-generation':
        return 'no_op'
    if phase in ('after-intent', 'before-temp') or (operation == 'delete' and phase == 'before-prepare'):
        return 'aborted'
    return 'committed'

# Fixed reviewed matrix. Inapplicable directory/temp hooks are excluded before
# launching; no rows or expected classifications are inferred from child output.
CASES = tuple(dict(case_id=variant+'-'+operation+'-'+phase, variant=variant,
    operation=operation, phase=phase, negative='', expected=expected_classification(operation,phase))
    for variant in ('some','none') for operation,points in (('delete',DELETE_POINTS),('import',IMPORT_POINTS))
    for phase in points) + tuple(dict(case_id='fresh-import-'+phase, variant='fresh', operation='import',
    phase=phase, negative='', expected=expected_classification('import',phase)) for phase in POINTS) + tuple(
    dict(case_id=variant+'-delete-negative-'+negative, variant=variant, operation='delete',
    phase='before-rename', negative=negative, expected='pending')
    for variant in ('some','none') for negative in ('both-present','both-absent','foreign-destination','tamper'))


def require(value, message):
    if not value:
        raise AssertionError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()



def run_matrix(suite, report, crash, jobs, *, smoke=False):
    # Reuse the existing mandatory sandbox reader, bounded event/reap logic and
    # exact owned PID/group checks. The task-local module changes only its
    # structured event marker; no producer or source file is modified.
    crash.EVENT = EVENT
    children = []
    registry_lock = threading.Lock()
    launch_lock = threading.Lock()
    stop_launching = threading.Event()

    def stop_new_cases():
        with launch_lock:
            stop_launching.set()

    class Child(crash.Child):
        def __init__(self, test, root, row, label, case_index, baseline=None):
            env = crash.LIFETIME.BASE.environment(root)
            env.update(SWITCH_BACKUP_LIFECYCLE_ROOT=str(root), SWITCH_BACKUP_LIFECYCLE_PHASE=row['phase'],
                SWITCH_BACKUP_LIFECYCLE_VARIANT=row['variant'], SWITCH_BACKUP_LIFECYCLE_OPERATION=row['operation'],
                SWITCH_BACKUP_LIFECYCLE_NEGATIVE=row['negative'])
            if baseline is not None:
                env['SWITCH_BACKUP_LIFECYCLE_BASELINE'] = json.dumps(baseline, separators=(',', ':'))
            self.log_path = suite.logs / (label + '.log')
            self.log = self.log_path.open('x', encoding='utf-8')
            self.events = queue.Queue()
            self.output_errors = []
            self.process = subprocess.Popen([
                '/usr/bin/sandbox-exec', '-f', str(suite.profile), str(suite.binary),
                '--exact', test, '--ignored', '--nocapture', '--test-threads=1'],
                env=env, cwd=root, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT, text=True, encoding='utf-8', errors='strict',
                start_new_session=True)
            self.record = dict(label=label, pid=self.process.pid, test=test, phase=row['phase'],
                negative=row['negative'], variant=row['variant'], operation=row['operation'], case_id=row['case_id'], root=str(root), log=str(self.log_path), case_index=case_index,
                role='worker' if test == WORKER else 'observer', intentional_sigkill=False,
                forced_cleanup=False, timeout=False, reaped=False,
                expected_process_exit_only_not_power_loss=test == WORKER)
            self.reader = threading.Thread(target=self._read, daemon=True)
            # Register every Popen-derived owner before starting any pipe work.
            with registry_lock:
                report['processes'].append(self.record)
                children.append(self)
            self.reader.start()

        def reap(self, expected):
            super().reap(expected)
            # The inherited method actually checks killpg(owned pgid,0) returns
            # ESRCH before returning; this records that actual observation.
            self.record['owned_process_group_gone'] = True

        def cleanup(self):
            super().cleanup()
            try:
                os.killpg(self.process.pid, 0)
            except ProcessLookupError:
                self.record['owned_process_group_gone'] = True
                return
            # Only this exact Popen-owned group is eligible for cleanup.
            self.record['forced_cleanup'] = True
            self.record['surviving_owned_descendants'] = True
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except ProcessLookupError:
                self.record['owned_process_group_gone'] = True
                return
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    os.killpg(self.process.pid, 0)
                except ProcessLookupError:
                    self.record['owned_process_group_gone'] = True
                    return
                time.sleep(0.05)
            raise AssertionError('owned process group did not disappear after cleanup')

    crash.inventory(suite, report, (WORKER, OBSERVER), 'backup-test-inventory')
    cases = CASES[:1] if smoke else CASES
    report['matrix_sha256'] = hashlib.sha256(json.dumps(CASES, sort_keys=True,
        separators=(',', ':')).encode()).hexdigest()
    require(report['matrix_sha256'] == 'cc9ee4f38297d5952108fa4289ea97cfe3bf01ba2bd289ebae7517f816539abf',
            'reviewed exact case IDs/classifications changed')
    report['scenarios'] = [dict(row, case_index=number,
        root=str(suite.path / ('backup-lifecycle-' + str(number))), passed=False,
        status='not_launched') for number, row in enumerate(cases)]
    report['case_jobs'] = jobs
    report['case_cleanup_errors'] = []

    def run_case(number):
        row = report['scenarios'][number]
        phase, negative, root = row['phase'], row['negative'], Path(row['root'])
        local_children = []
        try:
            with launch_lock:
                if stop_launching.is_set():
                    row['status'] = 'not_launched_after_failure'
                    return False
                row['status'] = 'running'
                root.mkdir(mode=0o700)
                worker = Child(WORKER, root, row, str(number) + '-worker', number)
                local_children.append(worker)
            ready = worker.event('ready', timeout=240)
            require(ready.get('root') == str(root) and ready.get('pid') == worker.process.pid
                and ready.get('phase') == phase and ready.get('variant') == row['variant']
                and ready.get('operation') == row['operation'], 'worker identity/case binding differs')
            if row['variant'] == 'fresh':
                require(ready.get('genuine_completed_recovery') is False
                    and ready.get('reconciliation_native_entries') == 0
                    and ready.get('historical') == {} and ready.get('original') is None,
                    'fresh root falsely claims historical authority')
            else:
                require(ready.get('genuine_completed_recovery') is True
                    and ready.get('original_voice_some') is (row['variant'] == 'some')
                    and ready.get('actual_join_returned') is True
                    and ready.get('reconciliation_native_entries', 0) > 0
                    and ready.get('historical') and ready.get('original'),
                    'worker lacks genuine Some/None full-session/join provenance')
            boundary = worker.event('boundary', timeout=60)
            require(boundary.get('process_exit_only_not_power_loss') is True
                and boundary.get('pid') == worker.process.pid and boundary.get('phase') == phase
                and isinstance(boundary.get('generation'), int)
                and boundary['generation'] == ready.get('next_generation') and boundary['generation'] > 0,
                'actual hook phase/generation/PID was not reached')
            worker.reap(86)
            observer = Child(OBSERVER, root, row, str(number) + '-observer', number, ready)
            local_children.append(observer)
            observed = observer.event('restart', timeout=60)
            require(observed.get('pid') == observer.process.pid and observed.get('phase') == phase
                and observed.get('negative') == negative and observed.get('reconciliation_native_entries') == 0,
                'fresh observer identity differs or entered native/session execution')
            for field in ('metadata_resume_only', 'prefix_raw_inodes_unchanged',
                    'historical_raw_inodes_unchanged', 'second_lifetime_same_result', 'no_overwrite'):
                require(observed.get(field) is True, 'restart did not prove ' + field)
            require(observed.get('pending') is (row['expected'] == 'pending')
                and observed.get('aborted') is (row['expected'] == 'aborted')
                and observed.get('no_op') is (row['expected'] == 'no_op'),
                'actual result differs from fixed reviewed expected classification')
            require(observed.get('actual_public_path_and_archive_checked') is (row['expected'] != 'pending')
                and observed.get('post_restart_import_delete_verified') is (row['expected'] != 'pending'),
                    'public deletion/import placement evidence missing')
            if negative:
                require(observed.get('controlled_tamper_not_original_preservation_claim') is True,
                        'controlled negative falsely claims unchanged original bytes')
            observer.reap(0)
            row.update(ready=ready, boundary=boundary, observer=observed,
                       passed=True, status='completed')
        except Exception as error:
            row.update(error=type(error).__name__ + ': ' + str(error), status='failed', passed=False)
            stop_new_cases()
        finally:
            for child in reversed(local_children):
                try:
                    child.cleanup()
                except Exception as error:
                    row.setdefault('cleanup_errors', []).append(str(error))
                    row.update(status='failed', passed=False)
                    stop_new_cases()
        return row['passed']

    next_index, completed = 0, 0
    try:
        with ThreadPoolExecutor(max_workers=jobs, thread_name_prefix='backup_lifecycle594') as executor:
            pending = {}
            while next_index < len(cases) and len(pending) < jobs:
                pending[executor.submit(run_case, next_index)] = next_index
                next_index += 1
            while pending:
                done, _ = wait(pending, return_when=FIRST_COMPLETED)
                for future in sorted(done, key=lambda item: pending[item]):
                    number = pending.pop(future)
                    try:
                        future.result()
                    except Exception as error:
                        report['scenarios'][number].update(status='failed', passed=False,
                            error=type(error).__name__ + ': ' + str(error))
                        stop_new_cases()
                    completed += 1
                    row = report['scenarios'][number]
                    print(json.dumps({'progress':'backup_lifecycle_case_completed', 'case_index':number,
                        'completed':completed, 'total':len(cases), 'phase':row['phase'],
                        'negative':row['negative'], 'case_id':row['case_id'], 'expected':row['expected'], 'passed':row['passed'], 'status':row['status']}), flush=True)
                while not stop_launching.is_set() and next_index < len(cases) and len(pending) < jobs:
                    pending[executor.submit(run_case, next_index)] = next_index
                    next_index += 1
            if stop_launching.is_set():
                for row in report['scenarios']:
                    if row['status'] == 'not_launched':
                        row['status'] = 'not_launched_after_failure'
                        row['error'] = 'not launched after another owned case failed; remains unverified'
    finally:
        stop_new_cases()
        # Executor has joined all active case threads before this final pass.
        # Case-local cleanup handles failures promptly; the registry also covers
        # a constructor that raised after Popen but before returning to run_case.
        with registry_lock:
            registered = list(children)
        for child in reversed(registered):
            try:
                child.cleanup()
            except Exception as error:
                report['case_cleanup_errors'].append(str(error))
        report['processes'].sort(key=lambda row: (row['case_index'], 0 if row['role'] == 'worker' else 1))


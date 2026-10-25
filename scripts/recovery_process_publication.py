#!/usr/bin/env python3
"""Root-run-only genuine dual-directory publication process-exit matrix.

Every history comes from the actual Owned managed-voice/full native byte-I/O
fixture and original production finalizer. This is not a power-loss test or
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
# Promoted matrix from iteration-593-publication-crash-gate.py; original SHA-256 6e2a24d360757c7087a9deefa197bbb81547ad444aeffc28f65e9c2bfd52024b.
WORKER = 'startup_recovery::reconcile::crash_tests::publication_crash_worker'
OBSERVER = 'startup_recovery::reconcile::crash_tests::publication_restart_observer'
EVENT = 'SWITCH_PUBLICATION_EVENT '
SLOT_POINTS = ('before-prepare', 'after-prepare', 'before-rename', 'after-rename',
    'before-archive-sync', 'after-archive-sync', 'before-attempt-sync', 'after-attempt-sync',
    'before-namespace-sync', 'after-namespace-sync', 'before-root-sync', 'after-root-sync',
    'before-placement', 'after-placement')
PHASES = tuple(slot + ':' + point for slot in ('journal', 'voice-control') for point in SLOT_POINTS) + tuple(
    'publication:' + point for point in ('before-manifest', 'after-manifest',
    'before-publication', 'after-publication', 'before-receipt', 'after-receipt'))
NEGATIVES = tuple(slot + ':' + case for slot in ('journal', 'voice-control') for case in (
    'both-present', 'both-absent', 'foreign-destination', 'tamper'))


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
        def __init__(self, test, root, phase, label, case_index, baseline=None, negative=''):
            env = crash.LIFETIME.BASE.environment(root)
            env.update(SWITCH_PUBLICATION_ROOT=str(root), SWITCH_PUBLICATION_PHASE=phase,
                       SWITCH_PUBLICATION_NEGATIVE=negative)
            if baseline is not None:
                env['SWITCH_PUBLICATION_BASELINE'] = json.dumps(baseline, separators=(',', ':'))
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
            self.record = dict(label=label, pid=self.process.pid, test=test, phase=phase,
                negative=negative, root=str(root), log=str(self.log_path), case_index=case_index,
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

    crash.inventory(suite, report, (WORKER, OBSERVER), 'publication-test-inventory')

    cases = [(phase, '') for phase in PHASES] + [
        (negative.split(':')[0] + ':before-prepare', negative) for negative in NEGATIVES]
    if smoke:
        cases = cases[:1]
    # Keep all 42 rows in stable index order, including unlaunched failures.
    report['scenarios'] = [dict(case_index=number, phase=phase, negative=negative,
        root=str(suite.path / ('publication-' + str(number))), passed=False,
        status='not_launched') for number, (phase, negative) in enumerate(cases)]
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
                worker = Child(WORKER, root, phase, str(number) + '-worker', number)
                local_children.append(worker)
            ready = worker.event('ready', timeout=240)
            require(ready.get('root') == str(root) and ready.get('original_voice_some') is True
                and ready.get('actual_join_returned') is True
                and ready.get('actual_reconciliation_native_entries', 0) > 0,
                'worker lacks genuine Some-voice full-session/join provenance')
            require(set(ready.get('baseline', {})) == {'journal', 'voice-control'}
                and all(ready['baseline'][slot] for slot in ('journal', 'voice-control')),
                'worker did not capture both original raw/inode trees')
            boundary = worker.event('boundary', timeout=60)
            require(boundary.get('process_exit_only_not_power_loss') is True,
                    'test boundary incorrectly labelled as power-loss acceptance')
            worker.reap(86)
            observer = Child(OBSERVER, root, phase, str(number) + '-observer', number, ready['baseline'], negative)
            local_children.append(observer)
            observed = observer.event('restart', timeout=60)
            require(observed.get('reconciliation_native_entries') == 0,
                    'metadata observer entered reconciliation native/session execution')
            if negative:
                require(observed.get('negative') == negative and observed.get('pending') is True
                    and observed.get('no_overwrite') is True
                    and observed.get('controlled_tamper_not_original_preservation_claim') is True,
                    'ambiguous/tampered placement accepted or changed after rejection')
            else:
                for field in ('metadata_resume_only', 'prefix_raw_inodes_unchanged',
                    'both_original_trees_raw_inodes_preserved', 'events_exactly_once',
                    'original_unknown1_not_rewritten', 'fresh_gate_activated', 'second_lifetime_accepted'):
                    require(observed.get(field) is True, 'restart did not prove ' + field)
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
        with ThreadPoolExecutor(max_workers=jobs, thread_name_prefix='publication593') as executor:
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
                    print(json.dumps({'progress':'publication_case_completed', 'case_index':number,
                        'completed':completed, 'total':len(cases), 'phase':row['phase'],
                        'negative':row['negative'], 'passed':row['passed'], 'status':row['status']}), flush=True)
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


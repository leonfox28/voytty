#!/usr/bin/env python3
"""Audit retained matrix evidence without contacting or modifying test devices.

Usage: python3 tests/e2e/multi-device-audit.py RUN_ID [RUN_ID ...]
Writes acceptance-audit.json next to each run's unchanged raw evidence.
"""
import collections
import hashlib
import json
import math
import re
from pathlib import Path
import statistics
import sys

workspace = Path(__file__).resolve().parents[2]
base = workspace / 'target/verification/multi-device'
if len(sys.argv) < 2:
    sys.exit('usage: multi-device-audit.py RUN_ID [RUN_ID ...]')
exit_status = 0
keys = ('scenario', 'host', 'old', 'new', 'path', 'iteration')
for name in sys.argv[1:]:
    root = base / name
    if root.parent.resolve() != base.resolve():
        sys.exit('run IDs must name one directory below target/verification/multi-device')
    data = json.loads((root / 'results.json').read_text())
    planned = json.loads((root / 'matrix.json').read_text())
    results = data['results']
    groups = collections.defaultdict(collections.Counter)
    phases = collections.Counter()
    times = collections.defaultdict(list)
    paths = collections.Counter()
    failures = []
    issues = []
    proof_count = 0
    manifest = json.loads((root / 'manifest.json').read_text())
    def audit_timing(metrics, label, scenario):
        for key, limit in (('detection_seconds', 120), ('host_release_seconds', 120),
                           ('recovery_seconds', 60), ('retirement_seconds', 60)):
            if metrics.get(key, 0) > limit:
                issues.append({'case': label, 'issue': key + ' exceeds acceptance bound'})
        if manifest.get('timing_contract') != 'absolute-offline-deadlines-v1':
            return
        required = []
        if scenario in {'recover-long', 'host-network', 'handoff-early', 'handoff-late',
                        'race-reconnect', 'upload-disconnect'}:
            required.append('detection_seconds')
        if scenario == 'handoff-late':
            required.append('host_release_seconds')
        if scenario in {'handoff-early', 'handoff-late', 'race-reconnect'}:
            required.append('retirement_seconds')
        for key in required:
            if key not in metrics:
                issues.append({'case': label, 'issue': 'missing ' + key})

    def audit_offline(metrics, label, scenario, controller=None):
        if (scenario == 'recover-short'
                and manifest.get('short_outage_traffic_contract') == 'shell-output-and-controller-resize-v1'):
            resize = metrics.get('short_outage_resize')
            if not (isinstance(resize, dict)
                    and all(isinstance(resize.get(key), list) and len(resize[key]) == 2
                            and all(isinstance(size, int) and size > 0 for size in resize[key])
                            for key in ('before', 'requested', 'observed'))
                    and resize['before'] != resize['requested']
                    and resize['observed'] == resize['requested']):
                issues.append({'case': label, 'issue': 'short outage lacks a distinct applied controller resize'})
            if (controller == 'C'
                    and manifest.get('short_outage_viewport_contract') == 'host-and-android-surface-v1'
                    and not (isinstance(resize, dict)
                             and resize.get('controller_observed') == resize.get('requested')
                             and resize.get('controller_state') == 'active'
                             and resize.get('controller_input_ready') is True)):
                issues.append({'case': label, 'issue': 'short outage lacks the matching active Android surface'})
        if manifest.get('offline_scope') != 'owned-sockets-bidirectional':
            return  # Older runs explicitly retain their egress-only scope.
        required = scenario in {'recover-short', 'recover-long', 'host-network', 'handoff-early',
                                'handoff-late', 'race-reconnect', 'output-resize', 'upload-disconnect'}
        faults = metrics.get('faults', [])
        if required and not faults:
            issues.append({'case': label, 'issue': 'passed offline case lacks fault evidence'})
        for fault in faults:
            if (fault.get('scope') != 'owned-sockets-bidirectional' or not all(
                    fault.get('dropped_by_direction', {}).get(direction, 0) > 0
                    for direction in ('ingress', 'egress'))):
                issues.append({'case': label, 'issue': 'passed offline case lacks bidirectional packet loss'})
            evidence = workspace / fault.get('evidence', 'absent')
            if not evidence.is_file():
                issues.append({'case': label, 'issue': 'missing raw firewall counters'})
                continue
            try:
                raw = json.loads(evidence.read_text())
                observed = {direction: sum(int(m[1]) for output in raw.get(key, {}).values()
                    if isinstance(output, str)
                    for m in re.finditer(r'(?m)^\s*(\d+)\s+\d+\s+DROP\s', output))
                    for direction, key in (('ingress', 'ingress_families'), ('egress', 'families'))}
                if observed != fault.get('dropped_by_direction'):
                    issues.append({'case': label, 'issue': 'firewall metrics differ from raw counters'})
            except (ValueError, AttributeError):
                issues.append({'case': label, 'issue': 'invalid raw firewall counters'})
    runner_hash = hashlib.sha256((root / 'runner-source.py').read_bytes()).hexdigest()
    if runner_hash != manifest.get('runner_sha256'):
        issues.append({'issue': 'pinned runner does not match its manifest digest'})
    for row in results:
        groups[f"{row['host']}/{row['path']}"][row['status']] += 1
        if row['status'] != 'passed':
            phases[row.get('phase', 'not-started')] += 1
            failures.append({k: row.get(k) for k in (*keys, 'phase', 'status', 'evidence') } | {'error': row.get('error', '')[:350]})
        evidence = workspace / row.get('evidence', str(root / 'absent'))
        unknown = any(kind in row.get('error', '') for kind in ('operation_outcome_unknown', 'pair_outcome_unknown'))
        if unknown and not (evidence / 'outcome-reconciliation.json').exists():
            issues.append({'case': evidence.name, 'issue': 'unknown operation lacks read-only reconciliation'})
        if (manifest.get('session_cleanup_barrier_contract') == 'unknown-session-host-process-exit-v1'
                and 'operation_outcome_unknown' in row.get('error', '')):
            try:
                session_mutation = True
                if row['scenario'] == 'fresh-pair':
                    attempt = json.loads((evidence / 'pair-attempt.json').read_text())
                    if not isinstance(attempt.get('returned'), bool):
                        raise ValueError('pair completion was not recorded')
                    session_mutation = attempt['returned']
                if session_mutation:
                    barrier = json.loads((evidence / 'session-cleanup-barrier.json').read_text())
                    if not (barrier.get('host') == row['host']
                            and barrier.get('method') == 'test-uid-daemon-process-exit'
                            and barrier.get('confirmed_stopped') is True
                            and barrier.get('remaining_processes') == []):
                        raise ValueError('late Session mutations were not fenced')
            except (OSError, ValueError, KeyError, AttributeError, TypeError):
                issues.append({'case': evidence.name, 'issue': 'unknown Session lacks host exit before the next row'})
        if (manifest.get('post_pair_session_contract') == 'host-session-query-after-accepted-pair-v1'
                and row['scenario'] == 'fresh-pair' and 'operation_outcome_unknown' in row.get('error', '')):
            try:
                attempt = json.loads((evidence / 'pair-attempt.json').read_text())
                if not isinstance(attempt.get('returned'), bool):
                    raise ValueError('pair completion was not recorded')
                if attempt['returned']:
                    record = json.loads((evidence / 'session-outcome-reconciliation.json').read_text())
                    if (record.get('method') != 'read-only host session list; no mutation retry'
                            or 'expected_session' not in record or not isinstance(record.get('observed_sessions'), list)):
                        raise ValueError('Session outcome needs a separate host Session query')
            except (OSError, ValueError, KeyError, AttributeError, TypeError):
                issues.append({'case': evidence.name, 'issue': 'unknown Session after pairing lacks its own read-only query'})
        if (manifest.get('pair_cleanup_barrier_contract') == 'failed-accept-host-process-exit-v1'
                and row['scenario'] == 'fresh-pair'):
            attempt_path = evidence / 'pair-attempt.json'
            if attempt_path.exists() or row['status'] == 'passed':
                try:
                    attempt = json.loads(attempt_path.read_text())
                    valid = (attempt.get('host') == row['host'] and attempt.get('client') == row['old']
                        and isinstance(attempt.get('submitted'), bool) and isinstance(attempt.get('returned'), bool))
                    if row['status'] == 'passed':
                        valid = valid and attempt['submitted'] and attempt['returned']
                    if attempt.get('submitted') and not attempt.get('returned'):
                        barrier = json.loads((evidence / 'pair-cleanup-barrier.json').read_text())
                        valid = (valid and barrier.get('host') == row['host']
                            and barrier.get('method') == 'test-uid-daemon-process-exit'
                            and barrier.get('confirmed_stopped') is True
                            and barrier.get('remaining_processes') == [])
                    if not valid:
                        raise ValueError('pair accept cleanup did not fence late host commits')
                except (OSError, ValueError, KeyError, AttributeError, TypeError):
                    issues.append({'case': evidence.name, 'issue': 'unresolved pairing lacks host exit before cleanup'})
        if (manifest.get('pair_reconciliation_contract') == 'host-authorization-before-revoke-v1'
                and row['scenario'] == 'fresh-pair' and
                (row['status'] == 'passed' or unknown)):
            try:
                record = json.loads((evidence / 'outcome-reconciliation.json').read_text())
                before, observed = record['before_authorizations'], record['observed_authorizations']
                principals = sorted(set(observed) - set(before))
                valid = (record.get('operation') == 'pair-accept' and record.get('host') == row['host']
                    and record.get('method') == 'read-only host authorization list; no pairing retry'
                    and record.get('new_principals') == principals
                    and record.get('cleanup_revoked') == principals and not record.get('cleanup_errors')
                    and not record.get('query_error'))
                after = record['post_cleanup_authorizations']
                valid = valid and all(after.get(principal, {}).get('allowed') != 'Yes' for principal in principals)
                if row['status'] == 'passed':
                    valid = valid and len(principals) == 1 and all(
                        observed[principal].get('known') == 'No' and observed[principal].get('allowed') == 'Yes'
                        for principal in principals)
                if not valid:
                    raise ValueError('authorization observation/cleanup mismatch')
            except (OSError, ValueError, KeyError, AttributeError, TypeError):
                issues.append({'case': evidence.name, 'issue': 'pairing lacks read-only authorization reconciliation and cleanup'})
        if row['status'] != 'passed':
            continue
        metrics = row.get('metrics', {})
        if (manifest.get('host_readiness_contract') == 'local-host-online-before-scenario-v1'
                and row['path'] != 'local'):
            readiness = metrics.get('host_readiness', {})
            seconds = readiness.get('seconds')
            if (readiness.get('device') != row['host']
                    or readiness.get('method') != 'local-status-only'
                    or readiness.get('online') is not True
                    or not isinstance(seconds, (int, float)) or not 0 <= seconds <= 60):
                issues.append({'case': evidence.name, 'issue': 'host local readiness evidence missing or invalid'})
        if (manifest.get('cold_start_contract') == 'cli-autostart-daemon-v1'
                and row['scenario'] in {'cold', 'first-existing'} and row['old'] in {'A', 'B'}
                and metrics.get('cold_start_mode') != 'cli-starts-stopped-daemon'):
            issues.append({'case': evidence.name, 'issue': 'Linux cold case did not start through CLI'})
        audit_offline(metrics, evidence.name, row['scenario'], row.get('old'))
        audit_timing(metrics, evidence.name, row['scenario'])
        if not metrics.get('paths'):
            issues.append({'case': evidence.name, 'issue': 'passed case lacks selected path evidence'})
        for detach in metrics.get('detaches', []):
            if detach.get('normal_exit_required') and (detach.get('forced') or detach.get('exit') != 0):
                issues.append({'case': evidence.name, 'issue': 'normal detach required forced cleanup or returned an error'})
        for key, value in row.get('metrics', {}).items():
            if key.endswith('_seconds'):
                values = value if isinstance(value, list) else [value]
                times[key].extend(x for x in values if isinstance(x, (float, int)))
        for path in row.get('metrics', {}).get('paths', []):
            actual = path.get('snapshot', {}).get('path')
            paths[str(actual)] += 1
            required = path['path']
            if actual not in ({'direct', 'relay'} if required == 'auto' else {required}):
                issues.append({'case': evidence.name, 'issue': 'selected path mismatch'})
        proof = evidence / 'execution.log'
        if not proof.exists():
            issues.append({'case': evidence.name, 'issue': 'passed case lacks execution proof'})
            continue
        proof_count += 1
        rows = [l.split('|') for l in proof.read_text().splitlines() if l]
        if not rows:
            issues.append({'case': evidence.name, 'issue': 'passed case has empty execution proof'})
            continue
        baseline = (evidence / 'baseline.txt').read_text().strip()
        if any(len(r) != 6 or '|'.join(r[1:5]) != baseline for r in rows):
            issues.append({'case': evidence.name, 'issue': 'unexpected/replayed input or changed shell evidence'})
            continue
        if len({r[0] for r in rows}) != len(rows):
            issues.append({'case': evidence.name, 'issue': 'duplicate execution nonce'})
        counters = [int(r[-1]) for r in rows]
        if any(b < a for a, b in zip(counters, counters[1:])) or len(counters) > 1 and counters[-1] <= counters[0]:
            issues.append({'case': evidence.name, 'issue': 'background counter did not advance'})
    commanded = root / 'android-commands.jsonl'
    if commanded.exists():
        commands = [json.loads(l) for l in commanded.read_text().splitlines()]
        for i, command in enumerate(commands[:-1]):
            error = command.get('result', {}).get('snapshot', {}).get('error')
            if command['op'] == 'connect' and error not in (None, 'session_occupied') and commands[i+1]['op'] == 'select':
                issues.append({'command_line': i+1, 'issue': 'host-open error followed by another selection', 'error': error})
    planned_keys = {tuple(r[k] for k in keys) for r in planned}
    observed_keys = {tuple(r[k] for k in keys) for r in results}
    if observed_keys != planned_keys or len(observed_keys) != len(results) or len(planned_keys) != len(planned):
        issues.append({'issue': 'matrix coverage mismatch/duplicate'})
    def stats(xs):
        xs = sorted(xs)
        return {'samples': len(xs), 'median': round(statistics.median(xs), 3),
                'p95': round(xs[max(0, math.ceil(len(xs)*.95)-1)], 3), 'maximum': round(xs[-1], 3)}
    report = {'counts': data['counts'], 'planned': len(planned), 'recorded': len(results),
              'iterations': sorted({r['iteration'] for r in results}),
              'groups': dict(groups), 'failure_phases': phases, 'failures': failures,
              'successful_timings_seconds': {k: stats(v) for k, v in times.items() if v},
              'observed_path_samples_in_passed_cases': paths, 'passed_case_proofs_checked': proof_count,
              'incidents': data.get('incidents', []), 'audit_issues': issues}
    observed_counts = {status: sum(row['status'] == status for row in results)
                       for status in ('passed', 'failed', 'blocked', 'skipped')}
    if observed_counts != data['counts']:
        issues.append({'issue': 'reported counts do not match raw rows'})
    soak_file = root / 'soak.json'
    if soak_file.exists():
        soak = json.loads(soak_file.read_text())
        actions = soak.get('actions', [])
        soak_counts = collections.Counter(action['status'] for action in actions)
        completed_hour = soak['seconds'] >= 3600 and not soak.get('error')
        fixtures = []
        for host, session in zip(('A', 'B'), soak.get('sessions', [])):
            fixture = root / ('soak-' + host)
            try:
                baseline = (fixture / 'baseline.txt').read_text().strip()
                records = [line.split('|') for line in (fixture / 'execution.log').read_text().splitlines() if line]
                valid = bool(records) and all(len(r) == 6 and '|'.join(r[1:5]) == baseline for r in records)
                valid = valid and len({r[0] for r in records}) == len(records)
                counters = [int(r[-1]) for r in records] if valid else []
                valid = valid and len(counters) > 1 and counters[-1] > counters[0]
                valid = valid and all(b >= a for a, b in zip(counters, counters[1:]))
                retained = session in (fixture / (host + '-sessions.txt')).read_text()
            except (OSError, ValueError):
                valid, retained, records, counters = False, False, [], []
            fixtures.append({'host': host, 'session': session, 'execution_records': len(records),
                             'continuity_proof_valid': valid, 'original_session_retained': retained,
                             'counter_first_last': [counters[0], counters[-1]] if counters else []})
            if completed_hour and not (valid and retained):
                issues.append({'issue': 'completed soak lacks original-session continuity proof', 'host': host})
        if completed_hour and (len(fixtures) != 2 or {a['host'] for a in actions} != {'A', 'B'}):
            issues.append({'issue': 'completed soak does not cover both hosts'})
        for action in actions:
            label = f"soak-{action['host']}/action-{action['iteration']:04}"
            if action['status'] == 'passed':
                metrics = action.get('metrics', {})
                audit_offline(metrics, label, action['scenario'], action.get('old'))
                audit_timing(metrics, label, action['scenario'])
                if action['scenario'].startswith('recover') and not any(
                        fault.get('dropped_packets', 0) > 0 for fault in metrics.get('faults', [])):
                    issues.append({'case': label, 'issue': 'passed recovery lacks observed packet loss'})
                for path in metrics.get('paths', []):
                    if path.get('snapshot', {}).get('path') not in ('direct', 'relay'):
                        issues.append({'case': label, 'issue': 'soak lacks actual transport path'})
            elif 'operation_outcome_unknown' in action.get('error', ''):
                evidence = root / ('soak-' + action['host']) / f"failed-action-{action['iteration']:04}"
                if not (evidence / 'outcome-reconciliation.json').exists():
                    issues.append({'case': label, 'issue': 'unknown soak operation lacks read-only reconciliation'})
        if soak['status'] == 'passed' and (not completed_hour or any(a['status'] != 'passed' for a in actions)
                                          or soak.get('connection_failures')):
            issues.append({'issue': 'soak pass contradicts duration or recorded failures'})
        report['soak'] = {'seconds': soak['seconds'], 'completed_60_minutes': completed_hour,
                          'status': soak['status'], 'action_counts': dict(soak_counts),
                          'seed': soak['seed'], 'fixtures': fixtures,
                          'connection_failures': len(soak.get('connection_failures', []))}
    exit_status |= bool(issues)
    (root / 'acceptance-audit.json').write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n')
    print(name, data['counts'], 'audit issues', len(issues))

sys.exit(int(exit_status))

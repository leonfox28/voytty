#!/usr/bin/env python3
"""Socket-free installer contracts; release jobs also exercise signed real assets."""

import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
CURL_STUB = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
root = pathlib.Path(os.environ['FIXTURE'])
count = len(list((root / 'requests').iterdir()))
# Each record stays below the signature download's process file-size limit.
record = []
for option in ('--proto', '--proto-redir', '--max-redirs', '--max-time', '--speed-time', '--max-filesize'):
    record += [option, args[args.index(option) + 1]]
(root / 'requests' / str(count)).write_text(json.dumps(record))
replies = json.loads(os.environ.get('REPLIES', '[]'))
code, status = replies[count] if count < len(replies) else [0, '200']
clock = root / 'clock'
elapsed = min(int(os.environ.get('TRANSFER_SECONDS', '0')), int(args[args.index('--max-time') + 1]))
clock.write_text(str(int(clock.read_text()) + elapsed))
output = pathlib.Path(args[args.index('--output') + 1])
if code:
    output.write_bytes(b'partial failed response')
else:
    mapping = json.loads((root / 'urls').read_text())
    output.write_bytes(pathlib.Path(mapping[args[-1]]).read_bytes())
print(status, end='')
sys.exit(code)
'''
CLOCK_STUB = r'''#!/usr/bin/env python3
import os, pathlib, sys
clock = pathlib.Path(os.environ['FIXTURE']) / 'clock'
if pathlib.Path(sys.argv[0]).name == 'sleep':
    clock.write_text(str(int(clock.read_text()) + int(sys.argv[1])))
else:
    print(clock.read_text())
'''


class InstallerTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='voytty-installer-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.destination = self.root / 'installed'
        self.env = {**os.environ, 'FIXTURE': str(self.root),
                    'HOME': str(self.root / 'home'), 'TMPDIR': str(self.root),
                    'PATH': str(self.bin) + os.pathsep + os.environ['PATH'],
                    'VOYTTY_INSTALL_DIR': '', 'REPLIES': '[]', 'TRANSFER_SECONDS': '0',
                    'FIXTURE_OS': 'Linux', 'FIXTURE_ARCH': 'x86_64',
                    'FIXTURE_ARM': '0', 'FIXTURE_BITS': '64'}
        self.write('clock', '1000')
        self.clear_requests()
        self.executable('bin/curl', CURL_STUB)
        for name in ('date', 'sleep'):
            self.executable('bin/' + name, CLOCK_STUB)
        self.executable('bin/uname', '#!/bin/sh\ncase "$1" in -s) echo "$FIXTURE_OS";; -m) echo "$FIXTURE_ARCH";; esac\n')
        self.executable('bin/sysctl', '#!/bin/sh\necho "$FIXTURE_ARM"\n')
        self.executable('bin/sw_vers', '#!/bin/sh\necho 26.0\n')
        self.executable('bin/getconf', '#!/bin/sh\ncase "$1" in LONG_BIT) echo "$FIXTURE_BITS";; GNU_LIBC_VERSION) echo "glibc 2.28";; esac\n')
        candidate = b'''#!/bin/sh
printf '%s\\n' "$0" >> "$FIXTURE/candidate-ran"
case "$1" in
    --internal-release-self-check|--internal-release-verify) exit 0 ;;
    --internal-release-install) ln "$0" "$2" ;;
    *) exit 2 ;;
esac
'''
        archive = self.root / 'voytty.tar.gz'
        with tarfile.open(archive, 'w:gz') as tar:
            info = tarfile.TarInfo('voytty')
            info.size, info.mode = len(candidate), 0o700
            tar.addfile(info, io.BytesIO(candidate))
        manifest = self.write('voytty-release.json', '{"fixture":true}\n')
        signature = self.write('voytty-release.json.sig', 's' * 64)
        self.base = 'https://fixture.invalid/v1.2.3'
        self.urls = {self.base + '/' + path.name: str(path) for path in (archive, manifest, signature)}
        template = (ROOT / 'install/versioned.sh.in').read_text()
        targets = ('aarch64-apple-darwin', 'aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu')
        cases = '\n'.join(f"    {target}) archive_name='{archive.name}'; archive_length='{archive.stat().st_size}'; archive_sha256='{hashlib.sha256(archive.read_bytes()).hexdigest()}' ;;" for target in targets)
        for token, value in {'@TAG@': 'v1.2.3', '@BASE_URL@': self.base,
                             '@MANIFEST_SHA256@': hashlib.sha256(manifest.read_bytes()).hexdigest(),
                             '@TEST_MODE@': '0', '@ARTIFACT_CASES@': cases}.items():
            template = template.replace(token, value)
        self.installer = self.write('installer.sh', template)
        self.write('urls', json.dumps(self.urls))

    def write(self, name, text):
        path = self.root / name
        path.write_text(text)
        return path

    def executable(self, name, text):
        path = self.write(name, text)
        path.chmod(0o700)
        return path

    def run_script(self, script=None, args=None, expected=0):
        result = subprocess.run(['/bin/sh', str(script or self.installer), *(args if args is not None else ['--install-dir', str(self.destination)])],
                                env=self.env, text=True, capture_output=True, timeout=15)
        if expected == 0:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def clear_requests(self):
        directory = self.root / 'requests'
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir()

    def requests(self):
        directory = self.root / 'requests'
        return [json.loads((directory / str(index)).read_text()) for index in range(len(list(directory.iterdir())))]

    def bootstrap(self, tag=None):
        child = self.executable('child.sh', '#!/bin/sh\nprintf \'%s\\n\' "$@" > "$FIXTURE/arguments"\n')
        suffix = f'download/{tag}' if tag else 'latest/download'
        self.urls[f'https://github.com/leonfox28/voytty/releases/{suffix}/voytty-install.sh'] = str(child)
        self.write('urls', json.dumps(self.urls))
        return ROOT / 'install/install.sh'

    def test_bootstrap_latest_exact_tags_and_argument_forwarding(self):
        for tag in (None, 'v1.2.3', 'v1.2.3-rc.1'):
            with self.subTest(tag=tag):
                script = self.bootstrap(tag)
                path = str(self.root / 'bin with spaces')
                args = ['--install-dir', path] + (['--version', tag] if tag else [])
                self.run_script(script, args)
                self.assertEqual((self.root / 'arguments').read_text().splitlines(), ['--install-dir', path])

    def test_bootstrap_rejects_invalid_tags_without_download(self):
        for tag in ('main', 'v1.2.3+build', 'v01.2.3', 'v1.2.3-01', 'v1.2.3/other'):
            self.run_script(self.bootstrap(), ['--version', tag], expected=1)
        self.assertEqual(self.requests(), [])

    def test_incomplete_bootstrap_does_not_run_installation_body(self):
        source = (ROOT / 'install/install.sh').read_text()
        truncated = self.write('truncated.sh', source[:source.index('temporary=$(mktemp')])
        self.run_script(truncated, [], expected=1)
        self.assertEqual(self.requests(), [])

    def test_bootstrap_environment_default_and_explicit_override(self):
        self.env['VOYTTY_INSTALL_DIR'] = str(self.root / 'env-bin')
        self.run_script(self.bootstrap(), [])
        self.assertEqual((self.root / 'arguments').read_text().splitlines(), ['--install-dir', self.env['VOYTTY_INSTALL_DIR']])
        self.run_script(self.bootstrap(), ['--install-dir', str(self.destination)])
        self.assertEqual((self.root / 'arguments').read_text().splitlines(), ['--install-dir', str(self.destination)])

    def test_install_does_not_execute_in_system_temporary_directory(self):
        self.env['TMPDIR'] = str(self.root / 'unusable-system-tmp')
        result = self.run_script()
        self.assertTrue((self.destination / 'voytty').is_file())
        candidates = (self.root / 'candidate-ran').read_text().splitlines()
        self.assertTrue(all(Path(path).parent.parent == self.destination for path in candidates))
        self.assertEqual(list(self.destination.glob('.voytty-install.*')), [])
        self.assertIn('Next:', result.stdout)
        self.assertIn(' setup', result.stdout)
        self.assertIn('Verifying', result.stdout)

    def test_path_guidance_quotes_shell_metacharacters(self):
        self.destination = self.root / "a'b $(touch SHOULD_NOT_EXIST)"
        result = self.run_script()
        command = next(line.strip() for line in result.stdout.splitlines() if line.strip().startswith('export PATH='))
        verified = subprocess.run(['/bin/sh', '-c', command + '\nprintf "%s" "$PATH"'], env=self.env,
                                  cwd=self.root, text=True, capture_output=True, check=True)
        self.assertEqual(verified.stdout, str(self.destination) + ':' + self.env['PATH'])
        self.assertFalse((self.root / 'SHOULD_NOT_EXIST').exists())
        next_command = next(line.removeprefix('Next: ') for line in result.stdout.splitlines() if line.startswith('Next: '))
        self.assertEqual(shlex.split(next_command), [str(self.destination / 'voytty'), 'setup'])

    def test_trailing_slash_and_path_present_still_print_setup(self):
        self.env['PATH'] = str(self.destination) + ':' + self.env['PATH']
        result = self.run_script(args=['--install-dir', str(self.destination) + '///'])
        self.assertNotIn('Add ', result.stdout)
        self.assertIn('Next:', result.stdout)

    def test_existing_destination_is_rejected_before_network(self):
        self.destination.mkdir()
        original = self.destination / 'voytty'
        original.write_text('preserve me')
        result = self.run_script(expected=1)
        self.assertIn(str(original), result.stderr)
        self.assertEqual(original.read_text(), 'preserve me')
        self.assertEqual(self.requests(), [])

    def test_symlink_with_trailing_slash_is_rejected(self):
        self.destination.symlink_to(self.bin, target_is_directory=True)
        self.run_script(args=['--install-dir', str(self.destination) + '/'], expected=1)
        self.assertEqual(self.requests(), [])

    def test_missing_curl_has_no_install_directory_side_effect(self):
        empty = self.root / 'empty-path'
        empty.mkdir()
        self.env['PATH'] = str(empty)
        result = self.run_script(expected=1)
        self.assertIn('curl is required', result.stderr)
        self.assertFalse(self.destination.exists())

    def test_rosetta_selects_arm64_but_intel_and_32_bit_userspace_fail(self):
        self.env.update(FIXTURE_OS='Darwin', FIXTURE_ARM='1')
        self.assertIn('aarch64-apple-darwin', self.run_script().stdout)
        for settings, diagnostic in [({'FIXTURE_ARM': '0'}, 'Intel macOS'),
                                     ({'FIXTURE_OS': 'Linux', 'FIXTURE_BITS': '32'}, '64-bit Linux userspace')]:
            self.env.update(settings)
            result = self.run_script(expected=1)
            self.assertIn(diagnostic, result.stderr)

    def test_shadowed_binary_and_zterm_are_reported_without_running_them(self):
        for name in ('voytty', 'zterm'):
            self.executable('bin/' + name, '#!/bin/sh\nexit 99\n')
        result = self.run_script()
        self.assertIn(str(self.bin / 'voytty'), result.stdout)
        self.assertIn('transition-from-zterm', result.stdout)

    def test_transient_download_restarts_and_security_options_apply(self):
        self.env['REPLIES'] = json.dumps([[22, '503']])
        self.run_script()
        requests = self.requests()
        self.assertEqual(len(requests), 4)
        for args in requests:
            for option, value in [('--proto', '=https'), ('--proto-redir', '=https'),
                                  ('--max-redirs', '5'), ('--speed-time', '30')]:
                self.assertEqual(args[args.index(option) + 1], value)
            self.assertIn('--max-filesize', args)
        self.assertEqual(requests[1][requests[1].index('--max-time') + 1], '59')

    def test_permanent_errors_are_not_retried_in_either_stage(self):
        for script, args in [(self.installer, None), (self.bootstrap(), [])]:
            for reply in ([22, '404'], [60, '000'], [63, '200']):
                self.clear_requests()
                self.env['REPLIES'] = json.dumps([reply])
                self.run_script(script, args, expected=1)
                self.assertEqual(len(self.requests()), 1)

    def test_attempts_share_one_download_budget(self):
        self.env['REPLIES'] = json.dumps([[28, '000']] * 4)
        self.env['TRANSFER_SECONDS'] = '45'
        self.run_script(expected=1)
        requests = self.requests()
        self.assertEqual([args[args.index('--max-time') + 1] for args in requests], ['60', '14'])
        self.assertEqual((self.root / 'clock').read_text(), '1060')

    def test_large_download_budget_and_attempt_cap_in_both_stages(self):
        self.env['REPLIES'] = json.dumps([[28, '000']] * 4)
        for stage in ('install.sh', 'versioned.sh.in'):
            with self.subTest(stage=stage):
                self.clear_requests()
                source = (ROOT / 'install' / stage).read_text()
                start = source.index('download() {')
                end = source.index('\n}', start) + 2
                harness = self.write('download.sh', source[start:end] + '\ndownload "$1" "$2" "$3"\n')
                self.run_script(harness, [self.base + '/archive', str(self.root / 'download'), '2097152'], expected=1)
                self.assertEqual([args[args.index('--max-time') + 1] for args in self.requests()],
                                 ['600', '599', '597', '593'])

    def test_installer_environment_default(self):
        self.env['VOYTTY_INSTALL_DIR'] = str(self.destination)
        self.run_script(args=[])
        self.assertTrue((self.destination / 'voytty').is_file())


if __name__ == '__main__':
    unittest.main()

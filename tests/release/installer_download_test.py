#!/usr/bin/env python3
"""Exercise each standalone downloader against real loopback HTTPS responses."""

import http.server
import os
from pathlib import Path
import ssl
import subprocess
import tempfile
import threading
import unittest

from https_fixture import FixtureHTTPServer

ROOT = Path(__file__).resolve().parents[2]


class DownloadTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix='voytty-download-test-')
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.root = Path(cls.temporary.name)
        config = cls.root / 'certificate.conf'
        config.write_text('''[req]
prompt = no
distinguished_name = subject
x509_extensions = extensions
[subject]
CN = 127.0.0.1
[extensions]
subjectAltName = IP:127.0.0.1
basicConstraints = critical,CA:TRUE
keyUsage = critical,digitalSignature,keyEncipherment,keyCertSign
''')
        certificate, key = cls.root / 'certificate.crt', cls.root / 'private.key'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                        '-config', str(config), '-keyout', str(key), '-out', str(certificate)],
                       check=True, capture_output=True, timeout=20)
        cls.calls = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                cls.calls.append(self.path)
                if self.path == '/http':
                    self.send_response(302)
                    self.send_header('Location', f'http://127.0.0.1:{self.server.server_port}/unsafe')
                    body = b''
                elif self.path == '/https':
                    self.send_response(302)
                    self.send_header('Location', '/ok')
                    body = b''
                elif self.path == '/missing':
                    self.send_response(404)
                    body = b'not found'
                elif self.path == '/chunked-oversize':
                    self.send_response(200)
                    self.send_header('Transfer-Encoding', 'chunked')
                    self.end_headers()
                    chunk = b'x' * 2048
                    try:
                        self.wfile.write(f'{len(chunk):x}\r\n'.encode('ascii') + chunk + b'\r\n0\r\n\r\n')
                    except (BrokenPipeError, ConnectionResetError):
                        pass
                    return
                elif self.path == '/retry' and cls.calls.count('/retry') == 1:
                    self.send_response(503)
                    body = b'failed attempt'
                else:
                    self.send_response(200)
                    body = b'verified download'
                self.send_header('Content-Length', str(2048 if self.path == '/oversize' else len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *args):
                pass

        cls.server = FixtureHTTPServer(('127.0.0.1', 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(certificate, key)
        cls.server.socket = context.wrap_socket(cls.server.socket, server_side=True)
        cls.worker = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.worker.start()
        cls.addClassCleanup(cls.close_server)
        cls.env = {**os.environ, 'CURL_CA_BUNDLE': str(certificate), 'NO_PROXY': '127.0.0.1'}

    @classmethod
    def close_server(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.worker.join(timeout=5)

    def execute(self, stage, endpoint, maximum=64):
        self.calls.clear()
        source = (ROOT / 'install' / stage).read_text()
        start = source.index('download() {')
        end = source.index('\n}', start) + 2
        script = source[start:end] + '\ndownload "$1" "$2" "$3"\n'
        harness = self.root / 'download.sh'
        harness.write_text(script)
        output = self.root / 'output'
        result = subprocess.run(['/bin/sh', '-eu', str(harness),
                                 f'https://127.0.0.1:{self.server.server_port}/{endpoint}',
                                 str(output), str(maximum)],
                                env=self.env, text=True, capture_output=True, timeout=10)
        return result, output

    def test_real_https_redirect_retry_and_failure_boundaries(self):
        for stage in ('install.sh', 'versioned.sh.in'):
            for endpoint in ('https', 'retry'):
                with self.subTest(stage=stage, endpoint=endpoint):
                    result, output = self.execute(stage, endpoint)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(output.read_bytes(), b'verified download')
                    self.assertEqual(len(self.calls), 2)
            for endpoint in ('http', 'missing', 'oversize', 'chunked-oversize'):
                with self.subTest(stage=stage, endpoint=endpoint):
                    result, _ = self.execute(stage, endpoint)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(self.calls, ['/' + endpoint])
                    self.assertIn('Download failed:', result.stderr)

    def test_redirected_stderr_log_exceeding_ulimit_does_not_trigger_sigxfsz(self):
        for stage in ('install.sh', 'versioned.sh.in'):
            with self.subTest(stage=stage):
                self.calls.clear()
                source = (ROOT / 'install' / stage).read_text()
                start = source.index('download() {')
                end = source.index('\n}', start) + 2
                script = source[start:end] + '\ndownload "$1" "$2" "$3"\n'
                harness = self.root / f'download-{stage}.sh'
                harness.write_text(script)
                output = self.root / f'output-{stage}'
                stderr_log = self.root / f'stderr-{stage}.log'
                stderr_log.write_bytes(b'L' * 4096)
                with stderr_log.open('ab') as log_file:
                    result = subprocess.run(
                        ['/bin/sh', '-eu', str(harness),
                         f'https://127.0.0.1:{self.server.server_port}/missing',
                         str(output), '64'],
                        env=self.env, stdout=subprocess.PIPE, stderr=log_file, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                log_text = stderr_log.read_text(errors='replace')
                self.assertIn('Download failed:', log_text)
                self.assertIn('HTTP 404, curl status 22', log_text)


if __name__ == '__main__':
    unittest.main()

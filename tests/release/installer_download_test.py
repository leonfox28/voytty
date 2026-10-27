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
            for endpoint in ('http', 'missing', 'oversize'):
                with self.subTest(stage=stage, endpoint=endpoint):
                    result, _ = self.execute(stage, endpoint)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(self.calls, ['/' + endpoint])
                    self.assertIn('Download failed:', result.stderr)


if __name__ == '__main__':
    unittest.main()

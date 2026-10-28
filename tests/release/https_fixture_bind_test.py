#!/usr/bin/env python3
"""Socket-free regressions for fixture binding and readiness publication."""

from __future__ import annotations

import http.server
import importlib.util
import pathlib
import socket
import socketserver
import tempfile
from unittest import mock


fixture_path = pathlib.Path(__file__).with_name("https_fixture.py")
spec = importlib.util.spec_from_file_location("voytty_https_fixture", fixture_path)
if spec is None or spec.loader is None:
    raise SystemExit("unable to load HTTPS fixture module")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)

server = object.__new__(fixture.FixtureHTTPServer)
server.server_address = ("before-bind.invalid", 0)
bound_address = ("127.0.0.1", 43123)


def fake_tcp_bind(instance: object) -> None:
    if instance is not server:
        raise AssertionError("TCP bind received the wrong server instance")
    instance.server_address = bound_address  # type: ignore[attr-defined]


with (
    mock.patch.object(
        socketserver.TCPServer, "server_bind", side_effect=fake_tcp_bind
    ) as tcp_bind,
    mock.patch.object(
        http.server.HTTPServer,
        "server_bind",
        side_effect=AssertionError("inherited HTTPServer bind was called"),
    ),
    mock.patch.object(
        socket,
        "getfqdn",
        side_effect=AssertionError("hostname lookup was called"),
    ),
):
    fixture.FixtureHTTPServer.server_bind(server)

if tcp_bind.call_count != 1:
    raise AssertionError(f"TCP bind was called {tcp_bind.call_count} times")
if (server.server_name, server.server_port) != bound_address:
    raise AssertionError("fixture did not publish the post-bind address")
if not issubclass(fixture.FixtureHTTPServer, http.server.ThreadingHTTPServer):
    raise AssertionError("fixture no longer preserves threaded HTTP behavior")

with tempfile.TemporaryDirectory(prefix="voytty-port-publication-") as temporary:
    port_file = pathlib.Path(temporary) / "port"
    original_replace = pathlib.Path.replace
    published = []

    def observe_publication(pending: pathlib.Path, destination: pathlib.Path) -> pathlib.Path:
        if destination != port_file or destination.exists():
            raise AssertionError("readiness was visible before atomic publication")
        if pending.parent != destination.parent or pending.read_text(encoding="ascii") != "43123":
            raise AssertionError("ready port was not complete on the same filesystem")
        published.append(destination)
        return original_replace(pending, destination)

    with mock.patch.object(pathlib.Path, "replace", new=observe_publication):
        fixture.publish_port(port_file, 43123)
    if published != [port_file] or port_file.read_text(encoding="ascii") != "43123":
        raise AssertionError("ready port publication did not complete exactly once")
    if list(port_file.parent.iterdir()) != [port_file]:
        raise AssertionError("ready port publication left a staging file")

with tempfile.TemporaryDirectory(prefix="voytty-port-interruption-") as temporary:
    port_file = pathlib.Path(temporary) / "port"
    original_write = pathlib.Path.write_text

    def interrupted_write(path: pathlib.Path, text: str, **kwargs: object) -> int:
        if path == port_file:
            raise AssertionError("readiness path was opened before the write completed")
        original_write(path, text[:2], **kwargs)
        raise OSError("fixture write interrupted")

    with mock.patch.object(pathlib.Path, "write_text", new=interrupted_write):
        try:
            fixture.publish_port(port_file, 43123)
        except OSError:
            pass
        else:
            raise AssertionError("interrupted port write unexpectedly succeeded")
    if port_file.exists():
        raise AssertionError("interrupted write advertised readiness")

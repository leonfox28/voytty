# Optional self-hosted Iroh Relay

## Product boundary

Phase One endpoints currently use Iroh's official n0 production infrastructure
by default. This document covers the separately operable, optional self-hosted
Relay; it is not silently added to that default profile.

voytty does not implement or fork a Relay data plane. Its optional image
downloads the official Iroh 1.3.0 Linux musl artifact, selects it from
BuildKit's `TARGETARCH`, verifies the upstream SHA-256, and copies the binary
and Alpine CA bundle into a shell-free `scratch` runtime. The process runs as
numeric UID/GID 65532.

Canonical upstream metadata lives in `deploy/relay/artifact.sh`:

| Docker architecture | Official target | SHA-256 |
| --- | --- | --- |
| `amd64` | `x86_64-unknown-linux-musl` | `677f4c62342a6ba8044459b5fd4302f2b1dcb8402542072e3a4ade5039bc0b9e` |
| `arm64` | `aarch64-unknown-linux-musl` | `dc4b9d620642026966d498763ec8ba3a6eefde8d39994b2d63e3d15d7af770f8` |

These values match the official
[Iroh v1.3.0 release](https://github.com/n0-computer/iroh/releases/tag/v1.3.0).
The checksum is verified at the artifact-download boundary. Deployment does
not add another image digest parser; Docker and GHCR already transfer images by
content address.

The Relay forwards voytty's end-to-end encrypted packets and stores no
terminal state, device key, session, or business record. Like any network
forwarder, it can still observe connection metadata such as source addresses,
endpoint identifiers, timing, and encrypted traffic sizes.

## Supported deployment

There is one supported configuration for the optional self-hosted Relay:

- OpenResty or another same-host reverse proxy owns public TLS;
- Iroh serves plain HTTP inside the container on TCP 38451;
- Docker publishes that listener only as `127.0.0.1:38451`;
- QAD and metrics are disabled;
- Relay access is `everyone`, with 8 MiB/s per client and a 16 MiB burst;
- Docker's rotating and compressed `local` log driver bounds logs.

The complete deployment consists of only:

- `deploy/relay/compose.yaml`;
- `deploy/relay/relay.toml`;
- the local build inputs `Dockerfile`, `.dockerignore` and `artifact.sh`.

Compose project and container are both named `voytty-relay`. The image supplies
its own default `--config-path /etc/iroh-relay/relay.toml` command, so Compose
does not repeat it. There is no `.env` image indirection,
container healthcheck, custom probe, metrics port, certificate volume, monitor,
or direct TLS/ACME/QAD variant.

## Local verification

Run each contract once at its owning boundary:

```bash
sh tests/relay/static.sh
sh tests/relay/verify-upstream.sh
sh tests/relay/build-platforms.sh
sh tests/relay/smoke.sh
sh tests/relay/secret-scan.sh
```

- `verify-upstream.sh` downloads and verifies both official archives.
- `build-platforms.sh` builds and executes the amd64 and arm64 images.
- `static.sh` checks the minimal image, publication policy, configuration,
  and rendered Compose contract.
- `smoke.sh` reuses the native-platform image built by `build-platforms.sh`,
  then verifies `/healthz`, `/generate_204`, and an authenticated Iroh handshake without rebuilding it or
  introducing a second Compose shape.

The public acceptance probe creates an ephemeral Iroh endpoint identity and
succeeds only when the requested Relay reports an authenticated connection:

```bash
sh tests/relay/public-handshake.sh https://relay.example.com
```

It passes `RelayConfig::new(url, None)` because this deployment has no UDP QAD
endpoint. It performs one connection attempt; restart/reconnect and rollback
exercises are not part of normal release acceptance.

## Publication status

Publication is paused. Native voytty releases do not build or publish a companion
GHCR image, and the automated/manual image publisher has been removed. Restore
publication only when a future task explicitly requests it.

The existing `ghcr.io/leonfox28/zterm-relay` images and deployed infrastructure
remain historical assets; their `latest` tag no longer tracks new native voytty
versions. Existing Compose deployments and isolated relay build/runtime tests
retain their current behavior. Historical production/development publication
evidence is recorded in [Phase Zero verification](phase-zero-verification.md).

## Manual deployment and updates

Copy the reviewed `deploy/relay/` bundle to the 1Panel Compose directory and run:

```bash
cd /opt/1panel/docker/compose/voytty-relay
docker compose up --build -d
```

Compose builds the versioned image locally using the verified upstream archive.
The container uses a read-only filesystem with all capabilities dropped, a
128-process limit, a 256 MiB memory limit, and privilege escalation disabled.

After an update, accept the deployment once:

```bash
curl --fail http://127.0.0.1:38451/healthz
curl --fail https://relay.example.com/healthz
curl --fail --output /dev/null https://relay.example.com/generate_204
sh tests/relay/public-handshake.sh https://relay.example.com
```

Also inspect the host once to confirm that only loopback TCP 38451 is published,
the container is named `voytty-relay`, its configured image is
`voytty/iroh-relay:1.3.0`, and its log driver is `local`. Stop after
those checks pass; do not restart or switch images merely to repeat the same
evidence.

Credentials, SSH keys, and machine-specific secrets must never be copied into
the repository. This deployment does not need a real `.env` file.

## Reverse-proxy requirements

Use your own HTTPS Relay URL in the commands above. The proxy connects to
`http://127.0.0.1:38451` over loopback. The reverse proxy must preserve path,
query data, the long-lived HTTP/1.1 WebSocket upgrade on `/relay`, and Iroh's
authentication headers. Equivalent Nginx/OpenResty semantics are:

```nginx
map $http_upgrade $voytty_connection_upgrade {
    default upgrade;
    ''      close;
}

location / {
    proxy_pass http://127.0.0.1:38451;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $voytty_connection_upgrade;
    proxy_set_header Sec-WebSocket-Protocol $http_sec_websocket_protocol;
    proxy_set_header X-Iroh-Relay-Client-Auth-V1 $http_x_iroh_relay_client_auth_v1;
    proxy_buffering off;
    proxy_read_timeout 1d;
    proxy_send_timeout 1d;
}
```

The repository does not own the selected server's existing OpenResty or
Cloudflare configuration. Integrate these semantics with the host's conventions
instead of overwriting unrelated proxy configuration.

## NAT and QAD boundary

Relay fallback and direct NAT traversal are separate paths. A successful NAT
traversal uses a direct end-to-end QUIC path and bypasses this Relay. If direct
establishment fails, encrypted traffic can still use the Relay; forwarding does
not require QAD.

The current deployment sets `enable_quic_addr_discovery = false` and publishes
no UDP port because an HTTP reverse proxy cannot forward QAD. Phase One's
[Foundation Gate](foundation-gate.md) instead exercises four isolated network
controls. In the current nested Colima/Patchbay/TUN run, official-n0 Case A
completed QAD IPv4 and obtained global-v4 candidates on both endpoints, but
the observed mappings varied by QAD destination and the connection remained
relayed. Case B selected Direct after exact known candidates were injected;
Case C completed three encrypted streams through official WSS/TCP Relay with
non-DNS UDP blocked; and M10 Case D used a disposable lab-only Relay/QAD to
promote Relay to Direct automatically without `external_addr`.

Case D proves the retained Iroh candidate discovery/exchange path inside the
controlled double-NAT lab; it is not a supported self-hosted product profile.
Case A still neither passes Direct selection nor establishes that official QAD
generally fails on ordinary physical networks. A later M10 physical run used
the unchanged official-n0 profile from cellular macOS to Debian behind a home
IPv4 NAT. After removing deployment-local fake-IP interception of Iroh DNS and
UDP, both installed peers selected Direct with no Relay path and carried an
interactive terminal. That accepts Direct for the tested topology, not every
NAT or network. M10 remains open for the separately recorded initial-attachment
correlation defect and its other release-acceptance items. This evidence, or a
concrete self-hosting requirement without a reverse proxy, can justify a
separately reviewed UDP/TLS deployment later.

## Failure handling

The service is stateless. If its container has an ordinary runtime problem,
recreate it:

```bash
docker compose up -d --force-recreate
```

To roll back a local build, restore the previous reviewed deployment bundle,
including its pinned archive checksums and Compose image tag, then run
`docker compose up --build -d`. Run the health and handshake checks above.

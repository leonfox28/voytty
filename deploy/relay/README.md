# Relay bundle

This directory packages the unmodified official `iroh-relay` 1.3.0 binary and
contains the one supported deployment shape.

- `artifact.sh` owns the upstream version, architecture mapping, download URLs,
  and SHA-256 checksums.
- `Dockerfile` verifies the selected official archive and copies the relay plus
  CA bundle into a shell-free `scratch` image running as UID/GID 65532.
- `relay.toml` enables the open Relay listener on container TCP 38451 and
  explicitly disables unused metrics and UDP QAD.
- `compose.yaml` publishes that listener only on host loopback, where a
  same-host TLS reverse proxy can reach it.

Copy the bundle to `/opt/1panel/docker/compose/voytty-relay` and run
`docker compose up --build -d`. Compose builds the versioned image locally.
Publication is paused. The container runs with a read-only root filesystem,
no Linux capabilities, a 128-process limit and 256 MiB of memory.

Public TLS, WebSocket forwarding, and certificates belong to the host reverse
proxy. This bundle deliberately exposes no direct TLS/ACME template, metrics
listener, UDP QAD listener, custom health binary, monitor, database, or volume.
The Relay forwards end-to-end encrypted traffic and stores no voytty terminal
state.

See `docs/relay.md` for publication, deployment, and one-time acceptance
commands.

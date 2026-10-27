# Official installer boundary

`install.sh` is the small mutable GitHub HTTPS bootstrap. It resolves either
the latest stable Release or one exact published tag, downloads that Release's
immutable `voytty-install.sh`, and executes it. Both stages require curl and
enforce HTTPS-only redirects, bounded downloads, and bounded transient retries.
The bootstrap cannot defend against compromise of the HTTPS-delivered script
source; users who need a fixed trust root should follow the reviewed/manual
verification path in `docs/install.md`.

`versioned.sh.in` is never served directly. `voytty-release-tool prepare`
generates the immutable Release installer from the signed manifest's single
artifact inventory. The generated script authenticates the manifest/archive
against its embedded digest table, then delegates Ed25519 verification,
candidate identity checks, fsync, and atomic no-clobber publication to the
digest-authenticated candidate binary.

The candidate's self-verification checks consistency, rather than establishing
an independent trust root for first installation. The existing trusted binary
owns verification before executing any new candidate during `voytty update`.
Candidates are staged privately on the destination filesystem so installation
and update do not depend on executable system temporary directories.

`just installer-check` covers bootstrap arguments, platform detection, safe PATH
guidance, network bounds/retries and HTTPS redirects. Signed installer acceptance
remains in the release workflow, using the same immutable generated installer.

Neither stage runs setup, creates `~/.voytty`, starts a daemon, changes shell
files, invokes sudo, or registers a service.

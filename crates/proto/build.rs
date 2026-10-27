//! Reproducible protobuf code generation using a vendored `protoc` binary.

use std::{error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let proto_root = workspace_root.join("proto");
    let schemas = [
        "common.proto",
        "wire.proto",
        "local.proto",
        "pairing.proto",
        "device.proto",
        "transport.proto",
        "session.proto",
        "terminal.proto",
        "upload.proto",
    ]
    .map(|name| proto_root.join("voytty/v2").join(name));
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    for schema in &schemas {
        println!("cargo:rerun-if-changed={}", schema.display());
    }

    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc);
    // These generated values carry bearer tickets, nonces, proofs, opaque
    // payloads, terminal/input bytes, working directories, or route values.
    // Their Debug ownership is implemented manually in `src/lib.rs`; keep this
    // exact list in sync with the schemas rather than suppressing diagnostics
    // package-wide.
    config.skip_debug([
        ".voytty.v2.WireFrame",
        ".voytty.v2.UploadChunk",
        ".voytty.v2.UploadCompleted",
        ".voytty.v2.PairTicketV1",
        ".voytty.v2.PairBegin",
        ".voytty.v2.PairChallenge",
        ".voytty.v2.PairProof",
        ".voytty.v2.PairAccepted",
        ".voytty.v2.LocalPairCreateResponse",
        ".voytty.v2.LocalPairAcceptRequest",
        ".voytty.v2.LocalSessionUnaryRequest",
        ".voytty.v2.LocalSessionTunnelOpenRequest",
        ".voytty.v2.LocalSessionTunnelData",
        ".voytty.v2.LocalStatusResponse",
        ".voytty.v2.LocalValidateSetupRequest",
        ".voytty.v2.RelayRouteCacheV1",
        ".voytty.v2.SessionSummary",
        ".voytty.v2.SessionCreateRequest",
        ".voytty.v2.ResumeViewId",
        ".voytty.v2.TerminalAttachRequest",
        ".voytty.v2.TerminalInput",
        ".voytty.v2.TerminalClipboardWrite",
        ".voytty.v2.TerminalNotification",
        ".voytty.v2.TerminalNotification.content",
        ".voytty.v2.TerminalNotificationOsc777",
        ".voytty.v2.TerminalCell",
        ".voytty.v2.TerminalHyperlink",
        ".voytty.v2.TerminalSurfaceRow",
        ".voytty.v2.TerminalSurface",
        ".voytty.v2.TerminalSemanticSnapshot",
        ".voytty.v2.TerminalSemanticRowPatch",
        ".voytty.v2.TerminalSemanticDelta",
        ".voytty.v2.TerminalSemanticHistoryWindowFrame",
    ]);
    config.compile_protos(&schemas, &[proto_root])?;

    Ok(())
}

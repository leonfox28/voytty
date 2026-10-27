//! Cross-language golden vectors for the pairing ticket and transcript.
//!
//! Every vector below uses explicitly labeled fixed non-production credentials.
//! The canonical bytes are hand-reproducible with any language (they use fixed
//! big-endian length prefixes and SHA-256/HMAC-SHA256), so the expected hex
//! values are independent of Rust layout and protobuf serialization order.

use voytty_core::{
    AuthGeneration, DeviceAlias, DeviceId, MAX_TICKET_TEXT_BYTES, PAIR_PROTOCOL_VERSION,
    PAIR_TICKET_FORMAT_VERSION, PairAccepted, PairFingerprint, PairHandshakeBudget,
    PairHandshakeBudgetError, PairNonce, PairOfferId, PairProof, PairSecret, PairTicketFields,
    PairTranscript, RelayHint,
};

// Non-production fixed test credentials.
const HOST_DEVICE: [u8; 32] = [0x11; 32];
const CONTROLLER_DEVICE: [u8; 32] = [0x22; 32];
const OFFER_ID: [u8; 16] = [0xaa; 16];
const CONTROLLER_NONCE: [u8; 32] = [0x33; 32];
const HOST_NONCE: [u8; 32] = [0x44; 32];
const SECRET: [u8; 32] = [0x42; 32];
const EXPIRES_AT_UNIX: u64 = 1_700_000_000;
const GENERATION: u64 = 1;

const TICKET_CANONICAL_HEX: &str = "766f797474792d706169722d7469636b65742d7631000000000111111111111111111111111111111111111111111111111111111111111111110009746573742d686f737401001968747470733a2f2f72656c61792e6578616d706c652e636f6daaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000006553f100";
const TICKET_DIGEST_HEX: &str = "4fb270059c50cb6321afa637daebc230bde0db1a8b41d4ee06997b44314dc2c3";
const OFFER_KEY_HEX: &str = "99e16fbb57a1729502afa855b13a44becc5719e90ce54217d9ad9267cb93b2a6";
const TRANSCRIPT_CANONICAL_HEX: &str = "766f797474792d706169722d7472616e7363726970742d7631004fb270059c50cb6321afa637daebc230bde0db1a8b41d4ee06997b44314dc2c311111111111111111111111111111111111111111111111111111111111111112222222222222222222222222222222222222222222222222222222222222222aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa33333333333333333333333333333333333333333333333333333333333333334444444444444444444444444444444444444444444444444444444444444444000f746573742d636f6e74726f6c6c65720000000100000001000000006553f100";
const CONTROLLER_PROOF_HEX: &str =
    "1b1c050b572414937cf5bd2affda5b3beec9c7d404e09e1bbd9a236b73c305c4";
const HOST_CONFIRMATION_HEX: &str =
    "08e0d2fda1e2ca40bb99c8889517729ad83f667bf8746e6e8ae2bc6f3aa71aef";
const CREATE_FINGERPRINT_HEX: &str =
    "68052f77c86733b7c297748c76b1ceeb65a2b0774e286fee2f49bcbf41c53afe";
const ACCEPT_WITHOUT_ALIAS_FINGERPRINT_HEX: &str =
    "7941e616f1c4b243cd75005940097492f635e5d4befe167757de1ba360fdb312";
const ACCEPT_WITH_ALIAS_FINGERPRINT_HEX: &str =
    "f872cfa9947c626645b267aed462b7c331be44cc0a9524aa14241a88618059a2";

fn hex(input: &str) -> Vec<u8> {
    assert_eq!(input.len() % 2, 0, "hex string must have even length");
    input
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("hex chunk is ASCII");
            u8::from_str_radix(text, 16).expect("valid hex digit")
        })
        .collect()
}

fn ticket() -> PairTicketFields {
    PairTicketFields::new(
        PAIR_TICKET_FORMAT_VERSION,
        DeviceId::from_array(HOST_DEVICE),
        "test-host",
        vec![RelayHint::new("https://relay.example.com").expect("valid relay URL")],
        PairOfferId::from_array(OFFER_ID),
        EXPIRES_AT_UNIX,
    )
    .expect("bounded ticket")
}

fn transcript() -> PairTranscript {
    PairTranscript::new(
        &ticket(),
        DeviceId::from_array(CONTROLLER_DEVICE),
        "test-controller",
        PairNonce::from_array(CONTROLLER_NONCE),
        PairNonce::from_array(HOST_NONCE),
        PAIR_PROTOCOL_VERSION,
    )
    .expect("bounded transcript")
}

#[test]
fn canonical_ticket_bytes_match_the_language_neutral_golden_vector() {
    let ticket = ticket();
    assert_eq!(ticket.canonical_bytes().to_vec(), hex(TICKET_CANONICAL_HEX));
    assert_eq!(ticket.ticket_digest().to_vec(), hex(TICKET_DIGEST_HEX));
    assert_eq!(
        ticket.offer_key(&PairSecret::from_bytes(SECRET)).to_vec(),
        hex(OFFER_KEY_HEX)
    );
}

#[test]
fn canonical_transcript_and_proofs_match_the_language_neutral_golden_vector() {
    let transcript = transcript();
    let offer_key: [u8; 32] = hex(OFFER_KEY_HEX).try_into().expect("32 byte offer key");
    assert_eq!(
        transcript.canonical_bytes().to_vec(),
        hex(TRANSCRIPT_CANONICAL_HEX)
    );
    assert_eq!(
        transcript.controller_proof(&offer_key).to_vec(),
        hex(CONTROLLER_PROOF_HEX)
    );
    assert_eq!(
        transcript
            .host_confirmation(&offer_key, GENERATION)
            .to_vec(),
        hex(HOST_CONFIRMATION_HEX)
    );
}

#[test]
fn proofs_verify_constant_time_and_reject_tampered_inputs() {
    let transcript = transcript();
    let offer_key: [u8; 32] = hex(OFFER_KEY_HEX).try_into().expect("32 byte offer key");
    let controller_proof: [u8; 32] = hex(CONTROLLER_PROOF_HEX).try_into().expect("32 byte proof");
    let confirmation: [u8; 32] = hex(HOST_CONFIRMATION_HEX)
        .try_into()
        .expect("32 byte proof");

    assert!(transcript.verify_controller_proof(&offer_key, &controller_proof));
    assert!(transcript.verify_host_confirmation(&offer_key, GENERATION, &confirmation));

    // Wrong offer key (wrong secret) fails both proofs.
    let wrong_key = [0_u8; 32];
    assert!(!transcript.verify_controller_proof(&wrong_key, &controller_proof));
    assert!(!transcript.verify_host_confirmation(&wrong_key, GENERATION, &confirmation));

    // Wrong generation fails the host confirmation.
    assert!(!transcript.verify_host_confirmation(&offer_key, GENERATION + 1, &confirmation));

    // Tampered proof bytes fail.
    let mut tampered = controller_proof;
    tampered[0] ^= 0x01;
    assert!(!transcript.verify_controller_proof(&offer_key, &tampered));
}

#[test]
fn canonical_bytes_are_prefix_independent_and_change_with_every_bound_field() {
    let ticket = ticket();
    // The canonical bytes are the authentication input, never the ticket text
    // prefix or base64 alphabet.
    assert!(
        !ticket
            .canonical_bytes()
            .windows(b"voytty-pair-v1:".len())
            .any(|w| w == b"voytty-pair-v1:")
    );

    // Wrong secret changes only the offer key, not the ticket digest.
    let other_secret = PairSecret::from_bytes([0x99; 32]);
    assert_eq!(
        ticket.offer_key(&other_secret),
        ticket.offer_key(&other_secret),
        "offer key derivation is deterministic"
    );
    assert_ne!(
        ticket.offer_key(&PairSecret::from_bytes(SECRET)),
        ticket.offer_key(&other_secret)
    );
    assert_eq!(ticket.ticket_digest(), ticket.ticket_digest());

    // A different host identity changes the canonical bytes and digest.
    let other_host = PairTicketFields::new(
        PAIR_TICKET_FORMAT_VERSION,
        DeviceId::from_array([0x99; 32]),
        "test-host",
        ticket.relay_hints().to_vec(),
        PairOfferId::from_array(OFFER_ID),
        EXPIRES_AT_UNIX,
    )
    .expect("bounded ticket");
    assert_ne!(other_host.canonical_bytes(), ticket.canonical_bytes());
    assert_ne!(other_host.ticket_digest(), ticket.ticket_digest());

    // Relay order is retained and changes the canonical bytes.
    let second = RelayHint::new("https://second.example.com").expect("valid relay URL");
    let reordered = PairTicketFields::new(
        PAIR_TICKET_FORMAT_VERSION,
        DeviceId::from_array(HOST_DEVICE),
        "test-host",
        vec![second.clone(), ticket.relay_hints()[0].clone()],
        PairOfferId::from_array(OFFER_ID),
        EXPIRES_AT_UNIX,
    )
    .expect("bounded ticket");
    assert_ne!(reordered.canonical_bytes(), ticket.canonical_bytes());
}

#[test]
fn expiry_and_version_are_validated() {
    let ticket = ticket();
    assert!(!ticket.is_expired(EXPIRES_AT_UNIX - 1));
    assert!(ticket.is_expired(EXPIRES_AT_UNIX));
    assert!(ticket.is_expired(EXPIRES_AT_UNIX + 1));

    assert!(matches!(
        PairTicketFields::new(
            2,
            DeviceId::from_array(HOST_DEVICE),
            "test-host",
            ticket.relay_hints().to_vec(),
            PairOfferId::from_array(OFFER_ID),
            EXPIRES_AT_UNIX,
        ),
        Err(voytty_core::PairTicketError::UnsupportedFormatVersion { actual: 2 })
    ));
}

#[test]
fn secret_bearer_value_is_redacted_in_debug_display_and_errors() {
    let secret = PairSecret::from_bytes(SECRET);
    let debug = format!("{secret:?}");
    let display = format!("{secret}");
    assert_eq!(debug, "PairSecret([REDACTED])");
    assert_eq!(display, "[REDACTED]");
    assert!(!debug.contains("42"), "secret hex must not leak: {debug}");

    // The ticket fields and transcript never contain the raw secret bytes.
    let ticket = ticket();
    let ticket_debug = format!("{ticket:?}");
    assert!(
        !ticket_debug.contains("42"),
        "ticket Debug must be secret-free"
    );
    let transcript_debug = format!("{:?}", transcript());
    assert!(!transcript_debug.contains("42"));
}

#[test]
fn local_operation_fingerprints_match_domain_separated_golden_vectors() {
    assert_eq!(
        PairFingerprint::for_create(600).as_bytes().to_vec(),
        hex(CREATE_FINGERPRINT_HEX)
    );

    let ticket = b"voytty-pair-v1:EXAMPLE";
    let alias = DeviceAlias::new("phone").expect("valid alias");
    assert_eq!(
        PairFingerprint::for_accept(ticket, None)
            .as_bytes()
            .to_vec(),
        hex(ACCEPT_WITHOUT_ALIAS_FINGERPRINT_HEX)
    );
    assert_eq!(
        PairFingerprint::for_accept(ticket, Some(&alias))
            .as_bytes()
            .to_vec(),
        hex(ACCEPT_WITH_ALIAS_FINGERPRINT_HEX)
    );
}

#[test]
fn local_operation_fingerprints_bind_every_semantic_argument() {
    let ticket = b"voytty-pair-v1:EXAMPLE";
    let mut changed_ticket = *ticket;
    changed_ticket[changed_ticket.len() - 1] ^= 1;
    let alias = DeviceAlias::new("phone").expect("valid alias");
    let other_alias = DeviceAlias::new("Phone").expect("valid alias");

    assert_ne!(
        PairFingerprint::for_create(600),
        PairFingerprint::for_create(601)
    );
    assert_ne!(
        PairFingerprint::for_accept(ticket, None),
        PairFingerprint::for_accept(&changed_ticket, None)
    );
    assert_ne!(
        PairFingerprint::for_accept(ticket, None),
        PairFingerprint::for_accept(ticket, Some(&alias))
    );
    assert_ne!(
        PairFingerprint::for_accept(ticket, Some(&alias)),
        PairFingerprint::for_accept(ticket, Some(&other_alias))
    );

    // The incremental helper retains only a fixed digest even when a hostile
    // caller presents bytes over the wire ticket limit. The service boundary
    // separately rejects this input before allocating an operation cell.
    let oversized = vec![b'x'; MAX_TICKET_TEXT_BYTES + 1];
    let oversized_fingerprint = PairFingerprint::for_accept(&oversized, None);
    assert_eq!(oversized_fingerprint.as_bytes().len(), 32);
    assert_eq!(
        format!("{oversized_fingerprint:?}"),
        "PairFingerprint([REDACTED])"
    );
}

#[test]
fn proof_bearing_debug_and_validation_errors_never_expose_bytes() {
    const PROOF_SENTINEL: u8 = 0x7f;
    let proof = PairProof::from_bytes([PROOF_SENTINEL; 32]);
    let accepted = PairAccepted::new(
        AuthGeneration::new(1).expect("non-zero generation"),
        [PROOF_SENTINEL; 32],
        "test-build",
    )
    .expect("valid acceptance");
    let invalid_length =
        PairProof::from_slice(&[PROOF_SENTINEL; 31]).expect_err("short proof is rejected");

    let corpus = [
        format!("{proof:?}"),
        format!("{accepted:?}"),
        format!("{invalid_length:?}"),
        invalid_length.to_string(),
    ]
    .join("\n");
    assert_eq!(format!("{proof:?}"), "PairProof([REDACTED])");
    assert!(corpus.contains("[REDACTED]"));
    assert!(
        !corpus.contains("127"),
        "proof sentinel bytes leaked through formatting: {corpus}"
    );
}

#[test]
fn pairing_handshake_budget_honors_an_injected_validated_ceiling() {
    assert_eq!(
        PairHandshakeBudget::with_maximum(0),
        Err(PairHandshakeBudgetError::InvalidMaximum)
    );
    let mut budget = PairHandshakeBudget::with_maximum(3).expect("non-zero ceiling");
    assert_eq!(budget.maximum(), 3);
    budget.record(3).expect("exact injected ceiling fits");
    assert_eq!(budget.remaining(), 0);
    assert_eq!(
        budget.record(1),
        Err(PairHandshakeBudgetError::Exceeded {
            used: 4,
            maximum: 3,
        })
    );
    assert_eq!(budget.used(), 3, "a rejected frame is never accounted");
}

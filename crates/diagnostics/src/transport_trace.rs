//! Adapter for the locally maintained, numeric-only Iroh timing vocabulary.

use std::{fmt::Write, time::Instant};
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::registry::LookupSpan;

use crate::{Event, Kind, Level, Recorder, TransportFailure, TransportPhase};

/// Gate only the dedicated target before any packet hashing. Other subscribers
/// keep their own filters. `sometimes` revisits the finite detail window even
/// after a callsite was first observed while detail was disabled.
pub fn transport_trace_layer<S>(recorder: Recorder) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a> + 'static,
{
    // Never make unrelated dependency callsites active merely by installing
    // diagnostics. Dynamic interest still reaches our finite-window gate.
    TransportTraceLayer(recorder).with_filter(
        tracing_subscriber::filter::dynamic_filter_fn(|metadata, _| {
            metadata.target() == "iroh::voytty_transport" || relay_dial_target(metadata.target())
        })
        .with_callsite_filter(|metadata| {
            if metadata.target() == "iroh::voytty_transport" || relay_dial_target(metadata.target())
            {
                tracing::subscriber::Interest::sometimes()
            } else {
                tracing::subscriber::Interest::never()
            }
        }),
    )
}

struct TransportTraceLayer(Recorder);
fn relay_dial_target(target: &str) -> bool {
    matches!(
        target,
        "iroh_relay::client::tls" | "iroh_relay::client::conn"
    )
}

struct DialContext {
    relay: u64,
    elapsed_ms: u64,
    entered: Instant,
}

impl<S: tracing::Subscriber + for<'a> LookupSpan<'a>> tracing_subscriber::Layer<S>
    for TransportTraceLayer
{
    fn register_callsite(
        &self,
        metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        if metadata.target() == "iroh::voytty_transport" || relay_dial_target(metadata.target()) {
            tracing::subscriber::Interest::sometimes()
        } else {
            tracing::subscriber::Interest::always()
        }
    }
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        !(metadata.target() == "iroh::voytty_transport" || relay_dial_target(metadata.target()))
            || self.0.detail_enabled()
    }
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attributes.metadata().target() != "iroh::voytty_transport"
            || attributes.metadata().name() != "relay_dial"
            || !self.0.detail_enabled()
        {
            return;
        }
        let mut fields = TraceFields::default();
        attributes.record(&mut fields);
        if let (Some(relay), Some(elapsed_ms), Some(span)) =
            (fields.relay, fields.elapsed_ms, context.span(id))
        {
            span.extensions_mut().insert(DialContext {
                relay,
                elapsed_ms,
                entered: Instant::now(),
            });
        }
    }
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        record_transport_trace(event, &self.0);
        if !relay_dial_target(event.metadata().target()) || !self.0.detail_enabled() {
            return;
        }
        let Some(scope) = context.event_scope(event) else {
            return;
        };
        for span in scope {
            let extensions = span.extensions();
            let Some(dial) = extensions.get::<DialContext>() else {
                continue;
            };
            let mut message = DialMessage {
                bytes: [0; 64],
                len: 0,
                valid: true,
            };
            event.record(&mut message);
            let Some(phase) = message.phase(event.metadata().target()) else {
                return;
            };
            let mut record = Event::new(Kind::TransportTrace).level(Level::Debug);
            record.fields.transport_phase = Some(phase);
            record.fields.transport_relay_tag = Some(dial.relay);
            record.fields.transport_elapsed_ms = Some(dial.elapsed_ms.saturating_add(
                dial.entered.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            ));
            self.0.record(record);
            return;
        }
    }
}

// Pinned iroh-relay 1.3.0 emits these literal messages. Match only this closed
// vocabulary inside our anonymous dial span. Never format its address/error
// fields or forward the message. A dependency upgrade must check these markers.
struct DialMessage {
    bytes: [u8; 64],
    len: usize,
    valid: bool,
}
impl Write for DialMessage {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let Some(out) = self
            .bytes
            .get_mut(self.len..self.len.saturating_add(value.len()))
        else {
            self.valid = false;
            return Err(std::fmt::Error);
        };
        out.copy_from_slice(value.as_bytes());
        self.len += value.len();
        Ok(())
    }
}
impl Visit for DialMessage {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && write!(self, "{value:?}").is_err() {
            self.valid = false;
        }
    }
}
impl DialMessage {
    fn phase(&self, target: &str) -> Option<TransportPhase> {
        if !self.valid {
            return None;
        }
        Some(match (target, &self.bytes[..self.len]) {
            ("iroh_relay::client::tls", b"connecting TCP stream") => {
                TransportPhase::RelayTcpConnecting
            }
            ("iroh_relay::client::tls", b"TCP stream connected") => TransportPhase::RelayTcpReady,
            ("iroh_relay::client::tls", b"Starting TLS handshake") => {
                TransportPhase::RelayTlsStarted
            }
            ("iroh_relay::client::tls", b"tls_connector connect success") => {
                TransportPhase::RelayTlsReady
            }
            ("iroh_relay::client::conn", b"server_handshake: started") => {
                TransportPhase::RelayUpgradeReady
            }
            ("iroh_relay::client::conn", b"server_handshake: done") => {
                TransportPhase::RelayAuthReady
            }
            _ => return None,
        })
    }
}

/// Admit only the dedicated metadata event, and only in a finite detail window.
/// Ordinary dependency events, messages, addresses and Debug values are ignored.
pub fn record_transport_trace(event: &tracing::Event<'_>, recorder: &Recorder) {
    if event.metadata().target() != "iroh::voytty_transport" || !recorder.detail_enabled() {
        return;
    }
    let mut fields = TraceFields::default();
    event.record(&mut fields);
    let Some(phase) = fields.phase.and_then(TransportPhase::from_code) else {
        return;
    };
    let mut record = Event::new(Kind::TransportTrace).level(Level::Debug);
    record.fields.transport_phase = Some(phase);
    record.fields.transport_failure = fields.failure.and_then(TransportFailure::from_code);
    record.fields.transport_relay_tag = fields.relay;
    record.fields.transport_peer_tag = fields.peer.filter(|v| *v != 0);
    record.fields.transport_packet_tag = fields.packet.filter(|v| *v != 0);
    record.fields.transport_packet_kind = fields.kind.filter(|v| *v <= 5);
    record.fields.transport_queue_ms = fields.age_ms;
    record.fields.transport_elapsed_ms = fields.elapsed_ms;
    record.fields.bytes = fields.bytes;
    recorder.record(record);
}

#[derive(Default)]
struct TraceFields {
    phase: Option<u64>,
    failure: Option<u64>,
    relay: Option<u64>,
    peer: Option<u64>,
    packet: Option<u64>,
    kind: Option<u64>,
    bytes: Option<u64>,
    age_ms: Option<u64>,
    elapsed_ms: Option<u64>,
}
impl Visit for TraceFields {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "phase" => self.phase = Some(value),
            "failure" => self.failure = Some(value),
            "relay" => self.relay = Some(value),
            "peer" => self.peer = Some(value),
            "packet" => self.packet = Some(value),
            "kind" => self.kind = Some(value),
            "bytes" => self.bytes = Some(value),
            "age_ms" => self.age_ms = Some(value),
            "elapsed_ms" => self.elapsed_ms = Some(value),
            _ => {}
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

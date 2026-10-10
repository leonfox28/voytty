//! Outgoing packet gate for endpoint-restart tests using ephemeral identities.

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use iroh_base::CustomAddr;

use super::test_transport::TestTransport;
use crate::socket::transports::{
    RecvInfo, Transmit,
    custom::{CustomEndpoint, CustomSender, CustomTransport},
};

/// Suppresses close notifications while a test endpoint is being shut down.
#[derive(Debug)]
pub struct ResetTransport {
    inner: Arc<TestTransport>,
    gate: Arc<Gate>,
}

#[derive(Debug, Default)]
struct Gate {
    blocked: AtomicBool,
    dropped: AtomicUsize,
}

impl ResetTransport {
    /// Wraps an in-memory transport; no packets are dropped initially.
    pub fn new(inner: Arc<TestTransport>) -> Self {
        Self {
            inner,
            gate: Arc::new(Gate::default()),
        }
    }

    /// Changes only this transport's outgoing delivery; receive stays available.
    pub fn block_sends(&self, blocked: bool) {
        self.gate.blocked.store(blocked, Ordering::SeqCst);
    }

    /// Number of outgoing transmissions suppressed by the gate.
    pub fn dropped(&self) -> usize {
        self.gate.dropped.load(Ordering::SeqCst)
    }
}

impl CustomTransport for ResetTransport {
    fn bind(&self) -> io::Result<Box<dyn CustomEndpoint>> {
        Ok(Box::new(GatedEndpoint {
            inner: self.inner.bind()?,
            gate: self.gate.clone(),
        }))
    }
}

#[derive(Debug)]
struct GatedEndpoint {
    inner: Box<dyn CustomEndpoint>,
    gate: Arc<Gate>,
}

impl CustomEndpoint for GatedEndpoint {
    fn watch_local_addrs(&self) -> n0_watcher::Direct<Vec<CustomAddr>> {
        self.inner.watch_local_addrs()
    }

    fn create_sender(&self) -> Arc<dyn CustomSender> {
        Arc::new(GatedSender {
            inner: self.inner.create_sender(),
            gate: self.gate.clone(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        metas: &mut [noq_udp::RecvMeta],
        infos: &mut [RecvInfo],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, metas, infos)
    }
}

#[derive(Debug)]
struct GatedSender {
    inner: Arc<dyn CustomSender>,
    gate: Arc<Gate>,
}

impl CustomSender for GatedSender {
    fn is_valid_send_addr(&self, addr: &CustomAddr) -> bool {
        self.inner.is_valid_send_addr(addr)
    }

    fn poll_send(
        &self,
        cx: &mut Context,
        dst: &CustomAddr,
        src: Option<&CustomAddr>,
        transmit: &Transmit<'_>,
    ) -> Poll<io::Result<()>> {
        if self.gate.blocked.load(Ordering::SeqCst) {
            self.gate.dropped.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        } else {
            self.inner.poll_send(cx, dst, src, transmit)
        }
    }
}

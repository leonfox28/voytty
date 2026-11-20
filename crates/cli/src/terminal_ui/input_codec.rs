//! Incremental decoding of local terminal input across reads and input epochs.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum HostInputEvent {
    Bytes(Vec<u8>),
    Opaque(Vec<u8>),
    FocusReport(Vec<u8>),
    ForwardedBytes(Vec<u8>),
    LegacyCtrlC,
    EnhancedKey(EnhancedKey),
    Paste(Vec<u8>),
    PageUp,
    PageDown,
    Mouse(SgrMouse),
    TerminalReply { reply: HostReply, bytes: usize },
}

pub(super) struct HostInputCodec {
    pub(super) pending: Vec<u8>,
    in_paste: bool,
    discarding_osc: bool,
    osc_utf8_tail: u8,
    discarding_csi: bool,
    utf8_tail: u8,
    pending_epoch: Option<u64>,
    pending_geometry: Option<u64>,
    escape_deadline: Option<Instant>,
}

impl HostInputCodec {
    pub(super) const fn new() -> Self {
        Self {
            pending: Vec::new(),
            in_paste: false,
            discarding_osc: false,
            osc_utf8_tail: 0,
            discarding_csi: false,
            utf8_tail: 0,
            pending_epoch: None,
            pending_geometry: None,
            escape_deadline: None,
        }
    }

    pub(super) async fn read(
        &mut self,
        input: impl Future<Output = Option<StdinEvent>>,
        current: u64,
        current_geometry: u64,
    ) -> Result<Option<Vec<HostInputEvent>>, CliError> {
        let input = if let Some(deadline) = self.escape_deadline {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline.into()) => {
                    return Ok(Some(self.take_escape(current)));
                }
                input = input => input,
            }
        } else {
            input.await
        };
        match input {
            Some(StdinEvent::Bytes {
                epoch,
                geometry,
                bytes,
            }) => self
                .feed_for_lifetimes(&bytes, epoch, current, geometry, current_geometry)
                .map(Some),
            Some(StdinEvent::Error(detail)) => {
                Err(CliError::Io(format!("read terminal stdin: {detail}")))
            }
            Some(StdinEvent::Eof) | None if self.escape_deadline.is_some() => {
                Ok(Some(self.take_escape(current)))
            }
            Some(StdinEvent::Eof) | None => Ok(None),
        }
    }

    fn take_escape(&mut self, current: u64) -> Vec<HostInputEvent> {
        self.escape_deadline = None;
        self.pending_geometry = None;
        let bytes = std::mem::take(&mut self.pending);
        if self.pending_epoch.take() == Some(current) {
            vec![HostInputEvent::Bytes(bytes)]
        } else {
            Vec::new()
        }
    }

    #[cfg(test)]
    pub(super) fn feed_for_epoch(
        &mut self,
        bytes: &[u8],
        epoch: u64,
        current: u64,
    ) -> Result<Vec<HostInputEvent>, CliError> {
        self.feed_for_lifetimes(bytes, epoch, current, 0, 0)
    }

    pub(super) fn feed_for_lifetimes(
        &mut self,
        bytes: &[u8],
        epoch: u64,
        current: u64,
        geometry: u64,
        current_geometry: u64,
    ) -> Result<Vec<HostInputEvent>, CliError> {
        // A fresh key cannot inherit a stale Escape. Keep an OSC/CSI
        // continuation framed so terminal replies still cross epoch fences.
        let stale_key =
            self.pending_epoch != Some(epoch) && !matches!(bytes.first(), Some(b']' | b'['));
        let mut events =
            if self.escape_deadline.is_some() && (stale_key || bytes.starts_with(b"\x1b")) {
                self.take_escape(current)
            } else {
                Vec::new()
            };
        let source_geometry = self.pending_geometry.unwrap_or(geometry);
        let source_epoch = self.pending_epoch.unwrap_or(epoch);
        let decoded = self.feed(bytes)?;
        self.pending_epoch = (!self.pending.is_empty()
            || self.utf8_tail != 0
            || self.discarding_osc
            || self.discarding_csi)
            .then_some(source_epoch);
        self.pending_geometry = self.pending_epoch.map(|_| source_geometry);
        self.escape_deadline = (self.pending == b"\x1b" && !self.discarding_osc)
            .then(|| Instant::now() + Duration::from_millis(40));
        events.extend(
            decoded
                .into_iter()
                .filter(|event| {
                    !matches!(event, HostInputEvent::Mouse(_))
                        || (geometry == current_geometry && source_geometry == current_geometry)
                })
                .filter(|event| {
                    matches!(event, HostInputEvent::TerminalReply { .. })
                        || (epoch == current && source_epoch == current)
                }),
        );
        Ok(events)
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<HostInputEvent>, CliError> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        loop {
            if self.pending.is_empty() {
                break;
            }
            if self.in_paste {
                if let Some(index) = find_bytes(&self.pending, PASTE_END) {
                    let end = index + PASTE_END.len();
                    if end > RESUME_INPUT_BOUND {
                        return Err(paste_input_overflow());
                    }
                    events.push(HostInputEvent::Paste(self.pending.drain(..end).collect()));
                    self.in_paste = false;
                    continue;
                }
                if self.pending.len() > RESUME_INPUT_BOUND {
                    return Err(paste_input_overflow());
                }
                break;
            }
            if !self.discarding_osc && !self.discarding_csi && self.pending.starts_with(PASTE_START)
            {
                self.in_paste = true;
                continue;
            }
            if self.discarding_csi {
                let end = self
                    .pending
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte));
                let count = end.map_or(self.pending.len(), |index| index + 1);
                self.pending.drain(..count);
                self.discarding_csi = end.is_none();
                events.push(HostInputEvent::TerminalReply {
                    reply: HostReply::Ignored,
                    bytes: count,
                });
                continue;
            }
            // C1 introducers are controls only outside UTF-8 continuation bytes.
            if self.utf8_tail != 0 {
                let byte = self.pending[0];
                if byte & 0xc0 == 0x80 {
                    self.utf8_tail -= 1;
                    push_host_bytes(&mut events, vec![self.pending.remove(0)]);
                    continue;
                }
                self.utf8_tail = 0;
            }
            let osc_prefix = if self.pending.starts_with(b"\x1b]") {
                2
            } else if self.pending[0] == 0x9d {
                1
            } else {
                0
            };
            if osc_prefix != 0 || self.discarding_osc {
                let start = if self.discarding_osc { 0 } else { osc_prefix };
                let mut tail = if self.discarding_osc {
                    self.osc_utf8_tail
                } else {
                    0
                };
                let mut end = None;
                for index in start..self.pending.len() {
                    let byte = self.pending[index];
                    if tail != 0 && byte & 0xc0 == 0x80 {
                        tail -= 1;
                        continue;
                    }
                    tail = match byte {
                        0xc2..=0xdf => 1,
                        0xe0..=0xef => 2,
                        0xf0..=0xf4 => 3,
                        _ => 0,
                    };
                    if matches!(byte, 7 | 0x9c | 0x18 | 0x1a) {
                        end = Some((index, 1));
                        break;
                    }
                    if byte == 0x1b && self.pending.get(index + 1) == Some(&b'\\') {
                        end = Some((index, 2));
                        break;
                    }
                }
                if let Some((end, length)) = end {
                    let count = end + length;
                    let replies = if self.discarding_osc
                        || count > 1024
                        || matches!(self.pending[end], 0x18 | 0x1a)
                    {
                        vec![HostReply::Ignored]
                    } else {
                        host_colors::osc_replies(&self.pending[start..end])
                    };
                    for (index, reply) in replies.into_iter().enumerate() {
                        events.push(HostInputEvent::TerminalReply {
                            reply,
                            bytes: if index == 0 { count } else { 0 },
                        });
                    }
                    self.pending.drain(..count);
                    self.discarding_osc = false;
                    self.osc_utf8_tail = 0;
                    continue;
                }
                if self.pending.len() > 1024 {
                    let keep = usize::from(self.pending.last() == Some(&0x1b));
                    let count = self.pending.len() - keep;
                    self.pending.drain(..count);
                    self.discarding_osc = true;
                    self.osc_utf8_tail = tail;
                    events.push(HostInputEvent::TerminalReply {
                        reply: HostReply::Ignored,
                        bytes: count,
                    });
                }
                break;
            }
            if self.pending.starts_with(PAGE_UP) {
                self.pending.drain(..PAGE_UP.len());
                events.push(HostInputEvent::PageUp);
                continue;
            }
            if self.pending.starts_with(PAGE_DOWN) {
                self.pending.drain(..PAGE_DOWN.len());
                events.push(HostInputEvent::PageDown);
                continue;
            }
            if self.pending.starts_with(b"\x1b[<") {
                if let Some(end) = self
                    .pending
                    .iter()
                    .position(|byte| matches!(byte, b'M' | b'm'))
                {
                    let length = end + 1;
                    let raw: Vec<u8> = self.pending.drain(..length).collect();
                    if let Some(mouse) = SgrMouse::parse(raw.clone()) {
                        events.push(HostInputEvent::Mouse(mouse));
                    } else {
                        events.push(HostInputEvent::Opaque(raw));
                    }
                    continue;
                }
                if self.pending.len() < HOST_SEQUENCE_BOUND {
                    break;
                }
            }
            if self.pending.starts_with(b"\x1b[")
                || self.pending.starts_with(b"\x1bO")
                || self.pending[0] == 0x9b
            {
                let prefix_len = if self.pending[0] == 0x9b { 1 } else { 2 };
                if let Some(end) = self
                    .pending
                    .iter()
                    .enumerate()
                    .skip(prefix_len)
                    .find_map(|(index, byte)| (0x40..=0x7e).contains(byte).then_some(index))
                {
                    let raw: Vec<u8> = self.pending.drain(..=end).collect();
                    if let Some(reply) = host_colors::csi_reply(&raw) {
                        events.push(HostInputEvent::TerminalReply {
                            reply,
                            bytes: raw.len(),
                        });
                    } else if matches!(raw.as_slice(), b"\x1b[I" | b"\x1b[O" | b"\x9bI" | b"\x9bO")
                    {
                        events.push(HostInputEvent::FocusReport(raw));
                    } else if let Some(key) = EnhancedKey::parse(raw.clone()) {
                        events.push(HostInputEvent::EnhancedKey(key));
                    } else {
                        events.push(HostInputEvent::Opaque(raw));
                    }
                    continue;
                }
                if self.pending.len() < HOST_SEQUENCE_BOUND {
                    break;
                }
                if host_colors::is_color_csi(&self.pending) {
                    let count = self.pending.len();
                    self.pending.clear();
                    self.discarding_csi = true;
                    events.push(HostInputEvent::TerminalReply {
                        reply: HostReply::Ignored,
                        bytes: count,
                    });
                    break;
                }
            }
            if self.pending.starts_with(b"\x1b\x1b") {
                events.push(HostInputEvent::Bytes(vec![self.pending.remove(0)]));
                continue;
            }
            if known_host_prefix(&self.pending) {
                break;
            }
            // Keep legacy Alt and SS3 keys opaque as one input unit. In
            // command mode, cancelling such a key must not leak its suffix
            // or mistake an embedded control byte for a new prefix.
            if self.pending[0] == 0x1b && self.pending.get(1) != Some(&b'[') {
                let second = self.pending[1];
                let length = match second {
                    b'O' | 0xc2..=0xdf => 3,
                    0xe0..=0xef => 4,
                    0xf0..=0xf4 => 5,
                    _ => 2,
                };
                if self.pending.len() < length {
                    break;
                }
                events.push(HostInputEvent::Opaque(
                    self.pending.drain(..length).collect(),
                ));
                continue;
            }
            self.utf8_tail = match self.pending[0] {
                0xc2..=0xdf => 1,
                0xe0..=0xef => 2,
                0xf0..=0xf4 => 3,
                _ => 0,
            };
            push_host_bytes(&mut events, vec![self.pending.remove(0)]);
        }
        Ok(events)
    }

    pub(super) const fn paste_in_progress(&self) -> bool {
        self.in_paste
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SgrMouse {
    pub(super) code: u16,
    pub(super) column: u16,
    pub(super) row: u16,
    pub(super) release: bool,
    pub(super) raw: Vec<u8>,
}

impl SgrMouse {
    pub(super) fn parse(raw: Vec<u8>) -> Option<Self> {
        let release = *raw.last()? == b'm';
        let body = raw
            .strip_prefix(b"\x1b[<")?
            .get(..raw.len().checked_sub(4)?)?;
        let mut fields = body.split(|byte| *byte == b';');
        let code = parse_decimal(fields.next()?)?;
        let column = parse_decimal(fields.next()?)?;
        let row = parse_decimal(fields.next()?)?;
        if fields.next().is_some() || column == 0 || row == 0 {
            return None;
        }
        Some(Self {
            code,
            column,
            row,
            release,
            raw,
        })
    }

    pub(super) const fn is_wheel(&self) -> bool {
        self.code & 64 != 0
    }

    pub(super) const fn wheel_is_up(&self) -> bool {
        self.code & 1 == 0
    }

    pub(super) const fn is_motion(&self) -> bool {
        self.code & 32 != 0
    }

    pub(super) const fn is_left_press(&self) -> bool {
        !self.release && !self.is_motion() && self.code & 3 == 0
    }

    pub(super) const fn is_unmodified(&self) -> bool {
        self.code & (4 | 8 | 16) == 0
    }

    pub(super) fn content_point(&self, viewport: TerminalSize) -> Option<TerminalTextPoint> {
        if self.row == 0
            || self.column == 0
            || self.row > viewport.rows
            || self.column > viewport.columns
        {
            return None;
        }
        Some(TerminalTextPoint::new(self.row - 1, self.column - 1))
    }
}

pub(super) fn parse_decimal(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0_u16;
    for byte in bytes {
        value = value
            .checked_mul(10)?
            .checked_add(u16::from(*byte - b'0'))?;
    }
    Some(value)
}

fn known_host_prefix(bytes: &[u8]) -> bool {
    [PAGE_UP, PAGE_DOWN, PASTE_START]
        .into_iter()
        .any(|sequence| sequence.starts_with(bytes))
        || b"\x1b[<".starts_with(bytes)
}

pub(super) fn find_bytes(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(super) fn push_host_bytes(events: &mut Vec<HostInputEvent>, bytes: Vec<u8>) {
    for (index, span) in bytes.split(|byte| *byte == 0x03).enumerate() {
        if index > 0 {
            events.push(HostInputEvent::LegacyCtrlC);
        }
        if span.is_empty() {
            continue;
        }
        push_raw_host_bytes(events, span.to_vec());
    }
}

fn push_raw_host_bytes(events: &mut Vec<HostInputEvent>, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    if let Some(HostInputEvent::Bytes(previous)) = events.last_mut() {
        previous.extend_from_slice(&bytes);
    } else {
        events.push(HostInputEvent::Bytes(bytes));
    }
}

fn paste_input_overflow() -> CliError {
    terminal_daemon_error(
        DomainErrorKind::ResourceExhausted,
        "bracketed terminal paste exceeded its fixed input bound",
    )
}

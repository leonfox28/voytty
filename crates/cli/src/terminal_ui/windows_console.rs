// Windows console IO around the shared semantic compositor and input epochs.
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind as ConsoleKeyKind, KeyModifiers, MouseButton, MouseEventKind,
};

fn terminal_size(_: &impl AsFd) -> Result<TerminalSize, CliError> {
    let (columns, rows) =
        crossterm::terminal::size().map_err(|e| terminal_io("read console viewport", e))?;
    if rows == 0 || columns == 0 {
        return Err(CliError::Usage("console viewport must be non-zero".into()));
    }
    Ok(TerminalSize::new(rows, columns))
}
fn write_all_fd(fd: &impl AsFd, bytes: &[u8]) -> io::Result<()> {
    let mut file = std::fs::File::from(fd.as_handle().try_clone_to_owned()?);
    file.write_all(bytes)
}

struct TerminalGuard {
    output: winsafe::HSTD,
    original_output: winsafe::co::CONSOLE,
    restored: bool,
    appearance_owned: Arc<std::sync::atomic::AtomicBool>,
}
impl TerminalGuard {
    fn enter(_: &impl AsFd, _: &impl AsFd) -> Result<Self, CliError> {
        use winsafe::{HSTD, co};
        // GetStdHandle borrows the process's standard handle; do not close it.
        let output = HSTD::GetStdHandle(co::STD_HANDLE::OUTPUT)
            .map_err(|e| terminal_io("open console output", io::Error::other(e)))?
            .leak();
        let original_output = output
            .GetConsoleMode()
            .map_err(|e| terminal_io("read console mode", io::Error::other(e)))?;
        crossterm::terminal::enable_raw_mode()
            .map_err(|e| terminal_io("enable console raw mode", e))?;
        let mut guard = Self {
            output,
            original_output,
            restored: false,
            appearance_owned: Arc::default(),
        };
        let entered = guard
            .output
            .SetConsoleMode(original_output | co::CONSOLE::ENABLE_VIRTUAL_TERMINAL_PROCESSING)
            .map_err(io::Error::other)
            .and_then(|()| io::stdout().write_all(ENTER_TERMINAL_UI))
            .and_then(|()| crossterm::execute!(io::stdout(), event::EnableMouseCapture));
        if let Err(error) = entered {
            let _ = guard.restore();
            return Err(terminal_io("enter console UI", error));
        }
        Ok(guard)
    }
    fn restore(&mut self) -> Result<(), CliError> {
        if self.restored {
            return Ok(());
        }
        let display = (|| {
            crossterm::execute!(io::stdout(), event::DisableMouseCapture)?;
            if self.appearance_owned.load(Ordering::Acquire) {
                io::stdout().write_all(b"\x1b[?2031l")?;
            }
            io::stdout().write_all(RESTORE_TERMINAL_UI)?;
            io::stdout().flush()
        })();
        let input = crossterm::terminal::disable_raw_mode();
        let output = self
            .output
            .SetConsoleMode(self.original_output)
            .map_err(io::Error::other);
        self.restored = display.is_ok() && input.is_ok() && output.is_ok();
        display
            .and(input)
            .and(output)
            .map_err(|e| terminal_io("restore console", e))
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

enum Signal {
    Resize(tokio::time::Interval),
    Interrupt(tokio::signal::windows::CtrlC),
    Break(tokio::signal::windows::CtrlBreak),
    Close(tokio::signal::windows::CtrlClose),
}
impl Signal {
    async fn recv(&mut self) -> Option<()> {
        match self {
            Self::Resize(timer) => {
                timer.tick().await;
                Some(())
            }
            Self::Interrupt(signal) => signal.recv().await,
            Self::Break(signal) => signal.recv().await,
            Self::Close(signal) => signal.recv().await,
        }
    }
}
impl TerminalSignals {
    fn install() -> Result<Self, CliError> {
        Ok(Self {
            resize: Signal::Resize(tokio::time::interval(Duration::from_millis(100))),
            interrupt: Signal::Interrupt(
                tokio::signal::windows::ctrl_c()
                    .map_err(|e| terminal_io("listen for console interrupt", e))?,
            ),
            terminate: Signal::Break(
                tokio::signal::windows::ctrl_break()
                    .map_err(|e| terminal_io("listen for console break", e))?,
            ),
            hangup: Signal::Close(
                tokio::signal::windows::ctrl_close()
                    .map_err(|e| terminal_io("listen for console close", e))?,
            ),
        })
    }
}

struct StdinPump {
    receiver: Option<mpsc::Receiver<StdinEvent>>,
    retained: VecDeque<StdinEvent>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<JoinHandle<Result<Option<StdinEvent>, String>>>,
}
impl StdinPump {
    fn start(_: &impl AsFd, epoch: InputEpoch) -> Result<Self, CliError> {
        let (sender, receiver) = mpsc::channel(STDIN_CHANNEL_CAPACITY);
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let handle = std::thread::Builder::new()
            .name("voytty-cli-console".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let generation = epoch.current();
                    let geometry = epoch.geometry();
                    let next = event::poll(Duration::from_millis(20)).and_then(|ready| {
                        if ready {
                            event::read().map(Some)
                        } else {
                            Ok(None)
                        }
                    });
                    let mut pending = match next {
                        Ok(None) => continue,
                        Ok(Some(event)) => {
                            let bytes = console_event_bytes(event);
                            if bytes.is_empty() {
                                continue;
                            }
                            StdinEvent::Bytes {
                                epoch: generation,
                                geometry,
                                bytes,
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                            StdinEvent::Eof
                        }
                        Err(error) => StdinEvent::Error(error.to_string()),
                    };
                    let terminal = !matches!(pending, StdinEvent::Bytes { .. });
                    loop {
                        if stop.load(Ordering::Acquire) {
                            return Ok(Some(pending));
                        }
                        match sender.try_send(pending) {
                            Ok(()) if terminal => return Ok(None),
                            Ok(()) => break,
                            Err(mpsc::error::TrySendError::Closed(event)) => {
                                return Ok(Some(event));
                            }
                            Err(mpsc::error::TrySendError::Full(event)) => pending = event,
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                Ok(None)
            })
            .map_err(|e| terminal_io("start console input", e))?;
        Ok(Self {
            receiver: Some(receiver),
            retained: VecDeque::new(),
            stopped,
            handle: Some(handle),
        })
    }
    async fn recv(&mut self) -> Option<StdinEvent> {
        if let Some(event) = self.retained.pop_front() {
            return Some(event);
        }
        self.receiver.as_mut()?.recv().await
    }
    fn replace_after_active_fence(
        &mut self,
        input: &impl AsFd,
        epoch: &InputEpoch,
        current: &mut u64,
        prefix: &mut CommandMode,
    ) -> Result<(), CliError> {
        let mut retained = self.stop_reader()?;
        let mut length = retained
            .iter()
            .map(|event| match event {
                StdinEvent::Bytes { bytes, .. } => bytes.len(),
                _ => 1,
            })
            .sum::<usize>();
        while event::poll(Duration::ZERO).map_err(|e| terminal_io("drain console input", e))? {
            let bytes = console_event_bytes(
                event::read().map_err(|e| terminal_io("read inactive console input", e))?,
            );
            length = length.saturating_add(bytes.len().max(1));
            if length > RESUME_INPUT_BOUND {
                return Err(terminal_daemon_error(
                    DomainErrorKind::ResourceExhausted,
                    "inactive console input exceeded bound",
                ));
            }
            if !bytes.is_empty() {
                retained.push_back(StdinEvent::Bytes {
                    epoch: epoch.current(),
                    geometry: epoch.geometry(),
                    bytes,
                });
            }
        }
        *current = epoch.advance();
        prefix.clear_pending();
        let mut replacement = Self::start(input, epoch.clone())?;
        replacement.retained = retained;
        *self = replacement;
        Ok(())
    }
    fn stop_reader(&mut self) -> Result<VecDeque<StdinEvent>, CliError> {
        self.stopped.store(true, Ordering::Release);
        let mut receiver = self.receiver.take();
        if let Some(receiver) = &mut receiver {
            receiver.close();
        }
        let tail = match self.handle.take() {
            Some(handle) => handle
                .join()
                .map_err(|_| CliError::Io("console input worker panicked".into()))?
                .map_err(CliError::Io)?,
            None => None,
        };
        let mut retained = std::mem::take(&mut self.retained);
        if let Some(receiver) = &mut receiver {
            while let Ok(event) = receiver.try_recv() {
                retained.push_back(event);
            }
        }
        retained.extend(tail);
        Ok(retained)
    }
    fn shutdown(&mut self) -> Result<(), CliError> {
        self.stop_reader().map(drop)
    }
}
impl Drop for StdinPump {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn console_event_bytes(event: Event) -> Vec<u8> {
    match event {
        Event::Key(key) => {
            // Feed typed CSI-u events to the shared codec so modifiers, key-up
            // and the child's negotiated keyboard modes keep one owner.
            let code = match key.code {
                KeyCode::Char(c) => u32::from(c),
                KeyCode::Enter => 13,
                KeyCode::Tab | KeyCode::BackTab => 9,
                KeyCode::Backspace => 127,
                KeyCode::Esc => 27,
                KeyCode::Insert => 57_348,
                KeyCode::Delete => 57_349,
                KeyCode::Left => 57_350,
                KeyCode::Right => 57_351,
                KeyCode::Up => 57_352,
                KeyCode::Down => 57_353,
                KeyCode::PageUp => 57_354,
                KeyCode::PageDown => 57_355,
                KeyCode::Home => 57_356,
                KeyCode::End => 57_357,
                KeyCode::F(n @ 1..=35) => 57_363 + u32::from(n),
                _ => return Vec::new(),
            };
            let modifiers = if key.code == KeyCode::BackTab {
                key.modifiers | KeyModifiers::SHIFT
            } else {
                key.modifiers
            };
            let kind = match key.kind {
                ConsoleKeyKind::Press => 1,
                ConsoleKeyKind::Repeat => 2,
                ConsoleKeyKind::Release => 3,
            };
            format!("\x1b[{code};{}:{kind}u", console_modifier(modifiers)).into_bytes()
        }
        Event::Paste(text) => [b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~"].concat(),
        Event::FocusGained => b"\x1b[I".to_vec(),
        Event::FocusLost => b"\x1b[O".to_vec(),
        Event::Mouse(mouse) => {
            let button = |button| match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
            };
            let (mut code, end) = match mouse.kind {
                MouseEventKind::Down(b) => (button(b), 'M'),
                MouseEventKind::Up(b) => (button(b), 'm'),
                MouseEventKind::Drag(b) => (button(b) + 32, 'M'),
                MouseEventKind::Moved => (35, 'M'),
                MouseEventKind::ScrollUp => (64, 'M'),
                MouseEventKind::ScrollDown => (65, 'M'),
                MouseEventKind::ScrollLeft => (66, 'M'),
                MouseEventKind::ScrollRight => (67, 'M'),
            };
            if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                code += 4;
            }
            if mouse.modifiers.contains(KeyModifiers::ALT) {
                code += 8;
            }
            if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                code += 16;
            }
            format!(
                "\x1b[<{code};{};{}{end}",
                u32::from(mouse.column) + 1,
                u32::from(mouse.row) + 1
            )
            .into_bytes()
        }
        _ => Vec::new(),
    }
}
fn console_modifier(modifiers: KeyModifiers) -> u8 {
    1 + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL))
        + 8 * u8::from(modifiers.contains(KeyModifiers::SUPER))
        + 16 * u8::from(modifiers.contains(KeyModifiers::HYPER))
        + 32 * u8::from(modifiers.contains(KeyModifiers::META))
}

#[cfg(test)]
mod windows_console_tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseEvent};

    #[test]
    fn console_keys_preserve_unicode_modifiers_and_release_for_the_shared_codec() {
        for (code, modifiers, expected) in [
            (KeyCode::Char('界'), KeyModifiers::NONE, "界".as_bytes()),
            (
                KeyCode::Char('b'),
                KeyModifiers::CONTROL,
                b"\x02".as_slice(),
            ),
            (KeyCode::Char('x'), KeyModifiers::ALT, b"\x1bx".as_slice()),
            (KeyCode::Up, KeyModifiers::CONTROL, b"\x1b[1;5A".as_slice()),
        ] {
            let bytes = console_event_bytes(Event::Key(KeyEvent::new(code, modifiers)));
            let parsed = EnhancedKey::parse(bytes).expect("shared typed key");
            assert_eq!(parsed.legacy_bytes(TerminalModes::default()), expected);
        }
        let released = console_event_bytes(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            ConsoleKeyKind::Release,
        )));
        let parsed = EnhancedKey::parse(released).expect("release key");
        assert_eq!(parsed.kind, KeyEventKind::Release);
        assert!(parsed.legacy_bytes(TerminalModes::default()).is_empty());
    }

    #[test]
    fn console_mouse_and_paste_reach_the_existing_input_framing() {
        let mouse = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 4,
            row: 7,
            modifiers: KeyModifiers::SHIFT,
        };
        assert_eq!(console_event_bytes(Event::Mouse(mouse)), b"\x1b[<4;5;8m");
        assert_eq!(
            console_event_bytes(Event::Paste("hi 世界".into())),
            "\x1b[200~hi 世界\x1b[201~".as_bytes()
        );
        assert_eq!(console_event_bytes(Event::FocusLost), b"\x1b[O");
    }
}

//! The Bluetooth controller of the WILC3000 (docs/bluetooth.md): its HCI UART, the control
//! node of the `wilc-spi` driver, and the thread that runs the `tetrad_ble` host over them.
//!
//! Measured on a unit (2026-10-28): HCI runs on `/dev/ttySC2` at 115200 8N1 with no flow
//! control; faster rates gave a hardware error. Read Local Version tells the controller's state:
//! manufacturer 0x0060 is the boot ROM, 0x0013 (Atmel) the BLE firmware. The driver loads the
//! firmware with `BT_POWER_UP`, `BT_DOWNLOAD_FW` and `BT_FW_CHIP_WAKEUP`, and a new load brings
//! back a controller that stopped answering. `BT_POWER_DOWN` while Wi-Fi runs left the core
//! silent until a reboot, so Tetrad never sends it.
//!
//! The UART sends with DMA, and its driver (`adi_uart4.c`, ADI kernel 4.0) can lose the end of a
//! transfer: its DMA interrupt does nothing when the channel still reads as running, and then no
//! transfer starts again. Under Tetrad's load this stopped the output on a unit (2026-10-28).
//! [`OutputWatch`] sees bytes that do not leave and restarts the output with `tcflow`, which
//! runs the driver's stop (it waits for the transmitter and ends the transfer) and start.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tetrad_ble::{Bond, Command, Config, Event, Host};

pub const HCI_UART: &str = "/dev/ttySC2";
/// The control node of the `wilc-spi` driver.
pub const CONTROL: &str = "/dev/wilc_bt";

/// Manufacturers in Read Local Version.
const ROM: u16 = 0x0060;
const FIRMWARE: u16 = 0x0013;

/// The HCI UART: raw, 115200 8N1, no flow control, non-blocking.
pub struct HciUart {
    file: File,
}

impl HciUart {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(path)?;
        let fd = file.as_raw_fd();
        let mut termios = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `termios` is a live, writable termios; tcgetattr fully initializes it on success.
        if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: tcgetattr returned 0, so every field is initialized.
        let mut termios = unsafe { termios.assume_init() };
        // SAFETY: cfmakeraw and the speed calls only write the exclusively borrowed struct.
        unsafe {
            libc::cfmakeraw(&mut termios);
            libc::cfsetospeed(&mut termios, libc::B115200);
            libc::cfsetispeed(&mut termios, libc::B115200);
        }
        termios.c_cflag |= libc::CLOCAL | libc::CREAD;
        termios.c_cflag &= !(libc::CRTSCTS | libc::CSTOPB | libc::PARENB);
        termios.c_cc[libc::VMIN] = 0;
        termios.c_cc[libc::VTIME] = 0;
        // SAFETY: `termios` is initialized and only read by tcsetattr; `file` keeps the descriptor open.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: tcflush takes the open descriptor and a constant.
        unsafe { libc::tcflush(fd, libc::TCIOFLUSH) };
        Ok(Self { file })
    }

    /// The bytes queued now; empty when none.
    pub fn read_available(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buf) {
            Ok(read) => Ok(read),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Bytes written and not yet sent (`TIOCOUTQ`).
    pub fn pending_output(&self) -> io::Result<usize> {
        let mut pending: libc::c_int = 0;
        // SAFETY: TIOCOUTQ writes one int through the pointer, which points to a live local.
        if unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCOUTQ, &mut pending) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(usize::try_from(pending).unwrap_or(0))
    }

    /// Stops and starts the output: the driver ends a lost transfer and sends the rest.
    pub fn restart_output(&self) -> io::Result<()> {
        let fd = self.file.as_raw_fd();
        // SAFETY: tcflow takes the open descriptor and a constant.
        let stopped = unsafe { libc::tcflow(fd, libc::TCOOFF) };
        // SAFETY: as above.
        let started = unsafe { libc::tcflow(fd, libc::TCOON) };
        if stopped != 0 || started != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Writes all of `bytes`, waiting while the driver's buffer is full.
    pub fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            match self.file.write(bytes) {
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    wait(
                        &[(self.file.as_raw_fd(), libc::POLLOUT)],
                        Some(Duration::from_millis(100)),
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Reads what arrives within `within`.
    fn read_for(&mut self, within: Duration, want: usize) -> io::Result<Vec<u8>> {
        let until = Instant::now() + within;
        let mut out = Vec::new();
        let mut buf = [0; 256];
        while out.len() < want {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            wait(&[(self.file.as_raw_fd(), libc::POLLIN)], Some(left))?;
            let read = self.read_available(&mut buf)?;
            out.extend_from_slice(&buf[..read]);
        }
        Ok(out)
    }
}

/// Watches the bytes that wait in the UART's output queue. When the count stays the same for
/// longer than they take to send (with margin), the output stopped: restart it.
#[derive(Debug, Default)]
pub struct OutputWatch {
    last: usize,
    since: Duration,
}

impl OutputWatch {
    /// One look at the queue at `now`. `true`: restart the output.
    pub fn stalled(&mut self, pending: usize, now: Duration) -> bool {
        if pending == 0 || pending != self.last {
            self.last = pending;
            self.since = now;
            return false;
        }
        // 87 µs per byte at 115200 8N1; twice that, and 30 ms.
        let allowed = Duration::from_micros(30_000 + pending as u64 * 174);
        if now.saturating_sub(self.since) > allowed {
            self.since = now;
            return true;
        }
        false
    }
}

/// Polls `fds` for their events; returns the events each one has. Waits forever for `None`.
fn wait(
    fds: &[(RawFd, libc::c_short)],
    timeout: Option<Duration>,
) -> io::Result<Vec<libc::c_short>> {
    let mut entries: Vec<libc::pollfd> = (fds.iter())
        .map(|&(fd, events)| libc::pollfd {
            fd,
            events,
            revents: 0,
        })
        .collect();
    let timeout = timeout.map_or(-1, |t| {
        libc::c_int::try_from(t.as_nanos().div_ceil(1_000_000)).unwrap_or(libc::c_int::MAX)
    });
    // SAFETY: `entries` is a valid, exclusively borrowed array of `entries.len()` pollfds whose
    // descriptors the caller keeps open for the call.
    let ready = unsafe { libc::poll(entries.as_mut_ptr(), entries.len() as libc::nfds_t, timeout) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(vec![0; fds.len()]);
        }
        return Err(error);
    }
    Ok(entries.iter().map(|entry| entry.revents).collect())
}

/// One command to the driver's control node.
fn control(command: &str) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(CONTROL)?
        .write_all(command.as_bytes())
}

/// The controller's state: who answers Read Local Version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Rom,
    Firmware,
    Silent,
}

fn state(uart: &mut HciUart) -> io::Result<State> {
    // HCI Reset, then Read Local Version; drop what came before.
    let mut buf = [0; 256];
    while uart.read_available(&mut buf)? > 0 {}
    uart.write_all(&tetrad_ble::hci::command(tetrad_ble::hci::op::RESET, &[]))?;
    let reset = uart.read_for(Duration::from_millis(500), 7)?;
    if reset.len() < 7 {
        return Ok(State::Silent);
    }
    let version = tetrad_ble::hci::op::READ_LOCAL_VERSION;
    uart.write_all(&tetrad_ble::hci::command(version, &[]))?;
    let reply = uart.read_for(Duration::from_millis(500), 15)?;
    // 04 0e 0c ncmd 01 10 status hci rev(2) lmp manufacturer(2) subversion(2)
    if reply.len() < 15 || reply[..2] != [0x04, 0x0e] {
        return Ok(State::Silent);
    }
    Ok(match u16::from_le_bytes([reply[11], reply[12]]) {
        ROM => State::Rom,
        FIRMWARE => State::Firmware,
        _ => State::Silent,
    })
}

/// Loads the BLE firmware when the controller needs it. Returns a UART with the firmware
/// answering at 115200.
pub fn bring_up(path: &Path) -> Result<HciUart, String> {
    if !Path::new(CONTROL).exists() {
        // Tetrad loads the driver when Wi-Fi starts; Bluetooth can come first.
        let output = std::process::Command::new("modprobe")
            .arg("wilc-spi")
            .output()
            .map_err(|error| format!("modprobe wilc-spi: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "modprobe wilc-spi: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let mut uart = HciUart::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let io = |error: io::Error| format!("Bluetooth controller: {error}");
    match state(&mut uart).map_err(io)? {
        State::Firmware => return Ok(uart),
        State::Rom => control("BT_POWER_UP").map_err(io)?,
        // A stopped controller: a new load starts it again.
        State::Silent => {}
    }
    control("BT_DOWNLOAD_FW").map_err(io)?;
    std::thread::sleep(Duration::from_millis(300));
    control("BT_FW_CHIP_WAKEUP").map_err(io)?;
    // Right after boot the firmware once needed more time to answer: look a few times.
    let mut last = State::Silent;
    for _ in 0..4 {
        last = state(&mut uart).map_err(io)?;
        if last == State::Firmware {
            return Ok(uart);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("the Bluetooth firmware did not start ({last:?})"))
}

/// The running host: a thread with the UART. Events arrive through [`Self::events`]; a
/// [`Event::Fault`] ends the thread.
pub struct Bluetooth {
    commands: Sender<Command>,
    wake: UnixStream,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Bluetooth {
    pub fn start(config: Config, bonds: Vec<Bond>) -> io::Result<Self> {
        let (commands, inbox) = mpsc::channel();
        let (outbox, events) = mpsc::channel();
        let (wake, woken) = UnixStream::pair()?;
        woken.set_nonblocking(true)?;
        let thread = std::thread::Builder::new()
            .name("bluetooth".into())
            .spawn(move || run(config, bonds, &inbox, &woken, &outbox))?;
        Ok(Self {
            commands,
            wake,
            events,
            thread: Some(thread),
        })
    }

    pub fn send(&mut self, command: Command) {
        if self.commands.send(command).is_ok() {
            let _ = self.wake.write(&[1]);
        }
    }

    /// The events so far.
    pub fn events(&mut self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for Bluetooth {
    /// Ends the thread: it resets the controller (no advertising, no links) and returns.
    fn drop(&mut self) {
        let _ = self.wake.shutdown(std::net::Shutdown::Both);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn random(out: &mut [u8]) {
    if File::open("/dev/urandom")
        .and_then(|mut urandom| urandom.read_exact(out))
        .is_err()
    {
        // No kernel random source: the pairing still works, with weaker keys.
        let seed = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
            .map_or(0, |t| t.as_nanos() as u64)
            ^ 0x9e37_79b9_7f4a_7c15;
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = (seed.rotate_left(i as u32 % 64) >> (i % 8)) as u8;
        }
    }
}

fn run(
    config: Config,
    bonds: Vec<Bond>,
    commands: &Receiver<Command>,
    mut woken: &UnixStream,
    events: &Sender<Event>,
) {
    let fault = |reason: String| {
        let _ = events.send(Event::Fault(reason));
    };
    let mut uart = match bring_up(Path::new(HCI_UART)) {
        Ok(uart) => uart,
        Err(reason) => return fault(reason),
    };
    let started = Instant::now();
    let mut host = Host::new(config, bonds, Box::new(random));
    let mut buf = [0; 512];
    let mut watch = OutputWatch::default();
    let mut restarts: u64 = 0;
    loop {
        let output = host.take_output();
        if !output.is_empty()
            && let Err(error) = uart.write_all(&output)
        {
            return fault(format!("{HCI_UART}: {error}"));
        }
        let mut dead = false;
        while let Some(event) = host.next_event() {
            dead |= matches!(event, Event::Fault(_));
            if events.send(event).is_err() {
                return;
            }
        }
        if dead {
            return;
        }
        let now = started.elapsed();
        let pending = match uart.pending_output() {
            Ok(pending) => pending,
            Err(error) => return fault(format!("{HCI_UART}: {error}")),
        };
        if watch.stalled(pending, now) {
            if let Err(error) = uart.restart_output() {
                return fault(format!("{HCI_UART}: {error}"));
            }
            restarts += 1;
            if restarts.is_power_of_two() {
                let text = format!("the UART output stopped; restarted ({restarts} times)");
                if events.send(Event::Notice(text)).is_err() {
                    return;
                }
            }
        }
        // While bytes wait, look again soon.
        let most = if pending > 0 {
            Duration::from_millis(10)
        } else {
            Duration::from_secs(1)
        };
        let timeout = (host.deadline())
            .map(|at| at.saturating_sub(now))
            .unwrap_or(most)
            .min(most);
        let ready = match wait(
            &[
                (uart.file.as_raw_fd(), libc::POLLIN),
                (woken.as_raw_fd(), libc::POLLIN),
            ],
            Some(timeout),
        ) {
            Ok(ready) => ready,
            Err(error) => return fault(format!("poll: {error}")),
        };
        let now = started.elapsed();
        if ready[0] & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return fault(format!("{HCI_UART} hung up"));
        }
        if ready[0] & libc::POLLIN != 0 {
            match uart.read_available(&mut buf) {
                // Readable with nothing to read: the line hung up.
                Ok(0) => return fault(format!("{HCI_UART} hung up")),
                Ok(read) => host.receive(&buf[..read], now),
                Err(error) => return fault(format!("{HCI_UART}: {error}")),
            }
        }
        if ready[1] != 0 {
            let mut drain = [0; 64];
            match woken.read(&mut drain) {
                // The device side closed: stop advertising and leave the links.
                Ok(0) => {
                    let reset = tetrad_ble::hci::command(tetrad_ble::hci::op::RESET, &[]);
                    let _ = uart.write_all(&reset);
                    return;
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        }
        loop {
            match commands.try_recv() {
                Ok(command) => host.apply(command),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        host.tick(now);
    }
}

#[cfg(test)]
mod tests {
    use super::OutputWatch;
    use std::time::Duration;

    #[test]
    fn output_that_does_not_drain_is_restarted_once_per_wait() {
        let ms = Duration::from_millis;
        let mut watch = OutputWatch::default();
        assert!(!watch.stalled(0, ms(0)));
        // 100 bytes take 8.7 ms; the watch allows 30 ms and twice that.
        assert!(!watch.stalled(100, ms(0)));
        assert!(!watch.stalled(100, ms(40)));
        assert!(watch.stalled(100, ms(50)));
        // The wait starts again after a restart.
        assert!(!watch.stalled(100, ms(60)));
        // Moving bytes are never a stall.
        assert!(!watch.stalled(60, ms(200)));
        assert!(!watch.stalled(0, ms(400)));
    }
}

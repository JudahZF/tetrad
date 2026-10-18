//! Stock `zenUI::USBHIDController` connection state (singleton `0x6e1330`).
//!
//! Three handshake flags (`+0x3fc` keep-alive, `+0x3fd` Version, `+0x3fe`
//! Connection message) decide `connected` (`+0x3d8`) in `0x6e24a8`. Any
//! received message other than KeepAlive renews keep-alive (`0x290ae8` calls
//! `0x6e28a8(1)`); the keep-alive timer (`+0x3dc`) clears it on expiry.

use std::time::Duration;

/// Connection transitions that the app must observe. Stock emits
/// `controlConnectionChanged(bool)` and `controlInternetConnectionChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Connected,
    /// Stock teardown `0x6e1d74` runs: the app cancels cloud bulk operations,
    /// drops meter and tuner subscriptions and resets receiver state.
    Disconnected,
    /// `KeepAliveMessage.is_online` changed (`0x6e0740`).
    Online(bool),
    /// The keep-alive timer expired (`0x6e294c`), or USB was unplugged
    /// (SOC2 ARM diagnostics byte 0 falling, `0x6e2b8c`).
    KeepAliveExpired,
    /// `0x29ad14`: drop both queued transmit deques and queue the transport
    /// reset marker (HID header byte 2 = 0xff) on the priority deque.
    FlushTransmit,
}

/// Keep-alive interval after a connection starts or ends (`0x6e1170`,
/// `0x6e2000`). A KeepAlive with a `-` CorOS build can change it.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("keep-alive timeout must round to 1..=2147483647 milliseconds")]
    Timeout,
    #[error("monotonic time moved backwards")]
    ClockRollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Handshake {
    pub keep_alive: bool,
    pub version: bool,
    pub connection: bool,
}

impl Handshake {
    fn complete(self) -> bool {
        self.keep_alive && self.version && self.connection
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    handshake: Handshake,
    connected: bool,
    online: bool,
    timeout: Duration,
    deadline: Option<Duration>,
    now: Duration,
    honor_wire_timeout: bool,
}

impl Session {
    /// `timeout` is the initial keep-alive interval; the stock value is
    /// `KEEP_ALIVE_INTERVAL`.
    pub fn new(start: Duration, timeout: Duration) -> Result<Self, SessionError> {
        if timeout.is_zero() || timeout.as_millis() > i32::MAX as u128 {
            return Err(SessionError::Timeout);
        }
        Ok(Self {
            handshake: Handshake::default(),
            connected: false,
            online: false,
            timeout,
            deadline: None,
            now: start,
            honor_wire_timeout: false,
        })
    }

    /// KeepAlive (`0x17c7f4`) applies Control's timeout only when the CorOS
    /// version string has a `-` suffix (`0x668bec`), so release builds keep
    /// their own interval.
    pub fn with_coros_version(mut self, coros_version: &str) -> Self {
        self.honor_wire_timeout = coros_version.contains('-');
        self
    }

    pub fn honors_wire_timeout(&self) -> bool {
        self.honor_wire_timeout
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn online(&self) -> bool {
        self.online
    }

    pub fn handshake(&self) -> Handshake {
        self.handshake
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn deadline(&self) -> Option<Duration> {
        self.deadline
    }

    pub(crate) fn advance(&mut self, now: Duration) -> Result<(), SessionError> {
        if now < self.now {
            return Err(SessionError::ClockRollback);
        }
        self.now = now;
        Ok(())
    }

    /// `0x6e28a8`: set the keep-alive flag; while it is set, restart the timer.
    pub(crate) fn renew(&mut self, events: &mut Vec<SessionEvent>) {
        self.set(|flags| flags.keep_alive = true, events);
        self.deadline = self.now.checked_add(self.timeout);
    }

    pub(crate) fn set_version(&mut self, accepted: bool, events: &mut Vec<SessionEvent>) {
        self.set(|flags| flags.version = accepted, events);
    }

    /// `0x6e291c`, from the Connection receiver `0xfe880`.
    pub(crate) fn set_connection(&mut self, connected: bool, events: &mut Vec<SessionEvent>) {
        self.set(|flags| flags.connection = connected, events);
    }

    /// `0x6e0740`: emit only on change.
    pub(crate) fn set_online(&mut self, online: bool, events: &mut Vec<SessionEvent>) {
        if self.online != online {
            self.online = online;
            events.push(SessionEvent::Online(online));
        }
    }

    /// `0x6e1680`: seconds from the wire, `roundf(seconds * 1000)` ms.
    pub(crate) fn set_timeout_seconds(&mut self, seconds: f32) -> Result<(), SessionError> {
        let millis = (seconds * 1000.0).round();
        if !(1.0..=i32::MAX as f32).contains(&millis) {
            return Err(SessionError::Timeout);
        }
        self.timeout = Duration::from_millis(millis as u64);
        if self.deadline.is_some() {
            self.deadline = self.now.checked_add(self.timeout);
        }
        Ok(())
    }

    /// `0x6e0474`: a Download suspension stops the keep-alive timer, so no
    /// expiry is due until `resume_keep_alive`.
    pub(crate) fn pause_keep_alive(&mut self) {
        self.deadline = None;
    }

    /// `0x6e048c`: restart the keep-alive timer, only while connected. Unlike
    /// `renew`, this does not set the keep-alive flag.
    pub(crate) fn resume_keep_alive(&mut self) {
        if self.connected {
            self.deadline = self.now.checked_add(self.timeout);
        }
    }

    /// Keep-alive timer service. Expiry clears the keep-alive flag.
    pub(crate) fn expire(&mut self, events: &mut Vec<SessionEvent>) {
        if self.deadline.is_some_and(|deadline| self.now >= deadline) {
            self.drop_keep_alive(events);
        }
    }

    /// `keepAliveTimerSlot` `0x6e294c`: log, then clear a set keep-alive flag.
    pub(crate) fn drop_keep_alive(&mut self, events: &mut Vec<SessionEvent>) {
        self.deadline = None;
        events.push(SessionEvent::KeepAliveExpired);
        if self.handshake.keep_alive {
            self.set(|flags| flags.keep_alive = false, events);
        }
    }

    /// `0x6e24a8` evaluation, then `0x6e1d74` teardown on a falling edge,
    /// which clears all three flags and the online state.
    fn set(&mut self, change: impl FnOnce(&mut Handshake), events: &mut Vec<SessionEvent>) {
        change(&mut self.handshake);
        let connected = self.handshake.complete();
        if connected == self.connected {
            return;
        }
        self.connected = connected;
        if connected {
            events.push(SessionEvent::Connected);
        } else {
            self.handshake = Handshake::default();
            self.deadline = None;
            self.timeout = KEEP_ALIVE_INTERVAL;
            events.push(SessionEvent::Disconnected);
            self.set_online(false, events);
        }
    }
}

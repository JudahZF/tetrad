//! Internet reachability probe and NTP time set: the ZenUI internet manager (constructor VA
//! 0x004af280), its TCP "ping" worker (0x004af778, 0x004b1cd4) and time worker (0x004b2a38,
//! 0x004b23e4, 0x004b271c).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use thiserror::Error;

use crate::exec::{Command, CommandError, CommandRunner, NetError, NetIo};

/// Probe targets, rotated after a failure (`_INIT_389` VA 0x00094b0c).
pub const PROBE_TARGETS: [Ipv4Addr; 2] = [Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(8, 8, 4, 4)];
/// The "ping" is a TCP connect to port 53, not ICMP or a DNS query (VA 0x004b1cd4).
pub const PROBE_PORT: u16 = 53;
/// Inferred: the manager stores 40000 at +0x0c and hands it to the TCP worker (VA 0x004b0158).
pub const STOCK_PROBE_TIMEOUT: Duration = Duration::from_millis(40_000);
/// Inferred: the manager timer restarts with +0x10 = 5000 (VA 0x004b036c).
pub const STOCK_RECHECK_INTERVAL: Duration = Duration::from_millis(5_000);

/// Host is `<n>.pool.ntp.org` with `n` in `0..NTP_POOL_COUNT` (VA 0x004b2a38).
pub const NTP_POOL_SUFFIX: &str = ".pool.ntp.org";
pub const NTP_POOL_COUNT: u8 = 4;
/// Inferred: the port is a parameter of 0x004b2a38; 123 is the NTP port.
pub const NTP_PORT: u16 = 123;
/// Inferred: Qt's default `waitForReadyRead` wait; the decompile lost the argument.
pub const STOCK_NTP_WAIT: Duration = Duration::from_millis(30_000);
/// After a failed set, stock sleeps this long, then reports error 99 (VA 0x004b271c).
pub const NTP_FAILURE_DELAY: Duration = Duration::from_millis(5_000);
pub const NTP_FAILURE_CODE: i32 = 99;
pub const NTP_PACKET_LEN: usize = 48;
/// Owned: how many IPv4 addresses of one pool host an NTP attempt tries.
pub const NTP_ADDRESSES: usize = 4;
/// Seconds from 1900-01-01 to 1970-01-01. Stock adds `0x7c558180`, its two's complement.
const NTP_UNIX_OFFSET: i64 = 2_208_988_800;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{target} unreachable: {cause}")]
pub struct ProbeError {
    pub target: SocketAddr,
    pub cause: NetError,
}

/// TCP reachability of one public resolver. Success says only that this target accepted a TCP
/// connection; it is not DNS, TLS or cloud reachability.
#[derive(Debug, Clone)]
pub struct InternetProbe {
    next: usize,
    pub timeout: Duration,
}

impl InternetProbe {
    pub fn new(timeout: Duration) -> Self {
        Self { next: 0, timeout }
    }

    pub fn target(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(PROBE_TARGETS[self.next]), PROBE_PORT)
    }

    /// One connect attempt. A failure moves to the other target, as stock does before it emits
    /// the disconnected result (VA 0x004b1034 region, counter `DAT_0154bef0`).
    pub fn probe(&mut self, net: &mut impl NetIo) -> Result<SocketAddr, ProbeError> {
        let result = connect(net, self.target(), self.timeout);
        self.record(&result);
        result
    }

    fn record(&mut self, result: &Result<SocketAddr, ProbeError>) {
        if result.is_err() {
            self.next = (self.next + 1) % PROBE_TARGETS.len();
        }
    }
}

fn connect(
    net: &mut impl NetIo,
    target: SocketAddr,
    timeout: Duration,
) -> Result<SocketAddr, ProbeError> {
    net.tcp_connect(target, timeout)
        .map(|()| target)
        .map_err(|cause| ProbeError { target, cause })
}

/// Stock client request: first word `0xec0600e3` (LI 3, version 4, client mode, poll 6,
/// precision 0xec), reference id `1N14`, all else zero (VA 0x004b2a38).
pub fn ntp_request() -> [u8; NTP_PACKET_LEN] {
    let mut packet = [0u8; NTP_PACKET_LEN];
    packet[..4].copy_from_slice(&[0xe3, 0x00, 0x06, 0xec]);
    packet[12..16].copy_from_slice(b"1N14");
    packet
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NtpError {
    #[error("NTP reply is {0} bytes, not 48")]
    Size(usize),
    #[error("NTP reply is not from a server (mode {0})")]
    Mode(u8),
    #[error("NTP server is unsynchronized")]
    Unsynchronized,
}

/// Unix seconds from the transmit timestamp (bytes 40..44, big-endian).
///
/// Stock checks only the size, a non-zero timestamp and a positive `i32` result, so it fails
/// after 2038. Owned policy adds: server mode 4, stratum not 0 (kiss-of-death), leap indicator
/// not 3, and era 1 for timestamps below the Unix epoch offset (valid until 2106).
pub fn parse_ntp_response(reply: &[u8]) -> Result<i64, NtpError> {
    let reply: &[u8; NTP_PACKET_LEN] = reply.try_into().map_err(|_| NtpError::Size(reply.len()))?;
    let mode = reply[0] & 0x07;
    if mode != 4 {
        return Err(NtpError::Mode(mode));
    }
    let seconds = i64::from(u32::from_be_bytes([
        reply[40], reply[41], reply[42], reply[43],
    ]));
    if reply[0] >> 6 == 3 || reply[1] == 0 || seconds == 0 {
        return Err(NtpError::Unsynchronized);
    }
    let era = if seconds < NTP_UNIX_OFFSET {
        1i64 << 32
    } else {
        0
    };
    Ok(seconds + era - NTP_UNIX_OFFSET)
}

/// System time was written. Stock writes no RTC (no `hwclock` or RTC ioctl at VA 0x004b23e4),
/// so the time is lost at power off until the next sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSet {
    pub unix_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimeError {
    #[error("{host}: {cause}")]
    Resolve { host: String, cause: NetError },
    #[error("NTP exchange failed: {0}")]
    Exchange(NetError),
    #[error(transparent)]
    Reply(#[from] NtpError),
    #[error("setting system time failed: {0}")]
    Set(String),
}

impl From<CommandError> for TimeError {
    fn from(e: CommandError) -> Self {
        Self::Set(e.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct TimeSync {
    next_pool: u8,
    pub wait: Duration,
}

impl TimeSync {
    pub fn new(wait: Duration) -> Self {
        Self { next_pool: 0, wait }
    }

    pub fn host(&self) -> String {
        format!("{}{NTP_POOL_SUFFIX}", self.next_pool)
    }

    /// Resolve the pool host, query its first IPv4 address, set system time. The pool index
    /// advances after every attempt. The caller applies [`NTP_FAILURE_DELAY`] before reporting a
    /// failure if it wants the stock pacing.
    pub fn sync(
        &mut self,
        net: &mut impl NetIo,
        runner: &mut impl CommandRunner,
    ) -> Result<TimeSet, TimeError> {
        let host = self.host();
        self.next_pool = (self.next_pool + 1) % NTP_POOL_COUNT;
        sync_with(net, runner, &host, self.wait)
    }
}

fn sync_with(
    net: &mut impl NetIo,
    runner: &mut impl CommandRunner,
    host: &str,
    wait: Duration,
) -> Result<TimeSet, TimeError> {
    let addresses: Vec<IpAddr> = net
        .resolve(host)
        .map(|all| {
            (all.into_iter().filter(IpAddr::is_ipv4))
                .take(NTP_ADDRESSES)
                .collect()
        })
        .and_then(|found: Vec<_>| {
            if found.is_empty() {
                Err(NetError::NoAddress)
            } else {
                Ok(found)
            }
        })
        .map_err(|cause| TimeError::Resolve {
            host: host.to_owned(),
            cause,
        })?;
    // Owned (O): stock asks the first address only. A pool host lists several servers and one
    // can be silent (S8 on the QC, 2026-10-15: 30 s timeout, no time set), so the wait is
    // shared by up to NTP_ADDRESSES of them and the first good reply wins.
    let each = wait / addresses.len() as u32;
    let mut failure = None;
    for address in addresses {
        let reply = net.udp_request(SocketAddr::new(address, NTP_PORT), &ntp_request(), each);
        match reply
            .map_err(TimeError::Exchange)
            .and_then(|reply| parse_ntp_response(&reply).map_err(TimeError::from))
        {
            Ok(unix_seconds) => {
                set_system_time(runner, unix_seconds)?;
                return Ok(TimeSet { unix_seconds });
            }
            Err(error) => failure = Some(error),
        }
    }
    Err(failure.expect("at least one address was tried"))
}

/// Owned policy: BusyBox `date -u -s "YYYY-MM-DD hh:mm:ss"`. Stock calls `mktime` + `stime`
/// in-process (VA 0x004b23e4) with `tm_isdst = 0`; the device has no time zone, so both set UTC.
pub fn set_system_time(
    runner: &mut impl CommandRunner,
    unix_seconds: i64,
) -> Result<(), TimeError> {
    let stamp = utc_stamp(unix_seconds);
    let output = runner.run(&Command::new("date", ["-u", "-s", stamp.as_str()]))?;
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| TimeError::Set(format!("date ended with {:?}", output.status)))
}

/// `YYYY-MM-DD hh:mm:ss` in UTC (proleptic Gregorian, days-from-civil inverse).
fn utc_stamp(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let second = unix_seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        second / 3_600,
        second % 3_600 / 60,
        second % 60
    )
}

/// Internet manager status (observed values 1 and 2; the full enum is not recovered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternetStatus {
    Disconnected = 1,
    Connected = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternetNotice {
    Status(InternetStatus),
    TimeSet(TimeSet),
    TimeFailed(TimeError),
    /// First successful sync since start. Stock then starts an update check when automatic
    /// checks are on and the updater is not downloading (VA 0x004af488).
    FirstTimeSync,
}

/// Blocking internet work. [`InternetManager::due`] hands it out, a worker runs it with
/// [`InternetWork::run`], and [`InternetManager::finish`] takes the result back, so the event
/// loop never waits for the probe (up to [`STOCK_PROBE_TIMEOUT`]) or NTP (up to
/// [`STOCK_NTP_WAIT`]). Stock runs both on worker threads too (VA 0x004af778, 0x004b2a38).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternetWork {
    Probe {
        target: SocketAddr,
        timeout: Duration,
    },
    Time {
        host: String,
        wait: Duration,
    },
    /// Cortex Control SystemTimeSync (`0x1ba1b0` → `0x4b271c`): the same clock write as NTP
    /// (`0x4b23e4`), and the same result slot (`0x4af488`).
    SetClock {
        unix_seconds: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternetResult {
    Probe(Result<SocketAddr, ProbeError>),
    Time(Result<TimeSet, TimeError>),
}

impl InternetWork {
    pub fn run(self, net: &mut impl NetIo, runner: &mut impl CommandRunner) -> InternetResult {
        match self {
            Self::Probe { target, timeout } => InternetResult::Probe(connect(net, target, timeout)),
            Self::Time { host, wait } => InternetResult::Time(sync_with(net, runner, &host, wait)),
            Self::SetClock { unix_seconds } => InternetResult::Time(
                set_system_time(runner, unix_seconds).map(|()| TimeSet { unix_seconds }),
            ),
        }
    }

    /// The result of work that could not run at all.
    pub fn failed(self, cause: NetError) -> InternetResult {
        match self {
            Self::Probe { target, .. } => InternetResult::Probe(Err(ProbeError { target, cause })),
            Self::Time { .. } | Self::SetClock { .. } => {
                InternetResult::Time(Err(TimeError::Exchange(cause)))
            }
        }
    }
}

/// Probe and time-sync scheduling over an associated link with a lease.
///
/// Stock retries NTP until the first success, each failure delayed by [`NTP_FAILURE_DELAY`]
/// (VA 0x004af488 restarts the worker). Probes repeat every `recheck` while the link is up.
/// At most one probe and one sync are in flight; NTP starts only after a probe connected.
#[derive(Debug, Clone)]
pub struct InternetManager {
    pub probe: InternetProbe,
    pub time: TimeSync,
    recheck: Duration,
    link_up: bool,
    status: InternetStatus,
    time_synced: bool,
    next_probe_ms: u64,
    next_time_ms: u64,
    probing: bool,
    syncing: bool,
    /// `DisableConnectionCheck` (settings `+0x8c`, getter 0x0064e628).
    check_disabled: bool,
}

impl InternetManager {
    pub fn new(probe: InternetProbe, time: TimeSync, recheck: Duration) -> Self {
        Self {
            probe,
            time,
            recheck,
            link_up: false,
            status: InternetStatus::Disconnected,
            time_synced: false,
            next_probe_ms: 0,
            next_time_ms: 0,
            probing: false,
            syncing: false,
            check_disabled: false,
        }
    }

    /// The `DisableConnectionCheck` setting. While set, stock sends no ping: the status is the
    /// link state (0x004b0158, 0x004b03bc, 0x004b04b0). NTP still runs once Connected (I: the
    /// cloud signing needs the wall clock).
    pub fn set_check_disabled(&mut self, disabled: bool, now_ms: u64) -> Vec<InternetNotice> {
        if self.check_disabled == disabled {
            return Vec::new();
        }
        self.check_disabled = disabled;
        self.set_link(self.link_up, now_ms)
    }

    pub fn status(&self) -> InternetStatus {
        self.status
    }

    pub fn time_synced(&self) -> bool {
        self.time_synced
    }

    /// Wi-Fi association plus DHCP lease. Going down reports Disconnected at once.
    pub fn set_link(&mut self, up: bool, now_ms: u64) -> Vec<InternetNotice> {
        self.link_up = up;
        self.next_probe_ms = now_ms;
        self.set_status(match (up, self.check_disabled) {
            (false, _) => InternetStatus::Disconnected,
            (true, true) => InternetStatus::Connected,
            (true, false) => self.status,
        })
    }

    fn set_status(&mut self, status: InternetStatus) -> Vec<InternetNotice> {
        if self.status == status {
            return Vec::new();
        }
        self.status = status;
        vec![InternetNotice::Status(status)]
    }

    /// SystemTimeSync (`0x1ba1b0`): Cortex Control sets the clock only while no sync has
    /// succeeded since start (`0x4af54c`, `+0x24`). Stock waits for a running NTP worker first
    /// (`0x4b2a08`); here both may run and the first success counts.
    pub fn set_clock_if_unset(&mut self, ms_since_epoch: u64) -> Option<InternetWork> {
        if self.time_synced {
            return None;
        }
        let unix_seconds = i64::try_from(ms_since_epoch / 1000).ok()?;
        self.syncing = true;
        Some(InternetWork::SetClock { unix_seconds })
    }

    /// Work that is due now. Never blocks. Each item must come back through [`Self::finish`].
    pub fn due(&mut self, now_ms: u64) -> Vec<InternetWork> {
        let mut work = Vec::new();
        if !self.link_up {
            return work;
        }
        if !self.probing && !self.check_disabled && now_ms >= self.next_probe_ms {
            self.probing = true;
            self.next_probe_ms = now_ms.saturating_add(self.recheck.as_millis() as u64);
            work.push(InternetWork::Probe {
                target: self.probe.target(),
                timeout: self.probe.timeout,
            });
        }
        if !self.syncing
            && self.status == InternetStatus::Connected
            && !self.time_synced
            && now_ms >= self.next_time_ms
        {
            self.syncing = true;
            let host = self.time.host();
            self.time.next_pool = (self.time.next_pool + 1) % NTP_POOL_COUNT;
            work.push(InternetWork::Time {
                host,
                wait: self.time.wait,
            });
        }
        work
    }

    /// A finished [`InternetWork`]. A probe result that arrives after the link went down is
    /// dropped: the link change already reported Disconnected.
    pub fn finish(&mut self, result: InternetResult, now_ms: u64) -> Vec<InternetNotice> {
        match result {
            InternetResult::Probe(result) => {
                self.probing = false;
                self.probe.record(&result);
                if !self.link_up || self.check_disabled {
                    return Vec::new();
                }
                self.set_status(match result {
                    Ok(_) => InternetStatus::Connected,
                    Err(_) => InternetStatus::Disconnected,
                })
            }
            // `0x4af488`: the first success since start signals once (`+0x24` was clear); an
            // NTP result and a Control time set may both arrive.
            InternetResult::Time(Ok(set)) => {
                self.syncing = false;
                let first = !std::mem::replace(&mut self.time_synced, true);
                let mut notices = vec![InternetNotice::TimeSet(set)];
                notices.extend(first.then_some(InternetNotice::FirstTimeSync));
                notices
            }
            InternetResult::Time(Err(error)) => {
                self.syncing = false;
                self.next_time_ms = now_ms.saturating_add(NTP_FAILURE_DELAY.as_millis() as u64);
                vec![InternetNotice::TimeFailed(error)]
            }
        }
    }

    /// Runs whatever is due on the calling thread, to the end. Blocks for the probe and the NTP
    /// exchange: never call it from an event loop.
    pub fn poll(
        &mut self,
        net: &mut impl NetIo,
        runner: &mut impl CommandRunner,
        now_ms: u64,
    ) -> Vec<InternetNotice> {
        let mut notices = Vec::new();
        loop {
            let work = self.due(now_ms);
            if work.is_empty() {
                return notices;
            }
            for item in work {
                let result = item.run(net, runner);
                notices.extend(self.finish(result, now_ms));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{ScriptedNet, ScriptedRunner};

    fn reply(first: u8, stratum: u8, ntp_seconds: u32) -> Vec<u8> {
        let mut packet = vec![0u8; NTP_PACKET_LEN];
        packet[0] = first;
        packet[1] = stratum;
        packet[40..44].copy_from_slice(&ntp_seconds.to_be_bytes());
        packet
    }

    #[test]
    fn stock_request_bytes() {
        let packet = ntp_request();
        assert_eq!(&packet[..4], &[0xe3, 0x00, 0x06, 0xec]);
        assert_eq!(&packet[12..16], b"1N14");
        assert!(packet[4..12].iter().chain(&packet[16..]).all(|&b| b == 0));
    }

    #[test]
    fn response_validation_and_era() {
        // 2026-10-10 00:00:00 UTC.
        let ntp = (1_791_590_400i64 + NTP_UNIX_OFFSET) as u32;
        assert_eq!(parse_ntp_response(&reply(0x24, 2, ntp)), Ok(1_791_590_400));
        assert_eq!(
            parse_ntp_response(&reply(0x24, 2, 5)[..47]),
            Err(NtpError::Size(47))
        );
        assert_eq!(
            parse_ntp_response(&reply(0x23, 2, ntp)),
            Err(NtpError::Mode(3))
        );
        assert_eq!(
            parse_ntp_response(&reply(0xe4, 2, ntp)),
            Err(NtpError::Unsynchronized)
        );
        assert_eq!(
            parse_ntp_response(&reply(0x24, 0, ntp)),
            Err(NtpError::Unsynchronized)
        );
        assert_eq!(
            parse_ntp_response(&reply(0x24, 2, 0)),
            Err(NtpError::Unsynchronized)
        );
        // Era 1 (after 2036-02-07) keeps counting forward.
        assert_eq!(
            parse_ntp_response(&reply(0x24, 2, 10)),
            Ok((1i64 << 32) + 10 - NTP_UNIX_OFFSET)
        );
    }

    #[test]
    fn sync_sets_time_and_rotates_pool() {
        let mut net = ScriptedNet::default();
        net.names.push_back(Ok(vec![
            "2001:db8::1".parse().unwrap(),
            "192.0.2.7".parse().unwrap(),
        ]));
        net.udp.push_back(Ok(reply(
            0x24,
            2,
            (1_791_590_400i64 + NTP_UNIX_OFFSET) as u32,
        )));
        let mut runner = ScriptedRunner::default();
        runner.expect_ok("date -u -s 2026-10-10 00:00:00");
        let mut sync = TimeSync::new(STOCK_NTP_WAIT);
        assert_eq!(
            sync.sync(&mut net, &mut runner).unwrap().unix_seconds,
            1_791_590_400
        );
        assert_eq!(net.log, ["resolve 0.pool.ntp.org", "udp 192.0.2.7:123 48B"]);
        assert_eq!(sync.host(), "1.pool.ntp.org");
        assert!(runner.is_done());
    }

    /// S8 on the QC (owned): a silent first server does not fail the attempt; the next IPv4
    /// address of the same pool host answers.
    #[test]
    fn sync_tries_the_next_address_after_a_silent_server() {
        let mut net = ScriptedNet::default();
        net.names.push_back(Ok(vec![
            "192.0.2.7".parse().unwrap(),
            "192.0.2.8".parse().unwrap(),
        ]));
        net.udp.extend([
            Err(NetError::Timeout),
            Ok(reply(0x24, 2, (1_791_590_400i64 + NTP_UNIX_OFFSET) as u32)),
        ]);
        let mut runner = ScriptedRunner::default();
        runner.expect_ok("date -u -s 2026-10-10 00:00:00");
        let mut sync = TimeSync::new(STOCK_NTP_WAIT);
        assert_eq!(
            sync.sync(&mut net, &mut runner).unwrap().unix_seconds,
            1_791_590_400
        );
        assert_eq!(
            net.log,
            [
                "resolve 0.pool.ntp.org",
                "udp 192.0.2.7:123 48B",
                "udp 192.0.2.8:123 48B"
            ]
        );
        assert!(runner.is_done());
    }

    #[test]
    fn probe_rotates_target_after_failure() {
        let mut net = ScriptedNet::default();
        net.tcp.extend([Err(NetError::Timeout), Ok(())]);
        let mut probe = InternetProbe::new(STOCK_PROBE_TIMEOUT);
        assert!(probe.probe(&mut net).is_err());
        assert_eq!(probe.probe(&mut net).unwrap().to_string(), "8.8.4.4:53");
        assert_eq!(
            net.log,
            ["tcp 8.8.8.8:53 40000ms", "tcp 8.8.4.4:53 40000ms"]
        );
    }

    /// `DisableConnectionCheck` (0x004b0158): no ping; the status follows the link.
    #[test]
    fn disabled_check_takes_the_status_from_the_link() {
        let mut manager = InternetManager::new(
            InternetProbe::new(STOCK_PROBE_TIMEOUT),
            TimeSync::new(STOCK_NTP_WAIT),
            STOCK_RECHECK_INTERVAL,
        );
        assert!(manager.set_check_disabled(true, 0).is_empty());
        assert_eq!(
            manager.set_link(true, 0),
            [InternetNotice::Status(InternetStatus::Connected)]
        );
        let work = manager.due(0);
        assert!(
            matches!(work.as_slice(), [InternetWork::Time { .. }]),
            "{work:?}"
        );
        // Enabled again: the status stays until a probe answers, and one is due at once.
        assert!(manager.set_check_disabled(false, 10).is_empty());
        assert!(matches!(
            manager.due(10).as_slice(),
            [InternetWork::Probe { .. }]
        ));
    }

    #[test]
    fn manager_probes_then_syncs_time_once() {
        let mut net = ScriptedNet::default();
        net.tcp.extend([Ok(()), Ok(())]);
        net.names.extend([
            Err(NetError::NoAddress),
            Ok(vec!["192.0.2.7".parse().unwrap()]),
        ]);
        net.udp.push_back(Ok(reply(
            0x24,
            2,
            (1_791_590_400i64 + NTP_UNIX_OFFSET) as u32,
        )));
        let mut runner = ScriptedRunner::default();
        runner.expect_ok("date -u -s 2026-10-10 00:00:00");
        let mut manager = InternetManager::new(
            InternetProbe::new(STOCK_PROBE_TIMEOUT),
            TimeSync::new(STOCK_NTP_WAIT),
            STOCK_RECHECK_INTERVAL,
        );
        assert!(manager.poll(&mut net, &mut runner, 0).is_empty());
        manager.set_link(true, 0);
        let first = manager.poll(&mut net, &mut runner, 0);
        assert_eq!(first[0], InternetNotice::Status(InternetStatus::Connected));
        assert!(matches!(
            first[1],
            InternetNotice::TimeFailed(TimeError::Resolve { .. })
        ));
        // NTP waits the stock 5 s after a failure; the probe recheck is also 5 s.
        assert!(manager.poll(&mut net, &mut runner, 4_999).is_empty());
        let second = manager.poll(&mut net, &mut runner, 5_000);
        assert_eq!(
            second,
            [
                InternetNotice::TimeSet(TimeSet {
                    unix_seconds: 1_791_590_400
                }),
                InternetNotice::FirstTimeSync,
            ]
        );
        assert_eq!(
            manager.set_link(false, 6_000),
            [InternetNotice::Status(InternetStatus::Disconnected)]
        );
    }

    /// SystemTimeSync `0x1ba1b0`: Control sets the clock only before the first sync; its write
    /// is the NTP one and its success is the first-sync signal (`0x4af488`) once only.
    #[test]
    fn control_clock_set_runs_once_before_the_first_sync() {
        let mut manager = InternetManager::new(
            InternetProbe::new(STOCK_PROBE_TIMEOUT),
            TimeSync::new(STOCK_NTP_WAIT),
            STOCK_RECHECK_INTERVAL,
        );
        let work = manager.set_clock_if_unset(1_791_590_400_999).unwrap();
        let mut runner = ScriptedRunner::default();
        runner.expect_ok("date -u -s 2026-10-10 00:00:00");
        let result = work.run(&mut ScriptedNet::default(), &mut runner);
        let set = InternetNotice::TimeSet(TimeSet {
            unix_seconds: 1_791_590_400,
        });
        assert_eq!(
            manager.finish(result.clone(), 0),
            [set.clone(), InternetNotice::FirstTimeSync]
        );
        assert_eq!(manager.finish(result, 0), [set]);
        assert_eq!(manager.set_clock_if_unset(1_791_590_500_000), None);
    }

    /// The event loop only dispatches: one probe in flight at a time, no NTP before a probe
    /// connected, and a probe result after the link went down changes nothing.
    #[test]
    fn manager_dispatches_once_and_drops_stale_probe() {
        let mut manager = InternetManager::new(
            InternetProbe::new(STOCK_PROBE_TIMEOUT),
            TimeSync::new(STOCK_NTP_WAIT),
            STOCK_RECHECK_INTERVAL,
        );
        manager.set_link(true, 0);
        let target: SocketAddr = "8.8.8.8:53".parse().unwrap();
        assert_eq!(
            manager.due(0),
            [InternetWork::Probe {
                target,
                timeout: STOCK_PROBE_TIMEOUT
            }]
        );
        assert!(manager.due(10_000).is_empty(), "probe still in flight");
        manager.set_link(false, 10_000);
        let late = InternetResult::Probe(Ok(target));
        assert!(manager.finish(late, 10_001).is_empty());
        assert_eq!(manager.status(), InternetStatus::Disconnected);

        manager.set_link(true, 20_000);
        let [probe] = &manager.due(20_000)[..] else {
            panic!("one probe");
        };
        let failed = probe.clone().failed(NetError::Timeout);
        assert!(manager.finish(failed, 20_001).is_empty());
        assert!(manager.due(20_002).is_empty(), "no NTP while unreachable");
        assert_eq!(manager.probe.target().to_string(), "8.8.4.4:53");
    }
}

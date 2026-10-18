//! Caller-supplied process records; no procfs access or process control.

use std::{collections::BTreeSet, num::NonZeroU32};

use thiserror::Error;

/// Owned parser budget, not a stock watchdog limit.
pub const MAX_STAT_BYTES: usize = 4_096;
const MAX_NAME_BYTES: usize = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessName {
    bytes: [u8; MAX_NAME_BYTES],
    len: u8,
}

impl ProcessName {
    /// Target comm bytes, not UTF-8 or an executable path. No trimming occurs. The 15-byte bound is the
    /// user-task comm limit (`TASK_COMM_LEN` - 1); longer observed names never equal a target.
    pub fn new(name: &[u8]) -> Result<Self, ParseError> {
        if name.is_empty() || name.len() > MAX_NAME_BYTES || name.contains(&0) {
            return Err(ParseError::Name);
        }
        let mut bytes = [0; MAX_NAME_BYTES];
        bytes[..name.len()].copy_from_slice(name);
        Ok(Self {
            bytes,
            len: name.len() as u8,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// The start tick distinguishes a new process that reuses a PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: NonZeroU32,
    pub start_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessTarget {
    /// Any non-exited process with exactly these comm bytes.
    Name(ProcessName),
    /// Only this identity; a reused PID does not satisfy the target.
    Identity(ProcessIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecord {
    identity: ProcessIdentity,
    comm: Box<[u8]>,
    state: u8,
}

impl ProcessRecord {
    /// Parse fields 1, 2, 3 and 22 of one Linux `/proc/<pid>/stat` record.
    /// Fields after starttime remain opaque. No read or syscall is performed.
    /// Observed comm is not limited to 15 bytes: Linux >= 4.18 prints workqueue names and >= 5.17 full
    /// kthread names there. It may be empty (`prctl(PR_SET_NAME, "")`); `MAX_STAT_BYTES` bounds it.
    pub fn parse_stat(input: &[u8]) -> Result<Self, ParseError> {
        if input.len() > MAX_STAT_BYTES {
            return Err(ParseError::TooLong);
        }
        if input.contains(&0) {
            return Err(ParseError::Syntax);
        }
        let open = input
            .iter()
            .position(|&byte| byte == b'(')
            .ok_or(ParseError::Syntax)?;
        let close = input
            .iter()
            .rposition(|&byte| byte == b')')
            .ok_or(ParseError::Syntax)?;
        if open < 2 || close <= open || input[open - 1] != b' ' {
            return Err(ParseError::Syntax);
        }
        let pid = decimal(&input[..open - 1])
            .and_then(|pid| u32::try_from(pid).ok())
            .and_then(NonZeroU32::new)
            .ok_or(ParseError::Pid)?;
        let comm = input[open + 1..close].into();
        let tail = &input[close + 1..];
        if !tail.starts_with(b" ") {
            return Err(ParseError::Syntax);
        }
        let mut fields = tail
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|s| !s.is_empty());
        let state = fields.next().ok_or(ParseError::Syntax)?;
        if state.len() != 1 || !state[0].is_ascii_alphabetic() {
            return Err(ParseError::State);
        }
        // After field 3, skip fields 4..21 and require unsigned field 22.
        let start_ticks = fields
            .nth(18)
            .and_then(decimal)
            .ok_or(ParseError::StartTicks)?;
        Ok(Self {
            identity: ProcessIdentity { pid, start_ticks },
            comm,
            state: state[0],
        })
    }

    pub fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    /// Raw observed comm bytes.
    pub fn comm(&self) -> &[u8] {
        &self.comm
    }

    pub fn state_code(&self) -> u8 {
        self.state
    }

    /// Presence is not health. Zombies and dead tasks cannot satisfy a target.
    pub fn matches(&self, target: &ProcessTarget) -> bool {
        if matches!(self.state, b'Z' | b'X' | b'x') {
            return false;
        }
        match target {
            ProcessTarget::Name(name) => *self.comm == *name.as_bytes(),
            ProcessTarget::Identity(identity) => self.identity == *identity,
        }
    }
}

fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, &byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("stat record exceeds the owned byte budget")]
    TooLong,
    #[error("malformed stat record")]
    Syntax,
    #[error("invalid process ID")]
    Pid,
    #[error("target name must contain 1..=15 non-NUL bytes")]
    Name,
    #[error("invalid process state field")]
    State,
    #[error("missing or invalid unsigned start tick field")]
    StartTicks,
}

/// Return the records the adapter actually observed for the caller's selected
/// process scope. Any failure to enumerate, read, parse or complete that scope
/// within the adapter's budgets must be an error, never an empty or partial slice
/// presented as absence.
///
/// Coherence is per scope, not a promise of an atomic table. A single-task
/// (identity) scope is one bounded read and is coherent for that task; a
/// whole-system (name) scope on procfs is a best-effort walk during which tasks
/// may start or exit. Prefer identity targets when a process handle is available.
/// Allocation and I/O budgets are the adapter's responsibility; the supervisor
/// validates the returned length and rejects duplicate PIDs before matching.
pub trait ProcessSource {
    type Error;

    fn snapshot(&mut self) -> Result<&[ProcessRecord], Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SnapshotError {
    #[error("process snapshot has {actual} records; maximum is {maximum}")]
    TooMany { actual: usize, maximum: usize },
    #[error("duplicate process ID {0} in snapshot")]
    DuplicatePid(NonZeroU32),
}

pub(crate) fn presence(
    records: &[ProcessRecord],
    target: &ProcessTarget,
    maximum: usize,
) -> Result<bool, SnapshotError> {
    if records.len() > maximum {
        return Err(SnapshotError::TooMany {
            actual: records.len(),
            maximum,
        });
    }
    let mut seen = BTreeSet::new();
    for record in records {
        if !seen.insert(record.identity.pid) {
            return Err(SnapshotError::DuplicatePid(record.identity.pid));
        }
    }
    Ok(records.iter().any(|record| record.matches(target)))
}

#[cfg(test)]
pub(crate) fn fixture(pid: u32, name: &[u8], state: u8, start_ticks: u64) -> Vec<u8> {
    let mut bytes = format!("{pid} (").into_bytes();
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(b") ");
    bytes.push(state);
    for _ in 4..=21 {
        bytes.extend_from_slice(b" 0");
    }
    bytes.extend_from_slice(format!(" {start_ticks} 0\n").as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_preserve_comm_bytes_and_find_field_22() {
        let name = b"a ) b(\xff\n";
        let mut bytes = fixture(42, name, b'S', 987);
        // Unrelated signed fields must not shift the starttime position.
        let index = bytes.windows(2).position(|w| w == b" 0").unwrap();
        bytes.splice(index + 1..index + 2, b"-123".iter().copied());
        let record = ProcessRecord::parse_stat(&bytes).unwrap();
        assert_eq!(record.comm(), name);
        assert_eq!(record.identity().pid.get(), 42);
        assert_eq!(record.identity().start_ticks, 987);
        assert_eq!(record.state_code(), b'S');
    }

    #[test]
    fn malformed_stat_records_and_numeric_overflow_are_refused() {
        for bytes in [
            b"".to_vec(),
            fixture(0, b"app", b'R', 0),
            fixture(1, b"a\0b", b'R', 0),
            fixture(1, b"app", b'?', 0),
            b"1 (app) R 0".to_vec(),
            b"4294967296 (app) R".to_vec(),
            b"+1 (app) R".to_vec(),
            b"1 (app)R".to_vec(),
        ] {
            assert!(ProcessRecord::parse_stat(&bytes).is_err(), "{bytes:?}");
        }
        let valid = fixture(1, b"app", b'R', u64::MAX);
        assert_eq!(
            ProcessRecord::parse_stat(&valid)
                .unwrap()
                .identity()
                .start_ticks,
            u64::MAX
        );
        let mut overflow = valid.clone();
        let at = overflow
            .windows(20)
            .position(|w| w == b"18446744073709551615")
            .unwrap();
        overflow[at + 19] = b'6';
        assert_eq!(
            ProcessRecord::parse_stat(&overflow),
            Err(ParseError::StartTicks)
        );
        let mut at_limit = fixture(1, b"app", b'R', 0);
        at_limit.resize(MAX_STAT_BYTES, b' ');
        assert!(ProcessRecord::parse_stat(&at_limit).is_ok());
        at_limit.push(b' ');
        assert_eq!(
            ProcessRecord::parse_stat(&at_limit),
            Err(ParseError::TooLong)
        );
    }

    #[test]
    fn observed_comm_is_not_capped_at_the_target_limit() {
        // Linux >= 4.18 workqueue worker; a 15-byte cap made every whole-system snapshot an error.
        let long = b"kworker/u16:2-events_unbound";
        let record = ProcessRecord::parse_stat(&fixture(77, long, b'I', 5)).unwrap();
        assert_eq!(record.comm(), long);
        assert!(!record.matches(&ProcessTarget::Name(ProcessName::new(&long[..15]).unwrap())));
        assert_eq!(
            ProcessRecord::parse_stat(&fixture(78, b"", b'S', 5))
                .unwrap()
                .comm(),
            b""
        );
        assert_eq!(ProcessName::new(b"0123456789abcdef"), Err(ParseError::Name));
        assert_eq!(ProcessName::new(b""), Err(ParseError::Name));
    }

    #[test]
    fn selection_is_exact_rejects_exited_tasks_and_handles_pid_reuse() {
        let record = ProcessRecord::parse_stat(&fixture(2, b"app", b'S', 8)).unwrap();
        assert!(record.matches(&ProcessTarget::Name(ProcessName::new(b"app").unwrap())));
        assert!(!record.matches(&ProcessTarget::Name(ProcessName::new(b"ap").unwrap())));
        assert!(record.matches(&ProcessTarget::Identity(record.identity())));
        assert!(!record.matches(&ProcessTarget::Identity(ProcessIdentity {
            start_ticks: 9,
            ..record.identity()
        })));
        for state in [b'Z', b'X', b'x'] {
            let exited = ProcessRecord::parse_stat(&fixture(2, b"app", state, 8)).unwrap();
            assert!(!exited.matches(&ProcessTarget::Identity(record.identity())));
            assert!(!exited.matches(&ProcessTarget::Name(ProcessName::new(b"app").unwrap())));
        }
    }
}

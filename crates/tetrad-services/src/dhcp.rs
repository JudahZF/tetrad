//! DHCP through BusyBox 1.23.1 `udhcpc`, as stock ZenUI (start VA 0x0088e3b4, stop 0x0088e5c4).
//!
//! `--now --quit` takes one lease and exits: nothing renews it. The stock udhcpc
//! `default.script` applies address, routes and resolvers. The applied result is then read back
//! from `ifconfig` and `route -n` (owned policy) so callers get a lease fact, not an exit code.

use std::net::Ipv4Addr;

use thiserror::Error;

use crate::exec::{Command, CommandError, CommandRunner, ExitStatus};

pub const INTERFACE: &str = "wlan0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedLease {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Option<Ipv4Addr>,
    /// From udhcpc's `Lease of <ip> obtained, lease time <s>` line, when present.
    pub lease_seconds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DhcpError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("`{command}` ended with {status:?}")]
    Failed { command: String, status: ExitStatus },
    #[error("{0} has no IPv4 address after DHCP")]
    NoAddress(&'static str),
    #[error("netmask is not contiguous")]
    BadMask,
}

fn checked(runner: &mut impl CommandRunner, command: Command) -> Result<String, DhcpError> {
    let output = runner.run(&command)?;
    if !output.status.success() {
        return Err(DhcpError::Failed {
            command: command.line(),
            status: output.status,
        });
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

/// Run one lease acquisition and read back what the script applied.
pub fn acquire(runner: &mut impl CommandRunner) -> Result<AppliedLease, DhcpError> {
    let log = checked(
        runner,
        Command::new("udhcpc", ["--now", "--quit", "-i", INTERFACE]),
    )?;
    let lease_seconds = log.lines().find_map(|line| {
        line.split_once("lease time ")
            .filter(|_| line.contains("Lease of "))
            .and_then(|(_, rest)| rest.trim().parse().ok())
    });
    let ifconfig = checked(runner, Command::new("ifconfig", [INTERFACE]))?;
    let (address, mask) = parse_ifconfig(&ifconfig).ok_or(DhcpError::NoAddress(INTERFACE))?;
    let prefix_len = prefix_len(mask).ok_or(DhcpError::BadMask)?;
    let routes = checked(runner, Command::new("route", ["-n"]))?;
    Ok(AppliedLease {
        address,
        prefix_len,
        gateway: default_gateway(&routes, INTERFACE),
        lease_seconds,
    })
}

/// Stock stop: `killall udhcpc`. Exit 1 (nothing running) is not an error.
pub fn stop(runner: &mut impl CommandRunner) -> Result<(), DhcpError> {
    let command = Command::new("killall", ["udhcpc"]);
    match runner.run(&command)?.status {
        ExitStatus::Code(0 | 1) => Ok(()),
        status => Err(DhcpError::Failed {
            command: command.line(),
            status,
        }),
    }
}

/// BusyBox `ifconfig`: `inet addr:A  Bcast:B  Mask:M`.
fn parse_ifconfig(text: &str) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("inet addr:"))?;
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .and_then(|value| value.parse().ok())
    };
    Some((field("addr:")?, field("Mask:")?))
}

fn prefix_len(mask: Ipv4Addr) -> Option<u8> {
    let bits = u32::from(mask);
    let ones = bits.leading_ones();
    (bits.checked_shl(ones).unwrap_or(0) == 0 && ones > 0).then_some(ones as u8)
}

/// `route -n` row with destination `0.0.0.0`, flag `G` and our interface.
fn default_gateway(text: &str, interface: &str) -> Option<Ipv4Addr> {
    text.lines().find_map(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        match columns.as_slice() {
            ["0.0.0.0", gateway, _, flags, .., iface]
                if flags.contains('G') && *iface == interface =>
            {
                gateway.parse().ok()
            }
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Output;
    use crate::fake::ScriptedRunner;

    #[test]
    fn lease_is_read_back_from_interface_and_routes() {
        let mut runner = ScriptedRunner::default();
        runner
            .expect(
                "udhcpc --now --quit -i wlan0",
                Ok(Output::code(0).with_stdout(
                    "udhcpc: sending discover\nudhcpc: Lease of 192.0.2.40 obtained, lease time 86400\n",
                )),
            )
            .expect(
                "ifconfig wlan0",
                Ok(Output::code(0).with_stdout(
                    "wlan0     Link encap:Ethernet  HWaddr 02:00:00:00:00:01\n          inet addr:192.0.2.40  Bcast:192.0.2.255  Mask:255.255.255.0\n",
                )),
            )
            .expect(
                "route -n",
                Ok(Output::code(0).with_stdout(
                    "Kernel IP routing table\nDestination     Gateway         Genmask         Flags Metric Ref    Use Iface\n0.0.0.0         192.0.2.1       0.0.0.0         UG    0      0        0 wlan0\n192.0.2.0       0.0.0.0         255.255.255.0   U     0      0        0 wlan0\n",
                )),
            );
        let lease = acquire(&mut runner).unwrap();
        assert_eq!(
            lease,
            AppliedLease {
                address: Ipv4Addr::new(192, 0, 2, 40),
                prefix_len: 24,
                gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
                lease_seconds: Some(86_400),
            }
        );
    }

    #[test]
    fn failed_client_is_not_a_lease() {
        let mut runner = ScriptedRunner::default();
        runner.expect("udhcpc --now --quit -i wlan0", Ok(Output::code(1)));
        assert!(matches!(
            acquire(&mut runner),
            Err(DhcpError::Failed { .. })
        ));
        assert_eq!(prefix_len(Ipv4Addr::new(255, 0, 255, 0)), None);
    }
}

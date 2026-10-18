//! Bounded Linux process-presence checks and stock lifecycle plans. Reads procfs
//! and an optional command-line file only; every output is a printed intent. No
//! kill, reboot, unmount, launcher or hardware-watchdog feed.
#![forbid(unsafe_code)]

use std::{
    ffi::OsString,
    fs::File,
    io::Read,
    num::{NonZeroU32, NonZeroUsize},
    process::ExitCode,
    time::Duration,
};

use tetrad_watchdog::{
    Effect, Supervisor, WatchdogEvent,
    boot::{self, BootBackend, BootConfig, BootIntent, Probe, Status},
    cmdline::{KernelCmdline, Product},
    monitor::{self, Budget, MonotonicClock, Sample, Stop, ThreadSleep},
    power::{self, RestartReason},
    process::{ProcessIdentity, ProcessName, ProcessRecord, ProcessSource, ProcessTarget},
    procfs::{ProcfsSource, ScanBudget, Scope},
};

const USAGE: &str = "\
tetrad-watchdog: bounded Linux process-presence monitor (read-only, no effects)

  tetrad-watchdog inspect (--name COMM | --pid PID [--start-ticks T]) [OPTIONS]
  tetrad-watchdog monitor (--name COMM | --pid PID --start-ticks T)
                       (--samples N | --duration SECONDS) [OPTIONS]
  tetrad-watchdog plan (boot | power-off | restart | reboot-helper)
                    [--cmdline FILE] [--mini]

Options:
  --proc DIR            procfs root (default /proc); must be a trusted procfs mount
  --max-processes N     record budget for a system scan (default 8192)
  --max-entries N       directory entries visited per scan, numeric or not
                        (default 32768; must be >= --max-processes)
  --samples N           stop after N deliveries or errors (1..=10000)
  --duration SECONDS    stop after this wall-clock time (1..=3600)

inspect reads one snapshot and prints the target's presence.
monitor runs the recovered 500 ms presence / 1 s countdown model and prints
each acknowledged delivery. A reboot intent is printed, never executed.
plan prints the stock step order. --cmdline reads a kernel command line file
(at most 4 KiB); without it, reboot-helper takes the stock open-failure path.
restart takes the product from the libzenhal parser; --mini forces the mini
(as the \"ATMA on QC\" emulation file does).
The boot plan is a dry run: commands succeed and file probes are absent except
the splash startUI marker.";

const MAX_CMDLINE_BYTES: u64 = 4_096;

const DEFAULT_MAX_PROCESSES: usize = 8_192;
const DEFAULT_MAX_ENTRIES: usize = 32_768;

#[derive(Debug)]
enum Command {
    Inspect,
    Monitor,
}

#[derive(Debug)]
struct Options {
    command: Command,
    name: Option<ProcessName>,
    pid: Option<NonZeroU32>,
    start_ticks: Option<u64>,
    root: OsString,
    max_processes: NonZeroUsize,
    max_entries: NonZeroUsize,
    samples: Option<NonZeroU32>,
    duration: Option<Duration>,
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args[0] == "plan" {
        return match plan(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("tetrad-watchdog: {message}\nuse --help for usage");
                ExitCode::from(2)
            }
        };
    }
    let options = match parse(&args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("tetrad-watchdog: {message}\nuse --help for usage");
            return ExitCode::from(2);
        }
    };
    let outcome = match options.command {
        Command::Inspect => inspect(&options),
        Command::Monitor => run_monitor(&options),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("tetrad-watchdog: {message}");
            ExitCode::FAILURE
        }
    }
}

fn parse(args: &[OsString]) -> Result<Options, String> {
    let command = match args[0].to_str() {
        Some("inspect") => Command::Inspect,
        Some("monitor") => Command::Monitor,
        _ => return Err(format!("unknown command {:?}", args[0])),
    };
    let mut options = Options {
        command,
        name: None,
        pid: None,
        start_ticks: None,
        root: OsString::from("/proc"),
        max_processes: NonZeroUsize::new(DEFAULT_MAX_PROCESSES).expect("nonzero"),
        max_entries: NonZeroUsize::new(DEFAULT_MAX_ENTRIES).expect("nonzero"),
        samples: None,
        duration: None,
    };
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let flag = flag.to_str().ok_or("flags must be UTF-8")?;
        let value = rest.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let text = || {
            value
                .to_str()
                .ok_or_else(|| format!("{flag} value must be UTF-8"))
        };
        match flag {
            "--name" => {
                let name = ProcessName::new(text()?.as_bytes()).map_err(|e| e.to_string())?;
                options.name = Some(name);
            }
            "--pid" => options.pid = Some(number(flag, text()?)?),
            "--start-ticks" => options.start_ticks = Some(number(flag, text()?)?),
            "--proc" => options.root = value.clone(),
            "--max-processes" => options.max_processes = number(flag, text()?)?,
            "--max-entries" => options.max_entries = number(flag, text()?)?,
            "--samples" => options.samples = Some(number(flag, text()?)?),
            "--duration" => {
                options.duration = Some(Duration::from_secs(number(flag, text()?)?));
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    match (&options.name, options.pid, options.start_ticks) {
        (Some(_), None, None) | (None, Some(_), _) => {}
        (Some(_), Some(_), _) | (Some(_), None, Some(_)) => {
            return Err("use either --name or --pid, not both".into());
        }
        (None, None, _) => return Err("a --name or --pid target is required".into()),
    }
    scan_budget(&options)?;
    if matches!(options.command, Command::Monitor) {
        if options.pid.is_some() && options.start_ticks.is_none() {
            return Err("monitor --pid also needs --start-ticks (see inspect)".into());
        }
        Budget::new(options.samples, options.duration).map_err(|e| e.to_string())?;
    }
    Ok(options)
}

fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag} value {text:?} is not a valid number"))
}

fn target(options: &Options) -> Option<ProcessTarget> {
    match (&options.name, options.pid, options.start_ticks) {
        (Some(name), _, _) => Some(ProcessTarget::Name(name.clone())),
        (None, Some(pid), Some(start_ticks)) => Some(ProcessTarget::Identity(ProcessIdentity {
            pid,
            start_ticks,
        })),
        _ => None,
    }
}

fn scan_budget(options: &Options) -> Result<ScanBudget, String> {
    ScanBudget::new(options.max_processes, options.max_entries).map_err(|e| e.to_string())
}

fn scope(options: &Options) -> Scope {
    match options.pid {
        Some(pid) => Scope::Pid(pid),
        None => Scope::System,
    }
}

fn inspect(options: &Options) -> Result<(), String> {
    let mut source = ProcfsSource::new(&options.root, scope(options), scan_budget(options)?);
    let records = source.snapshot().map_err(|e| e.to_string())?;
    let target = target(options);
    let mut present = false;
    for record in records {
        let selected = match &target {
            Some(target) => record.matches(target),
            // `--pid` without start ticks: show the record so the caller can pin it.
            None => true,
        };
        if selected {
            present = true;
            println!("{}", describe(record));
        }
    }
    let report = source.report();
    println!(
        "present={present} scope={} visited={} candidates={} records={} vanished={}",
        match source.scope() {
            Scope::System => "system".to_owned(),
            Scope::Pid(pid) => format!("pid:{pid}"),
        },
        report.visited,
        report.candidates,
        report.records,
        report.vanished
    );
    if target.is_none() {
        println!("note: no --start-ticks given; presence above is by PID only");
    }
    Ok(())
}

fn run_monitor(options: &Options) -> Result<(), String> {
    let target = target(options).ok_or("monitor needs a full target")?;
    let budget = Budget::new(options.samples, options.duration).map_err(|e| e.to_string())?;
    let clock = MonotonicClock::new();
    let mut supervisor = Supervisor::new(Duration::ZERO, target.clone(), options.max_processes)
        .map_err(|e| e.to_string())?;
    let mut source = ProcfsSource::new(
        &options.root,
        Scope::for_target(&target),
        scan_budget(options)?,
    );
    println!(
        "monitoring {} (no effects will run)",
        describe_target(&target)
    );
    let stop = monitor::run(
        &mut supervisor,
        &clock,
        &mut source,
        budget,
        &mut ThreadSleep,
        |sample| match sample {
            Sample::Delivered { at, delivery } => println!(
                "+{:>8.3}s id={} {} -> {}",
                at.as_secs_f64(),
                delivery.id.get(),
                describe_event(delivery.event),
                describe_effect(delivery.effect)
            ),
            Sample::Failed { at, error } => {
                println!(
                    "+{:>8.3}s observation failed (counter kept): {error}",
                    at.as_secs_f64()
                );
            }
        },
    )
    .map_err(|e| format!("clock fault: {e}"))?;
    let reason = match stop {
        Stop::SampleBudget => "sample budget reached",
        Stop::DurationBudget => "duration budget reached",
        Stop::Terminal => "supervisor reached terminal state; reboot intent was NOT executed",
    };
    println!("stopped: {reason}; state={:?}", supervisor.state());
    Ok(())
}

fn describe_target(target: &ProcessTarget) -> String {
    match target {
        ProcessTarget::Name(name) => format!("name {}", escape(name.as_bytes())),
        ProcessTarget::Identity(identity) => {
            format!("pid {} start-ticks {}", identity.pid, identity.start_ticks)
        }
    }
}

fn describe(record: &ProcessRecord) -> String {
    let identity = record.identity();
    format!(
        "pid={} start-ticks={} state={} comm={}",
        identity.pid,
        identity.start_ticks,
        char::from(record.state_code()),
        escape(record.comm())
    )
}

fn describe_event(event: WatchdogEvent) -> String {
    match event {
        WatchdogEvent::PresenceChecked {
            present,
            consecutive_missed_ms,
        } => format!("presence present={present} missed={consecutive_missed_ms}ms"),
        WatchdogEvent::CountdownStarted { seconds } => format!("countdown armed from {seconds}s"),
        WatchdogEvent::CountdownTick { seconds } => format!("countdown {seconds}s"),
        WatchdogEvent::RebootRequested => "reboot requested".to_owned(),
        WatchdogEvent::Cancelled => "cancelled".to_owned(),
    }
}

fn describe_effect(effect: Effect) -> &'static str {
    match effect {
        Effect::None => "no effect",
        Effect::ShowCrash => "intent: show crash view (not shown)",
        Effect::SetCountdown { .. } => "intent: update countdown (not shown)",
        Effect::RebootIntent => "intent: reboot (NOT executed)",
        Effect::DismissCrash => "intent: dismiss crash view (not shown)",
    }
}

/// Comm bytes are raw; print them as an escaped ASCII literal.
fn escape(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    out.extend(
        bytes
            .iter()
            .flat_map(|&byte| std::ascii::escape_default(byte).map(char::from)),
    );
    out.push('"');
    out
}

fn plan(args: &[OsString]) -> Result<(), String> {
    let mut rest = args.iter();
    let which = rest
        .next()
        .and_then(|arg| arg.to_str())
        .ok_or("plan needs boot, power-off, restart or reboot-helper")?;
    let mut cmdline = None;
    let mut mini = false;
    while let Some(flag) = rest.next() {
        match flag.to_str() {
            Some("--cmdline") => {
                let path = rest.next().ok_or("--cmdline needs a file")?;
                cmdline = Some(read_cmdline(path)?);
            }
            Some("--mini") => mini = true,
            _ => return Err(format!("unknown plan flag {flag:?}")),
        }
    }
    let kernel = cmdline.as_deref().map(KernelCmdline::new);
    let product = if mini {
        Product::QuadCortexMini
    } else {
        kernel.map_or(Product::QuadCortex, KernelCmdline::hal_product)
    };
    let steps = match which {
        "boot" => {
            let mut dry = DryRun::default();
            let kernel = kernel.unwrap_or(KernelCmdline::new(b""));
            let report =
                boot::run(BootConfig::STOCK, kernel, &mut dry).map_err(|e| e.to_string())?;
            println!("result: {report:?}");
            return Ok(());
        }
        "power-off" => power::power_off(),
        "restart" => power::restart(RestartReason::PowerMenu, product),
        "reboot-helper" => power::reboot_helper(kernel),
        other => return Err(format!("unknown plan {other:?}")),
    };
    for (index, step) in steps.iter().enumerate() {
        println!("{:>2}. {step:?}", index + 1);
    }
    Ok(())
}

fn read_cmdline(path: &OsString) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_CMDLINE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read {path:?}: {e}"))?;
    if bytes.len() as u64 > MAX_CMDLINE_BYTES {
        return Err(format!("{path:?} is larger than {MAX_CMDLINE_BYTES} bytes"));
    }
    Ok(bytes)
}

/// Prints each boot intent instead of running it.
#[derive(Default)]
struct DryRun {
    count: usize,
    mounted: bool,
}

impl BootBackend for DryRun {
    type Error = String;

    fn perform(&mut self, intent: BootIntent) -> Result<Status, String> {
        self.mounted |= matches!(intent, BootIntent::MountExt4 { .. });
        self.count += 1;
        println!("{:>2}. {intent:?}", self.count);
        Ok(Status::Success)
    }

    fn probe(&mut self, probe: Probe) -> Result<bool, String> {
        Ok(match probe {
            Probe::IsFile(boot::SPLASH_START_UI) => true,
            Probe::Mounted(_) => self.mounted,
            _ => false,
        })
    }

    fn boot_env(&mut self, _: &'static str) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    fn read_file(&mut self, path: &'static str) -> Result<Vec<u8>, String> {
        Err(format!("the dry run does not read {path}"))
    }

    fn list_dir(&mut self, _: &'static str) -> Result<Vec<OsString>, String> {
        Ok(Vec::new())
    }
}

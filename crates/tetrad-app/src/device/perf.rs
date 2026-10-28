//! Loop statistics for finding what costs CPU and frame time on the unit. Diagnostics only; no
//! logic reads them. Built only with the `dev` feature. The counters always run (a few additions
//! per pass); a line goes to the log every [`WINDOW`] only after [`enable`], which `tetrad run
//! --dev` calls (the "Tetrad dev" boot entry):
//!
//! ```text
//! perf 10.0s: cpu 23.4% steps 940/s renders 12.3/s render 5.1/21ms present 1.9/8ms step max 40ms awake 9.8% [gather 31 reduce 12 apply 0 control 4 views 9 frame 64]
//! ```
//!
//! `cpu` is the CPU time this process used (all threads) over the window's wall time. `render`
//! and `present` are the mean and the slowest time of the two parts of a frame. `awake` is the
//! share of the window that the loop spent in a pass, not in its idle sleep; the brackets split
//! that time by phase in milliseconds per second.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::PhaseTimes;

/// How often a line is due.
pub const WINDOW: Duration = Duration::from_secs(10);
/// The unit of the times in `/proc/self/stat`: `USER_HZ`, 100 on Linux.
const USER_HZ: f64 = 100.0;
/// Whether the log line is on.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns the log line on for the rest of the run.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Time spent in `Ui::sync` (the snapshot rebuild that each input and each frame pays).
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncTimes {
    pub calls: u32,
    pub total: Duration,
    /// Building the snapshot from the device state.
    pub snapshot: Duration,
    /// Comparing it with the one the view has.
    pub compare: Duration,
    /// Handing a changed snapshot to the view.
    pub set: Duration,
}

impl SyncTimes {
    fn add(&mut self, other: Self) {
        self.calls += other.calls;
        self.total += other.total;
        self.snapshot += other.snapshot;
        self.compare += other.compare;
        self.set += other.set;
    }
}

#[derive(Debug, Default)]
pub struct Perf {
    sync: SyncTimes,
    /// The costliest units of the slowest pass of the window.
    slowest: String,
    since: Duration,
    steps: u32,
    step_max: Duration,
    awake: Duration,
    phases: [Duration; 6],
    renders: u32,
    render: Duration,
    render_max: Duration,
    present: Duration,
    present_max: Duration,
    /// Process jiffies (utime + stime) at the start of the window.
    cpu: Option<u64>,
}

impl Perf {
    /// Sync time since the last call, from the view bridge.
    pub fn sync(&mut self, times: SyncTimes) {
        self.sync.add(times);
    }

    /// One loop pass that took `took`, not counting the idle sleep, with its phase times.
    /// `units` names the costliest units of the pass; it is only called for a new slowest pass.
    pub fn step(&mut self, took: Duration, phases: PhaseTimes, units: impl FnOnce() -> String) {
        self.steps += 1;
        if took > self.step_max {
            self.slowest = units();
        }
        self.step_max = self.step_max.max(took);
        self.awake += took;
        let PhaseTimes {
            gather,
            reduce,
            apply,
            control,
            views,
            frame,
        } = phases;
        for (sum, time) in self
            .phases
            .iter_mut()
            .zip([gather, reduce, apply, control, views, frame])
        {
            *sum += time;
        }
    }

    /// One frame: the time to render it and the time to write it to the display.
    pub fn frame(&mut self, render: Duration, present: Duration) {
        self.renders += 1;
        self.render += render;
        self.render_max = self.render_max.max(render);
        self.present += present;
        self.present_max = self.present_max.max(present);
    }

    /// The log line when a window has ended and the line is [`enable`]d. A window always restarts.
    pub fn report(&mut self, now: Duration) -> Option<String> {
        let elapsed = now.saturating_sub(self.since);
        if elapsed < WINDOW {
            return None;
        }
        let window = std::mem::replace(
            self,
            Self {
                since: now,
                ..Self::default()
            },
        );
        self.cpu = read_cpu();
        if !ENABLED.load(Ordering::Relaxed) {
            return None;
        }
        let seconds = elapsed.as_secs_f64();
        let cpu = match (window.cpu, self.cpu, Some(USER_HZ)) {
            (Some(a), Some(b), Some(hz)) => {
                let used = b.saturating_sub(a) as f64 / hz;
                format!("cpu {:.1}%", used * 100.0 / seconds)
            }
            _ => "cpu n/a".to_owned(),
        };
        let per_second = |time: Duration| time.as_secs_f64() * 1000.0 / seconds;
        let [gather, reduce, apply, control, views, frame] = window.phases.map(per_second);
        let [sync, snapshot, compare, set] = [
            window.sync.total,
            window.sync.snapshot,
            window.sync.compare,
            window.sync.set,
        ]
        .map(per_second);
        let mean = |sum: Duration| {
            if window.renders == 0 {
                0.0
            } else {
                sum.as_secs_f64() * 1000.0 / f64::from(window.renders)
            }
        };
        Some(format!(
            "perf {seconds:.1}s: {cpu} steps {:.0}/s renders {:.1}/s render {:.1}/{}ms \
             present {:.1}/{}ms step max {}ms awake {:.1}% [gather {gather:.0} reduce {reduce:.0} \
             apply {apply:.0} control {control:.0} views {views:.0} frame {frame:.0}] \
             sync {}x {sync:.0}ms/s (snapshot {snapshot:.0} compare {compare:.0} set {set:.0}) \
             slowest [{}]",
            f64::from(window.steps) / seconds,
            f64::from(window.renders) / seconds,
            mean(window.render),
            window.render_max.as_millis(),
            mean(window.present),
            window.present_max.as_millis(),
            window.step_max.as_millis(),
            per_second(window.awake) / 10.0,
            window.sync.calls,
            window.slowest,
        ))
    }
}

fn read_cpu() -> Option<u64> {
    parse_cpu(&std::fs::read_to_string("/proc/self/stat").ok()?)
}

/// utime plus stime (fields 14 and 15, after the parenthesised name) of `/proc/self/stat`.
fn parse_cpu(process: &str) -> Option<u64> {
    let after_name = &process[process.rfind(')')? + 1..];
    let mut fields = after_name.split_ascii_whitespace().skip(11);
    Some(fields.next()?.parse::<u64>().ok()? + fields.next()?.parse::<u64>().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_is_read_from_proc_text() {
        // A name with a space and a parenthesis must not shift the fields.
        let process = "42 (te (trad) x) S 1 42 42 0 -1 4194560 100 0 0 0 700 300 0 0 20 0 5";
        assert_eq!(parse_cpu(process), Some(1000));
        assert_eq!(parse_cpu("garbage"), None);
        assert_eq!(parse_cpu("1 (x) S 1"), None);
    }

    #[test]
    fn a_window_reports_only_when_it_ends() {
        let mut perf = Perf::default();
        perf.step(Duration::from_millis(3), PhaseTimes::default(), String::new);
        perf.frame(Duration::from_millis(5), Duration::from_millis(2));
        assert_eq!(perf.report(Duration::from_secs(1)), None);
        // The line is off by default, so the window restarts without a line.
        assert_eq!(perf.report(WINDOW), None);
        assert_eq!(perf.steps, 0);
        assert_eq!(perf.since, WINDOW);
    }
}

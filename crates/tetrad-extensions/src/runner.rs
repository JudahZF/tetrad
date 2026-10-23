//! The child-process side: one QuickJS engine on one thread, driven by host frames.
//!
//! The runner has no file, network, process or timer API. JavaScript reaches the host only
//! through the prelude's `call`, `log` and `done` natives, and every call is a host frame.
//! Each JavaScript entry (a dispatch, a response, a job pump) runs under a slice deadline
//! enforced by the QuickJS interrupt handler. Activation and each event handler also have an
//! elapsed-time deadline; a stuck promise faults the runner. A [`Event::BuildBlock`] handler
//! (parse an asset, build IR) has longer budgets than other events.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use rquickjs::promise::PromiseState;
use rquickjs::{
    CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Module, Object, Persistent,
    Promise, Runtime, Value,
};
use serde::Serialize;
use serde_json::Value as Json;
use serde_json::value::RawValue;

use crate::protocol::{
    self, CallResult, Event, ExtensionInfo, HostMessage, LogLevel, PROTOCOL_VERSION, Registrations,
    RunnerMessage,
};

const PRELUDE: &str = include_str!("prelude.js");
/// Log lines one JavaScript entry may send before the rest are dropped.
const MAX_LOGS: usize = 32;

/// A frame for the host: a message, or a request line already encoded by the `call` native.
enum Outgoing {
    Message(RunnerMessage),
    Line(Vec<u8>),
}

/// The wire form of [`RunnerMessage::Request`] with borrowed raw params, so a large call
/// (block IR) is copied once instead of parsed into a value tree.
#[derive(Serialize)]
#[serde(tag = "type", rename = "request")]
struct RequestFrame<'a> {
    id: u64,
    scope: Option<u64>,
    method: &'a str,
    params: &'a RawValue,
}

/// Runner budgets. Trial values for the device; tune from ARM measurements.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// QuickJS heap limit in bytes.
    pub memory: usize,
    /// QuickJS stack limit in bytes.
    pub stack: usize,
    /// Longest synchronous JavaScript run per entry, on [`Limits::slice_clock`].
    pub slice: Duration,
    /// The clock of the slice. The device runner passes the thread's CPU time, so a runner
    /// that waits for the CPU (low priority on a busy unit) is not faulted for the wait.
    pub slice_clock: fn() -> Duration,
    /// Longest time from an event's dispatch to its handler settling.
    pub handler: Duration,
    /// [`Limits::slice`] while an [`Event::BuildBlock`] handler runs.
    pub build_slice: Duration,
    /// [`Limits::handler`] of an [`Event::BuildBlock`].
    pub build_handler: Duration,
    /// Longest activation, including its awaited host calls.
    pub activation: Duration,
    /// Most host calls in flight.
    pub pending_calls: usize,
    /// Most events queued behind the running handler.
    pub queued_events: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // An asset and the IR built from it are a few MiB each. The device process also has
            // a 128 MiB address-space limit.
            memory: 32 * 1024 * 1024,
            stack: 256 * 1024,
            slice: Duration::from_millis(50),
            slice_clock: monotonic,
            handler: Duration::from_secs(1),
            build_slice: Duration::from_secs(2),
            build_handler: Duration::from_secs(5),
            activation: Duration::from_secs(3),
            pending_calls: 32,
            queued_events: 256,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("host closed the connection")]
    Disconnected,
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("{0}")]
    Fault(String),
    #[error("write to host: {0}")]
    Io(#[from] std::io::Error),
}

/// Runs one extension module until the host sends `Stop` or closes stdin. A fault is sent to
/// the host as [`RunnerMessage::Fault`] before this returns the error.
pub fn run(
    source: &str,
    input: impl BufRead + Send + 'static,
    output: &mut impl Write,
    limits: Limits,
) -> Result<(), RunnerError> {
    let frames = spawn_reader(input);
    let mut runner = Runner::new(limits, output)?;
    let result = runner.main(source, &frames);
    if let Err(RunnerError::Fault(message) | RunnerError::Protocol(message)) = &result {
        runner.send(&RunnerMessage::Fault {
            message: message.clone(),
        })?;
    }
    result
}

/// Host frames, decoded on their own thread so the engine thread can wait with a deadline.
fn spawn_reader(mut input: impl BufRead + Send + 'static) -> Receiver<Result<HostMessage, String>> {
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new().stack_size(64 * 1024);
    let spawned = reader.spawn(move || {
        let mut line = Vec::new();
        loop {
            line.clear();
            // Bound the read: a line over the frame limit is a protocol error, not memory use.
            let read = std::io::Read::take(&mut input, protocol::MAX_FRAME as u64 + 2)
                .read_until(b'\n', &mut line);
            match read {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            } else if line.len() > protocol::MAX_FRAME {
                let _ = tx.send(Err("host frame over the size limit".into()));
                return;
            }
            let message = protocol::decode(&line).map_err(|e| e.to_string());
            let failed = message.is_err();
            if tx.send(message).is_err() || failed {
                return;
            }
        }
    });
    // Without a reader the channel closes at once and the runner sees a disconnected host.
    drop(spawned);
    rx
}

/// State the JavaScript natives share with the engine loop.
#[derive(Default)]
struct Shared {
    outbox: Vec<Outgoing>,
    /// Log lines since the last flush; past [`MAX_LOGS`] they are dropped.
    logged: usize,
    /// Resolve and reject functions of calls in flight, by request ID.
    pending: HashMap<u64, (Persistent<Function<'static>>, Persistent<Function<'static>>)>,
    next_id: u64,
    /// Settled handler scopes, with an error text when the handler threw and the JSON text
    /// of a USB handler's return value.
    done: Vec<(u64, Option<String>, Option<String>)>,
}

/// Wall-clock time since the first call: the default [`Limits::slice_clock`].
fn monotonic() -> Duration {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// The slice deadline, read by the interrupt handler on every QuickJS poll.
#[derive(Clone)]
struct Deadline {
    clock: fn() -> Duration,
    /// Nanoseconds on `clock`; `u64::MAX` means none.
    at: Arc<AtomicU64>,
    hit: Arc<AtomicBool>,
}

impl Deadline {
    fn new(clock: fn() -> Duration) -> Self {
        Self {
            clock,
            at: Arc::new(AtomicU64::new(u64::MAX)),
            hit: Arc::new(AtomicBool::new(false)),
        }
    }

    fn arm(&self, slice: Duration) {
        let at = (self.clock)() + slice;
        self.hit.store(false, Ordering::Relaxed);
        self.at.store(
            u64::try_from(at.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn disarm(&self) {
        self.at.store(u64::MAX, Ordering::Relaxed);
    }

    fn expired(&self) -> bool {
        let at = self.at.load(Ordering::Relaxed);
        let expired =
            at != u64::MAX && u64::try_from((self.clock)().as_nanos()).unwrap_or(u64::MAX) >= at;
        if expired {
            self.hit.store(true, Ordering::Relaxed);
        }
        expired
    }

    fn was_hit(&self) -> bool {
        self.hit.load(Ordering::Relaxed)
    }
}

enum Phase {
    WaitingForActivate,
    Activating {
        promise: Persistent<Promise<'static>>,
        until: Instant,
    },
    Running {
        busy: Option<Busy>,
        queue: VecDeque<(u64, Event)>,
    },
}

/// The event whose handler runs now.
#[derive(Clone, Copy)]
struct Busy {
    scope: u64,
    until: Instant,
    /// The slice budget of its JavaScript entries.
    slice: Duration,
}

struct Runner<'w, W: Write> {
    runtime: Runtime,
    context: Context,
    shared: Rc<RefCell<Shared>>,
    deadline: Deadline,
    limits: Limits,
    output: &'w mut W,
    phase: Phase,
}

impl<'w, W: Write> Runner<'w, W> {
    fn new(limits: Limits, output: &'w mut W) -> Result<Self, RunnerError> {
        let fault = |e: rquickjs::Error| RunnerError::Fault(format!("engine start: {e}"));
        let runtime = Runtime::new().map_err(fault)?;
        runtime.set_memory_limit(limits.memory);
        runtime.set_max_stack_size(limits.stack);
        let deadline = Deadline::new(limits.slice_clock);
        let interrupt = deadline.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || interrupt.expired())));
        let context = Context::full(&runtime).map_err(fault)?;
        Ok(Self {
            runtime,
            context,
            shared: Rc::default(),
            deadline,
            limits,
            output,
            phase: Phase::WaitingForActivate,
        })
    }

    fn send(&mut self, message: &RunnerMessage) -> Result<(), RunnerError> {
        let line = protocol::encode(message)
            .map_err(|e| RunnerError::Protocol(format!("outgoing frame: {e}")))?;
        self.output.write_all(&line)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), RunnerError> {
        let outbox = {
            let mut shared = self.shared.borrow_mut();
            shared.logged = 0;
            std::mem::take(&mut shared.outbox)
        };
        for outgoing in &outbox {
            match outgoing {
                Outgoing::Message(message) => self.send(message)?,
                Outgoing::Line(line) => self.output.write_all(line)?,
            }
        }
        self.output.flush()?;
        Ok(())
    }

    fn main(
        &mut self,
        source: &str,
        frames: &Receiver<Result<HostMessage, String>>,
    ) -> Result<(), RunnerError> {
        loop {
            self.flush()?;
            let wait = match &self.phase {
                Phase::WaitingForActivate => None,
                Phase::Activating { until, .. } => Some(*until),
                Phase::Running { busy, .. } => busy.map(|busy| busy.until),
            };
            let message = match wait {
                None => frames.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(until) => frames.recv_timeout(until.saturating_duration_since(Instant::now())),
            };
            match message {
                Ok(Ok(message)) => {
                    if !self.on_message(source, message)? {
                        return Ok(());
                    }
                }
                Ok(Err(error)) => return Err(RunnerError::Protocol(error)),
                Err(RecvTimeoutError::Disconnected) => return Err(RunnerError::Disconnected),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.advance()?;
        }
    }

    /// Returns `false` after `Stop`.
    fn on_message(&mut self, source: &str, message: HostMessage) -> Result<bool, RunnerError> {
        match message {
            HostMessage::Activate {
                protocol,
                extension,
                config,
            } => {
                if !matches!(self.phase, Phase::WaitingForActivate) {
                    return Err(RunnerError::Protocol("second Activate".into()));
                }
                if protocol != PROTOCOL_VERSION {
                    return Err(RunnerError::Protocol(format!(
                        "host protocol {protocol}, runner protocol {PROTOCOL_VERSION}"
                    )));
                }
                self.activate(source, &extension, &config)?;
            }
            HostMessage::Event { scope, event } => match &mut self.phase {
                Phase::Running { queue, .. } => {
                    if queue.len() >= self.limits.queued_events {
                        return Err(RunnerError::Fault("event queue overflow".into()));
                    }
                    queue.push_back((scope, event));
                }
                _ => return Err(RunnerError::Protocol("event before activation".into())),
            },
            HostMessage::Response { id, result } => self.respond(id, result)?,
            HostMessage::Stop => {
                if matches!(self.phase, Phase::Running { .. }) {
                    // Best effort: a throwing disposer does not block the exit.
                    let _ = self.enter(|ctx| {
                        let dispose: Function = ctx.globals().get("__tetrad_dispose")?;
                        dispose.call::<_, ()>(())
                    });
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn activate(
        &mut self,
        source: &str,
        extension: &ExtensionInfo,
        config: &Json,
    ) -> Result<(), RunnerError> {
        let shared = self.shared.clone();
        let limit = self.limits.pending_calls;
        let extension = serde_json::to_string(extension).unwrap_or_default();
        let config = serde_json::to_string(config).unwrap_or_default();
        let promise = self.enter(|ctx| {
            install_natives(&ctx, shared, limit)?;
            ctx.eval::<(), _>(PRELUDE)?;
            // No module loader is installed, so the bundle cannot import anything at run time.
            let (module, evaluated) = Module::declare(ctx.clone(), "extension", source)?.eval()?;
            evaluated.finish::<()>()?;
            let definition: Value = module.namespace()?.get("default")?;
            let activate: Function = ctx.globals().get("__tetrad_activate")?;
            let promise: Promise = activate.call((definition, extension, config))?;
            Ok(Persistent::save(&ctx, promise))
        })?;
        self.phase = Phase::Activating {
            promise,
            until: Instant::now() + self.limits.activation,
        };
        Ok(())
    }

    fn respond(&mut self, id: u64, result: CallResult) -> Result<(), RunnerError> {
        let Some((resolve, reject)) = self.shared.borrow_mut().pending.remove(&id) else {
            return Err(RunnerError::Protocol(format!(
                "response to unknown request {id}"
            )));
        };
        self.enter(|ctx| {
            match result {
                CallResult::Ok(value) => {
                    let json = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
                    resolve.restore(&ctx)?.call::<_, ()>((json,))?;
                }
                CallResult::Error(error) => {
                    let exception = Exception::from_message(ctx.clone(), &error.message)?;
                    let code = serde_json::to_value(error.code)
                        .ok()
                        .and_then(|c| c.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    exception.set("code", code)?;
                    reject.restore(&ctx)?.call::<_, ()>((exception,))?;
                }
            }
            Ok(())
        })
    }

    /// Settles activation, finishes handlers and starts the next queued event.
    fn advance(&mut self) -> Result<(), RunnerError> {
        self.pump()?;
        let now = Instant::now();
        if let Phase::Activating { promise, until } = &self.phase {
            let promise = promise.clone();
            let until = *until;
            let settled = self.enter(|ctx| {
                let promise = promise.restore(&ctx)?;
                Ok(match promise.state() {
                    PromiseState::Pending => None,
                    PromiseState::Resolved => Some(Ok(promise.result::<String>().transpose()?)),
                    PromiseState::Rejected => Some(Err(describe_rejection(&ctx, &promise))),
                })
            })?;
            match settled {
                None if now >= until => {
                    return Err(RunnerError::Fault(
                        "activation did not finish in time".into(),
                    ));
                }
                None => return Ok(()),
                Some(Err(error)) => {
                    return Err(RunnerError::Fault(format!("activation failed: {error}")));
                }
                Some(Ok(json)) => {
                    let registrations: Registrations =
                        serde_json::from_str(json.as_deref().unwrap_or("{}"))
                            .map_err(|e| RunnerError::Fault(format!("registrations: {e}")))?;
                    self.shared.borrow_mut().outbox.push(Outgoing::Message(
                        RunnerMessage::Activated { registrations },
                    ));
                    self.phase = Phase::Running {
                        busy: None,
                        queue: VecDeque::new(),
                    };
                }
            }
        }
        loop {
            let Phase::Running { busy, queue } = &mut self.phase else {
                return Ok(());
            };
            let done = std::mem::take(&mut self.shared.borrow_mut().done);
            for (scope, mut error, result) in done {
                if busy.is_some_and(|busy| busy.scope == scope) {
                    *busy = None;
                }
                let result = result.and_then(|json| {
                    serde_json::from_str(&json)
                        .inspect_err(|e| error = Some(format!("USB reply is not JSON: {e}")))
                        .ok()
                });
                self.shared
                    .borrow_mut()
                    .outbox
                    .push(Outgoing::Message(RunnerMessage::Done {
                        scope,
                        error,
                        result,
                    }));
            }
            if let Some(Busy { scope, until, .. }) = *busy {
                if Instant::now() >= until {
                    return Err(RunnerError::Fault(format!(
                        "handler for scope {scope} did not finish in time"
                    )));
                }
                return Ok(());
            }
            let Some((scope, event)) = queue.pop_front() else {
                return Ok(());
            };
            let (handler, slice) = match event {
                Event::BuildBlock { .. } => (self.limits.build_handler, self.limits.build_slice),
                _ => (self.limits.handler, self.limits.slice),
            };
            *busy = Some(Busy {
                scope,
                until: Instant::now() + handler,
                slice,
            });
            let event = serde_json::to_string(&event).unwrap_or_default();
            self.enter(|ctx| {
                let dispatch: Function = ctx.globals().get("__tetrad_dispatch")?;
                dispatch.call::<_, ()>((scope as f64, event))
            })?;
            self.pump()?;
        }
    }

    /// The slice budget of the next JavaScript entry: the running handler's, else the default.
    fn slice(&self) -> Duration {
        match &self.phase {
            Phase::Running {
                busy: Some(busy), ..
            } => busy.slice,
            _ => self.limits.slice,
        }
    }

    /// Runs queued promise jobs under one slice deadline.
    fn pump(&mut self) -> Result<(), RunnerError> {
        self.deadline.arm(self.slice());
        let result = loop {
            match self.runtime.execute_pending_job() {
                Ok(false) => break Ok(()),
                Ok(true) => {}
                Err(_) if self.deadline.was_hit() => {
                    break Err(RunnerError::Fault("execution slice exceeded".into()));
                }
                // A job that throws rejects its own promise; the handler reports it.
                Err(_) => {}
            }
        };
        self.deadline.disarm();
        result
    }

    /// One JavaScript entry under the slice deadline. An uncaught exception is a fault.
    fn enter<T>(
        &mut self,
        f: impl for<'js> FnOnce(Ctx<'js>) -> rquickjs::Result<T>,
    ) -> Result<T, RunnerError> {
        self.deadline.arm(self.slice());
        let result = self
            .context
            .with(|ctx| f(ctx.clone()).catch(&ctx).map_err(|error| describe(&error)));
        let hit = self.deadline.was_hit();
        self.deadline.disarm();
        result.map_err(|error| {
            RunnerError::Fault(if hit {
                "execution slice exceeded".into()
            } else {
                error
            })
        })
    }
}

impl<W: Write> Drop for Runner<'_, W> {
    /// Persistent handles keep engine objects alive; QuickJS aborts if the runtime is freed
    /// with live objects, so release them first.
    fn drop(&mut self) {
        let mut shared = self.shared.borrow_mut();
        shared.pending.clear();
        shared.done.clear();
        drop(shared);
        self.phase = Phase::WaitingForActivate;
        self.runtime.run_gc();
    }
}

fn install_natives<'js>(
    ctx: &Ctx<'js>,
    shared: Rc<RefCell<Shared>>,
    pending_limit: usize,
) -> rquickjs::Result<()> {
    let native = Object::new(ctx.clone())?;
    let calls = shared.clone();
    native.set(
        "call",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, method: String, scope: Option<f64>, params: String| {
                let (promise, resolve, reject) = ctx.promise()?;
                let mut shared = calls.borrow_mut();
                let id = shared.next_id + 1;
                // A raw newline would end the frame early; JSON.stringify never writes one.
                let line = RawValue::from_string(params)
                    .ok()
                    .filter(|params| !params.get().contains('\n'))
                    .ok_or("parameters are not single-line JSON")
                    .and_then(|params| {
                        protocol::encode(&RequestFrame {
                            id,
                            scope: scope.map(|s| s as u64),
                            method: &method,
                            params: &params,
                        })
                        .map_err(|_| "parameters are over the frame size limit")
                    });
                let line = match line {
                    Ok(line) if shared.pending.len() < pending_limit => line,
                    failed => {
                        let message = failed.err().unwrap_or("too many host calls in flight");
                        let exception = Exception::from_message(ctx.clone(), message)?;
                        exception.set("code", "limitExceeded")?;
                        reject.call::<_, ()>((exception,))?;
                        return Ok::<_, rquickjs::Error>(promise);
                    }
                };
                shared.next_id = id;
                shared.pending.insert(
                    id,
                    (
                        Persistent::save(&ctx, resolve),
                        Persistent::save(&ctx, reject),
                    ),
                );
                shared.outbox.push(Outgoing::Line(line));
                Ok(promise)
            },
        )?,
    )?;
    let logs = shared.clone();
    native.set(
        "log",
        Function::new(ctx.clone(), move |level: String, text: String| {
            let level = match level.as_str() {
                "warn" => LogLevel::Warn,
                "error" => LogLevel::Error,
                _ => LogLevel::Info,
            };
            let mut shared = logs.borrow_mut();
            shared.logged += 1;
            // A log call in a hot loop must not flood the host: one marker replaces the rest.
            let message = match shared.logged {
                n if n <= MAX_LOGS => RunnerMessage::Log {
                    level,
                    text: text.chars().take(1024).collect(),
                },
                n if n == MAX_LOGS + 1 => RunnerMessage::Log {
                    level: LogLevel::Warn,
                    text: "more log lines were dropped".into(),
                },
                _ => return,
            };
            shared.outbox.push(Outgoing::Message(message));
        })?,
    )?;
    native.set(
        "done",
        Function::new(
            ctx.clone(),
            move |scope: f64, error: Option<String>, result: Option<String>| {
                shared.borrow_mut().done.push((scope as u64, error, result));
            },
        )?,
    )?;
    ctx.globals().set("__tetrad_native", native)
}

fn describe(error: &CaughtError<'_>) -> String {
    match error {
        CaughtError::Exception(exception) => {
            let message = exception.message().unwrap_or_default();
            match exception.stack() {
                Some(stack) if !stack.is_empty() => format!("{message}\n{stack}"),
                _ => message,
            }
        }
        // QuickJS throws `null` when an allocation fails at the heap limit.
        CaughtError::Value(value) if value.is_null() => {
            "out of memory (the engine threw null)".into()
        }
        CaughtError::Value(value) => format!("{value:?}"),
        CaughtError::Error(error) => error.to_string(),
    }
}

fn describe_rejection<'js>(ctx: &Ctx<'js>, promise: &Promise<'js>) -> String {
    match promise.result::<Value>() {
        Some(Err(rquickjs::Error::Exception)) => {
            describe(&CaughtError::from_error(ctx, rquickjs::Error::Exception))
        }
        Some(Err(error)) => error.to_string(),
        _ => "rejected".into(),
    }
}

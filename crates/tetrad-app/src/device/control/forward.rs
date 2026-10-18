//! Cortex Control as the device's HTTP transport (stock `CloudUpdaterUSB::sendRequest`
//! `0x44a928` and the Product, Backups and Logs callers). A call on the service worker
//! becomes one forward message (29, 30, 31 or 61); Control does the HTTP and answers with
//! UPDATE responses that carry the body.
//!
//! - IDs: one 64-bit counter for every forward (`0x155a28`; the first ID is 1, I).
//! - Senders `0x1c0188` (61) and `0x156a28` (29): action UPDATE (I: the same message layout
//!   as the other stock senders), the ID in field 3, the request half with the timeout in ms
//!   (updater: check and header 60 s, changelog 120 s, package 30 min). The Updater
//!   `payload` is a string and goes out only when it is not empty.
//! - Registration `0x1bf000`: the call waits until now + timeout. The receivers (`0x155a6c`,
//!   `0x147dc8`, `0x1893ec`, `0x1beba4`) hand each chunk to the call and drop the call after
//!   its last chunk; a nonzero `error_code` on the last chunk fails it.
//! - Stock sends nothing while Control is not connected (`0x156a28` tests `+4`) and the call
//!   then waits for its deadline. Here it fails at once, as a disconnect fails the open
//!   calls (O).
//! - The forward carries the request headers (bearer, client signature) to Control, as
//!   stock does. They are never logged.
//! - Owned bound (review r21 H8): stock writes each chunk on the receiving thread, so a slow
//!   write holds Control back. Here the worker writes, so a call may hold at most
//!   `QUEUED_LIMIT` (64 MiB) bytes that its worker has not taken; a chunk past it fails the call.
//!   The bridge never blocks (a blocking send would stall every Control message).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use tetrad_control::codecs::{
    BackupsForwardMessage, BackupsForwardRequest, ControlMessage, CortexMessageType, EnumValue,
    HttpOperation, LogsForwardMessage, LogsForwardRequest, MessageAction, ProductForwardHeaderPair,
    ProductForwardMessage, ProductForwardRequest, UpdaterForwardMessage, UpdaterForwardRequest,
};
use tetrad_services::exec::Files;
use tetrad_services::http::{HttpClient, HttpError, HttpRequest, HttpResponse, Method, Route};

/// How often a waiting call checks its cancel token.
const CANCEL_POLL: Duration = Duration::from_millis(100);
/// The status of a forward whose last chunk has `error_code` 0 or none (I).
const FORWARDED_OK: u16 = 200;
/// The bytes one call may hold in its chunk queue (O): four 16 MiB product payloads.
const QUEUED_LIMIT: usize = 64 * 1024 * 1024;
/// `0x1458ac`: a backup body goes to Control in chunks of at most this many bytes (rest
/// > 149 999 → 150 000).
const BACKUP_CHUNK: usize = 150_000;

struct Call {
    request: HttpRequest,
    chunks: Sender<Chunk>,
    /// Bytes sent to the call and not yet taken by its worker.
    queued: Arc<AtomicUsize>,
}

enum Chunk {
    /// `status` is the HTTP status Control reported with the last chunk.
    Data {
        payload: Vec<u8>,
        last: bool,
        status: Option<u16>,
    },
    Failed(HttpError),
}

/// Posts calls to the Control bridge from any thread ([`super::Control::forward_link`]).
#[derive(Clone)]
pub struct ForwardLink(Sender<Call>);

/// The worker half: an [`HttpClient`] whose calls go out through the Control bridge
/// ([`Forwards`]). `download` writes the chunks into `dest` as they arrive (`files`).
pub struct ForwardHttp<F> {
    files: F,
    link: ForwardLink,
}

impl<F: Files> ForwardHttp<F> {
    pub fn new(link: ForwardLink, files: F) -> Self {
        Self { files, link }
    }

    /// Posts `request` and hands each chunk to `sink` until the last one.
    /// Returns the HTTP status of the answer.
    fn exchange(
        &mut self,
        request: &HttpRequest,
        mut sink: impl FnMut(&mut F, &[u8]) -> Result<(), HttpError>,
    ) -> Result<u16, HttpError> {
        let (chunks, answers) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        self.link
            .0
            .send(Call {
                request: request.clone(),
                chunks,
                queued: Arc::clone(&queued),
            })
            .map_err(|_| gone())?;
        loop {
            if request
                .cancel
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
            {
                return Err(HttpError::Cancelled);
            }
            match answers.recv_timeout(CANCEL_POLL) {
                Ok(Chunk::Data {
                    payload,
                    last,
                    status,
                }) => {
                    queued.fetch_sub(payload.len(), Ordering::Relaxed);
                    sink(&mut self.files, &payload)?;
                    if last {
                        return Ok(status.unwrap_or(FORWARDED_OK));
                    }
                }
                Ok(Chunk::Failed(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(gone()),
            }
        }
    }
}

fn gone() -> HttpError {
    HttpError::Transport("the Cortex Control bridge is gone".into())
}

fn file_error(error: impl std::fmt::Display) -> HttpError {
    HttpError::Transport(format!("writing a forwarded body: {error}"))
}

impl<F: Files> HttpClient for ForwardHttp<F> {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut body = Vec::new();
        let status = self.exchange(request, |_, chunk| {
            body.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(HttpResponse { status, body })
    }

    /// The package goes straight into `dest`, chunk by chunk (no tmp file, as stock).
    fn download(&mut self, request: &HttpRequest, dest: &Path) -> Result<u16, HttpError> {
        self.files.write(dest, &[]).map_err(file_error)?;
        self.exchange(request, |files, chunk| {
            files.append(dest, chunk).map_err(file_error)
        })
    }
}

/// The key of an open forward: its message type and forward ID.
pub(super) type ForwardKey = (CortexMessageType, u64);

/// A call that waits for Control's answer.
struct Open {
    chunks: Sender<Chunk>,
    queued: Arc<AtomicUsize>,
    /// Now + the timeout at the last message sent (`0x1bf000` registers each send).
    due: Duration,
    /// A backup body with chunks still to send, and the bytes sent so far.
    body: Option<(HttpRequest, usize)>,
}

/// The bridge half: posted calls become forward messages; Control's answers go back to
/// the waiting call.
pub struct Forwards {
    posted: Receiver<Call>,
    post: Sender<Call>,
    /// `0x155a28`: the last forward ID.
    last_id: u64,
    open: BTreeMap<(CortexMessageType, u64), Open>,
    /// [`QUEUED_LIMIT`]; tests lower it.
    queued_limit: usize,
}

impl Default for Forwards {
    fn default() -> Self {
        let (post, posted) = mpsc::channel();
        Self {
            posted,
            post,
            last_id: 0,
            open: BTreeMap::new(),
            queued_limit: QUEUED_LIMIT,
        }
    }
}

impl Forwards {
    pub(super) fn link(&self) -> ForwardLink {
        ForwardLink(self.post.clone())
    }

    /// The messages to send in this tick: the next chunk of each backup body under way, then
    /// one message per call posted since the last take (a backup body: its first chunk).
    /// `0x1458ac` sends one chunk and starts the next from a single-shot `QTimer` (`+8`,
    /// interval 0), so one per event-loop pass (C); here one per tick (review-r45 M1). A call
    /// waits until `now` + its timeout from its last message.
    pub(super) fn take(&mut self, now: Duration) -> Vec<(ForwardKey, ControlMessage)> {
        let mut messages = Vec::new();
        for (&key, open) in &mut self.open {
            let Some((request, sent)) = open.body.as_mut() else {
                continue;
            };
            let (message, next) = forward_message(request, key.1, *sent);
            open.due = now + request.timeout;
            match next {
                Some(next) => *sent = next,
                None => open.body = None,
            }
            messages.push((key, message));
        }
        while let Ok(call) = self.posted.try_recv() {
            self.last_id += 1;
            let id = self.last_id;
            let (message, next) = forward_message(&call.request, id, 0);
            let key = (message.message_type(), id);
            self.open.insert(
                key,
                Open {
                    chunks: call.chunks,
                    queued: call.queued,
                    due: now + call.request.timeout,
                    body: next.map(|next| (call.request, next)),
                },
            );
            messages.push((key, message));
        }
        messages
    }

    /// The call of `key` fails at once: its forward was not sent (review-r45 M4, M5).
    pub(super) fn fail(&mut self, key: &ForwardKey, reason: String) {
        if let Some(open) = self.open.remove(key) {
            _ = open
                .chunks
                .send(Chunk::Failed(HttpError::Transport(reason)));
        }
    }

    /// Calls posted while Control may not carry them (not connected, a bring-up refusal)
    /// fail at once with `reason`.
    pub(super) fn refuse_posted(&mut self, reason: &str) {
        while let Ok(call) = self.posted.try_recv() {
            _ = (call.chunks).send(Chunk::Failed(HttpError::Transport(reason.into())));
        }
    }

    /// One response chunk from the link (`DeviceModel::forward_response`). The `error_code` of
    /// the last chunk is the HTTP status of the forwarded call: stock's reply invoker
    /// `0x810694` builds the response with it as the status (codes 4 and 5 take its failure
    /// paths), and Cortex Control 4.1.0 answers a good upload with 200 (QC, 2026-10-17). 0 is
    /// a success too (I); a code outside 100..=599 fails the call (O).
    pub(super) fn answer(&mut self, message: ControlMessage, last: bool) {
        let kind = message.message_type();
        let Some((id, payload, error_code)) = response(message) else {
            return;
        };
        let key = (kind, id);
        let Some(open) = self.open.get(&key) else {
            return;
        };
        let held = open.queued.load(Ordering::Relaxed);
        let code = error_code.filter(|&code| code != 0 && last);
        let status =
            code.and_then(|code| u16::try_from(code).ok().filter(|s| (100..600).contains(s)));
        let (chunk, last) = match (code, status) {
            (Some(code), None) => (Chunk::Failed(HttpError::Forward(code)), true),
            _ if held.saturating_add(payload.len()) > self.queued_limit => {
                let error = format!(
                    "the forwarded body is over {} queued bytes",
                    self.queued_limit
                );
                (Chunk::Failed(HttpError::Transport(error)), true)
            }
            _ => {
                open.queued.fetch_add(payload.len(), Ordering::Relaxed);
                let data = Chunk::Data {
                    payload,
                    last,
                    status,
                };
                (data, last)
            }
        };
        // A call that stopped waiting (cancelled) is dropped with its first late chunk.
        if open.chunks.send(chunk).is_err() || last {
            self.open.remove(&key);
        }
    }

    /// Calls past their deadline fail with a timeout (`0x1bf000`).
    pub(super) fn expire(&mut self, now: Duration) {
        self.open.retain(|_, open| {
            let waiting = now < open.due;
            if !waiting {
                _ = open.chunks.send(Chunk::Failed(HttpError::Timeout));
            }
            waiting
        });
    }

    /// The link went down: every open call fails.
    pub(super) fn disconnected(&mut self) {
        for (_, open) in std::mem::take(&mut self.open) {
            let error = HttpError::Transport("Cortex Control disconnected".into());
            _ = open.chunks.send(Chunk::Failed(error));
        }
        self.refuse_posted("Cortex Control is not connected");
    }
}

fn timeout_ms(timeout: Duration) -> i32 {
    i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX)
}

fn headers(request: &HttpRequest) -> Vec<ProductForwardHeaderPair> {
    request
        .headers
        .iter()
        .map(|(name, value)| ProductForwardHeaderPair {
            name: Some(name.clone()),
            value: Some(value.clone()),
        })
        .collect()
}

/// The forward message of a call, and for a backup body that goes on the offset of its next
/// chunk. A backup body over [`BACKUP_CHUNK`] goes in pieces of that size from `offset` under
/// one ID, `is_last_chunk` on the last (`0x1458ac`); every other call is one message.
fn forward_message(
    request: &HttpRequest,
    id: u64,
    offset: usize,
) -> (ControlMessage, Option<usize>) {
    let action = Some(MessageAction::Update.into());
    let url = Some(request.url.clone());
    let timeout = Some(timeout_ms(request.timeout));
    let message = match request.route {
        Route::Product => ProductForwardMessage {
            action,
            cloud_request_id: Some(id),
            request: Some(ProductForwardRequest {
                operation: Some(operation(request.method).into()),
                url,
                timeout,
                // `0x80c254`: a multipart call goes with `is_multipart` (no Content-Type).
                is_multipart: request.multipart.is_some().then_some(true),
                payload: request.body.clone(),
                headers: headers(request),
            }),
            ..Default::default()
        }
        .into(),
        Route::Backups(op) => {
            let (payload, next) = match request.body.as_deref() {
                Some(body) if body.len() > BACKUP_CHUNK => {
                    let end = body.len().min(offset + BACKUP_CHUNK);
                    let chunk = body.get(offset..end).unwrap_or_default().to_vec();
                    (Some(chunk), (end < body.len()).then_some(end))
                }
                _ => (request.body.clone(), None),
            };
            let message = BackupsForwardMessage {
                action,
                backups_request_id: Some(id),
                request: Some(BackupsForwardRequest {
                    request_type: Some(wire(op as i32)),
                    url,
                    payload,
                    is_last_chunk: Some(next.is_none()),
                    timeout,
                    // `0x1458ac` (C): the header list goes with the last chunk only.
                    headers: next.map_or_else(|| headers(request), |_| Vec::new()),
                }),
                ..Default::default()
            };
            return (message.into(), next);
        }
        Route::Logs(op) => LogsForwardMessage {
            action,
            logs_request_id: Some(id),
            request: Some(LogsForwardRequest {
                request_type: Some(wire(op as i32)),
                url,
                payload: request.body.clone(),
                is_last_chunk: Some(true),
                timeout,
                headers: headers(request),
            }),
            ..Default::default()
        }
        .into(),
        Route::Updater(op) => UpdaterForwardMessage {
            action,
            updater_request_id: Some(id),
            request: Some(UpdaterForwardRequest {
                request_type: Some(wire(op as i32)),
                url,
                timeout,
                payload: (request.body.as_deref())
                    .filter(|body| !body.is_empty())
                    .map(|body| String::from_utf8_lossy(body).into_owned()),
                headers: headers(request),
            }),
            ..Default::default()
        }
        .into(),
    };
    (message, None)
}

/// tetrad-services request types use the wire numbers.
fn wire<E: TryFrom<i32>>(number: i32) -> EnumValue<E> {
    E::try_from(number).map_or(EnumValue::Unknown(number), EnumValue::Known)
}

fn operation(method: Method) -> HttpOperation {
    match method {
        Method::Get => HttpOperation::Get,
        Method::Put => HttpOperation::Put,
        Method::Post => HttpOperation::Post,
        Method::Delete => HttpOperation::Delete,
    }
}

/// The forward ID, body chunk and error code of a response.
fn response(message: ControlMessage) -> Option<(u64, Vec<u8>, Option<i32>)> {
    let pick = |id: Option<u64>, payload: Option<Vec<u8>>, code: Option<i32>| {
        Some((id.unwrap_or(0), payload.unwrap_or_default(), code))
    };
    match message {
        ControlMessage::ProductForward(m) => {
            let r = m.response?;
            pick(m.cloud_request_id, r.payload, r.error_code)
        }
        ControlMessage::BackupsForward(m) => {
            let r = m.response?;
            pick(m.backups_request_id, r.payload, r.error_code)
        }
        ControlMessage::LogsForward(m) => {
            let r = m.response?;
            pick(m.logs_request_id, r.payload, r.error_code)
        }
        ControlMessage::UpdaterForward(m) => {
            let r = m.response?;
            pick(m.updater_request_id, r.payload, r.error_code)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_services::exec::CancelToken;
    use tetrad_services::fake::MemFiles;
    use tetrad_services::http::UpdaterOp;

    /// A cancelled call stops waiting; its late chunk closes the forward.
    #[test]
    fn cancelled_call_stops_waiting_and_its_late_chunk_closes_the_forward() {
        let mut forwards = Forwards::default();
        let link = forwards.link();
        let cancel = CancelToken::default();
        let request = HttpRequest::new(
            Method::Get,
            "https://example.invalid/fw",
            Route::Updater(UpdaterOp::DownloadUpdate),
            Duration::from_secs(1800),
        )
        .cancel(cancel.clone());
        let worker =
            std::thread::spawn(move || ForwardHttp::new(link, MemFiles::default()).send(&request));
        let message = loop {
            if let Some((_, message)) = forwards.take(Duration::ZERO).pop() {
                break message;
            }
            std::thread::yield_now();
        };
        cancel.cancel();
        assert_eq!(worker.join().unwrap(), Err(HttpError::Cancelled));
        let ControlMessage::UpdaterForward(sent) = message else {
            panic!("an UpdaterForward");
        };
        let late = UpdaterForwardMessage {
            response: Some(tetrad_control::codecs::UpdaterForwardResponse {
                payload: Some(b"late".to_vec()),
                ..Default::default()
            }),
            ..sent
        };
        forwards.answer(late.into(), false);
        assert!(forwards.open.is_empty());
    }

    /// r45-services MR 2 and review-r45 M1 (`0x1458ac`): a backup body goes to Control in
    /// chunks of at most 150 000 bytes under one `backups_request_id`, one chunk per take,
    /// `is_last_chunk` on the last only; the call waits for its timeout from the last chunk. A
    /// body that fits is one message.
    #[test]
    fn a_backup_body_goes_in_150000_byte_chunks() {
        use tetrad_services::http::BackupsOp;
        /// One chunk: its ID, its size and `is_last_chunk`.
        type Sent = (Option<u64>, usize, Option<bool>);
        let chunks = |bytes: usize| -> Vec<Vec<Sent>> {
            let mut forwards = Forwards::default();
            let mut request = HttpRequest::new(
                Method::Put,
                "https://example.invalid/backup",
                Route::Backups(BackupsOp::CreateOrUpdate),
                Duration::from_secs(600),
            );
            request.body = Some(vec![7; bytes]);
            let (chunks, _answers) = mpsc::channel();
            let queued = Arc::default();
            (forwards.link().0.send(Call {
                request,
                chunks,
                queued,
            }))
            .unwrap();
            (0..4)
                .map(|tick| forwards.take(Duration::from_secs(tick)))
                .map(|messages| {
                    (messages.into_iter())
                        .map(|(_, message)| match message {
                            ControlMessage::BackupsForward(m) => {
                                let request = m.request.unwrap();
                                let len = request.payload.map_or(0, |body| body.len());
                                (m.backups_request_id, len, request.is_last_chunk)
                            }
                            other => panic!("not a BackupsForward: {other:?}"),
                        })
                        .collect()
                })
                .collect()
        };
        assert_eq!(
            chunks(2 * 150_000 + 1),
            [
                vec![(Some(1), 150_000, Some(false))],
                vec![(Some(1), 150_000, Some(false))],
                vec![(Some(1), 1, Some(true))],
                Vec::new(),
            ]
        );
        assert_eq!(
            chunks(150_000),
            [vec![(Some(1), 150_000, Some(true))], vec![], vec![], vec![]]
        );
        // The last chunk went at 2 s: the call waits until 602 s.
        let mut forwards = Forwards::default();
        let mut request = HttpRequest::new(
            Method::Put,
            "https://example.invalid/backup",
            Route::Backups(BackupsOp::CreateOrUpdate),
            Duration::from_secs(600),
        );
        request.body = Some(vec![7; 2 * 150_000 + 1]);
        let (chunks, answers) = mpsc::channel();
        let call = Call {
            request,
            chunks,
            queued: Arc::default(),
        };
        forwards.link().0.send(call).unwrap();
        (0..3).for_each(|tick| _ = forwards.take(Duration::from_secs(tick)));
        forwards.expire(Duration::from_secs(601));
        assert!(answers.try_recv().is_err(), "still waiting at 601 s");
        forwards.expire(Duration::from_secs(602));
        assert!(matches!(
            answers.try_recv(),
            Ok(Chunk::Failed(HttpError::Timeout))
        ));
    }

    /// Review r21 H8: chunks a slow worker has not taken count against the bound; the chunk
    /// past it fails the call and closes the forward, and the bridge never waits.
    #[test]
    fn a_call_that_falls_behind_its_queue_bound_fails() {
        let mut forwards = Forwards {
            queued_limit: 8,
            ..Forwards::default()
        };
        let (chunks, slow) = mpsc::channel();
        let request = HttpRequest::new(
            Method::Get,
            "https://example.invalid/fw",
            Route::Updater(UpdaterOp::DownloadUpdate),
            Duration::from_secs(1800),
        );
        let queued = Arc::new(AtomicUsize::new(0));
        let call = Call {
            request,
            chunks,
            queued: Arc::clone(&queued),
        };
        forwards.link().0.send(call).unwrap();
        let Some((_, ControlMessage::UpdaterForward(sent))) = forwards.take(Duration::ZERO).pop()
        else {
            panic!("an UpdaterForward");
        };
        let chunk = |payload: &[u8]| UpdaterForwardMessage {
            response: Some(tetrad_control::codecs::UpdaterForwardResponse {
                payload: Some(payload.to_vec()),
                ..Default::default()
            }),
            ..sent.clone()
        };
        forwards.answer(chunk(b"12345").into(), false);
        assert_eq!(queued.load(Ordering::Relaxed), 5);
        forwards.answer(chunk(b"6789").into(), false);
        assert!(forwards.open.is_empty());
        assert!(matches!(
            slow.try_recv(),
            Ok(Chunk::Data { last: false, .. })
        ));
        assert!(matches!(
            slow.try_recv(),
            Ok(Chunk::Failed(HttpError::Transport(_)))
        ));
    }
}

//! Outbound message collection for one link step.

use crate::REPORT_BYTES;
use crate::codecs::{ControlMessage, CortexMessageType, SceneMessage};
use crate::pipeline::{
    Outgoing, PipelineConfig, PipelineError, RequestStamp, SendLimits, SendOptions, Stages,
    stock_send_options,
};

/// Collects encoded report bodies in send order. The request context is the
/// stock "current request ID": the dispatcher sets it to the request's ID
/// while a receiver runs, so every message sent from that receiver echoes it.
pub struct Outbox<'a> {
    pipeline: &'a PipelineConfig,
    stages: Stages<'a>,
    limits: SendLimits,
    pub(crate) current_request_id: u64,
    reports: Vec<[u8; REPORT_BYTES]>,
    sent: Vec<CortexMessageType>,
}

impl<'a> Outbox<'a> {
    pub(crate) fn new(
        pipeline: &'a PipelineConfig,
        stages: Stages<'a>,
        limits: SendLimits,
    ) -> Self {
        Self {
            pipeline,
            stages,
            limits,
            current_request_id: 0,
            reports: Vec::new(),
            sent: Vec::new(),
        }
    }

    /// Normal path with the stock per-type flags and the current request
    /// context. Connection and sender-enable gates belong to the caller.
    pub fn send(&mut self, message: impl Into<ControlMessage>) -> Result<(), PipelineError> {
        let message = message.into();
        let options = stock_send_options(message.message_type());
        self.send_with(message, options, 0)
    }

    /// Priority path; see `PipelineConfig::send_direct`.
    pub fn send_direct(&mut self, message: impl Outgoing) -> Result<(), PipelineError> {
        let message_type = CortexMessageType::try_from(message.message_type())
            .expect("typed outgoing messages have known types");
        let reports = self.pipeline.send_direct(message, self.limits)?;
        self.reports.extend(reports);
        self.sent.push(message_type);
        Ok(())
    }

    pub fn send_scene(&mut self, message: SceneMessage) -> Result<(), PipelineError> {
        self.send_with(message, SendOptions::default(), 0)
    }

    /// Stock sender with explicit options; a nonzero `explicit_request_id`
    /// overrides the current request context.
    pub fn send_with<M: Outgoing>(
        &mut self,
        message: M,
        options: SendOptions,
        explicit_request_id: u64,
    ) -> Result<(), PipelineError> {
        let message_type = CortexMessageType::try_from(message.message_type())
            .expect("typed outgoing messages have known types");
        let stamp = RequestStamp {
            explicit: explicit_request_id,
            current: self.current_request_id,
        };
        if let Some(reports) =
            self.pipeline
                .send(message, options, stamp, self.limits, self.stages)?
        {
            self.reports.extend(reports);
            self.sent.push(message_type);
        }
        Ok(())
    }

    pub(crate) fn sent_count(&self) -> usize {
        self.sent.len()
    }

    pub(crate) fn finish(self) -> (Vec<[u8; REPORT_BYTES]>, Vec<CortexMessageType>) {
        (self.reports, self.sent)
    }
}

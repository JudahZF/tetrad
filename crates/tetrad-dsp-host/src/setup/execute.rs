//! Executes [`DspWrite`]s over a [`DspSession`] in stock order: single
//! requests at once, or stock bulk mode (VAs `0x51267c`, `0x517010`,
//! `0x517228`, `0x511084`, `0x511188`).

use tetrad_dsp_protocol::DspCore;
use tetrad_dsp_protocol::packet::{BulkMode, BulkRecord};
use tetrad_dsp_protocol::requests::{
    BULK_PARAMETER_RECORDS_MAX, BULK_SEND_POLICY, BulkFrame, BulkRequest, BypassRecord, Request,
};

use super::DspWrite;
use super::tables::StockTables;
use crate::SharedMemory;
use crate::model::CoreWindows;
use crate::session::{BypassUpdate, Clock, DspSession, ParameterUpdate, SessionError};
use crate::transport::ControlPacket;

const DSP_CORES: [DspCore; 4] = [
    DspCore::Core0,
    DspCore::Core1,
    DspCore::Core2,
    DspCore::Core3,
];

/// Stock senders log a timed-out send, a dead core or dropped data
/// (VA `0x51965c`) and carry on.
fn tolerated(error: &SessionError) -> bool {
    matches!(
        error,
        SessionError::SendTimeout { .. } | SessionError::DeadCore(_) | SessionError::Refused(_)
    )
}

/// Keeps the first tolerated error; stops on anything else.
fn tolerate<T>(
    first: &mut Option<SessionError>,
    result: Result<T, SessionError>,
) -> Result<(), SessionError> {
    match result {
        Ok(_) => Ok(()),
        Err(error) if tolerated(&error) => {
            first.get_or_insert(error);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Stock bulk queues (controller `+0xb8ac` parameters, `+0xb8dc` bypass).
struct Bulk {
    mode: BulkMode,
    parameters: [Vec<BulkRecord>; 4],
    bypass: [Vec<BypassRecord>; 4],
}

impl<M: SharedMemory, C: Clock> DspSession<M, C> {
    /// Stock post-start order (VA `0x585464`): every constructor write at
    /// once (the factory runs before the push), then the block pushes inside
    /// bulk mode `mode` (`Value16` after a full redeploy, `Value8` for an
    /// in-place update).
    pub fn run_setup<W: SharedMemory>(
        &mut self,
        windows: &mut CoreWindows<W>,
        tables: &impl StockTables,
        mode: BulkMode,
        constructors: &[DspWrite],
        push: &[DspWrite],
    ) -> Result<(), SessionError> {
        let constructed = self.execute(windows, tables, None, constructors);
        if let Err(error) = &constructed
            && !tolerated(error)
        {
            return constructed;
        }
        let pushed = self.execute(windows, tables, Some(mode), push);
        constructed.and(pushed)
    }

    /// Sends `writes` in order. `Some(mode)` is stock bulk mode: parameters
    /// queue per core and every core flushes when one queue holds 169
    /// records; bypass states queue per core; `0x03` and ReceiveData go out
    /// at once. The end flushes parameters (cores 0..=3), then bypass, then
    /// `syncAllCores` and `0x3e` when a bypass message went out.
    pub fn execute<W: SharedMemory>(
        &mut self,
        windows: &mut CoreWindows<W>,
        tables: &impl StockTables,
        bulk: Option<BulkMode>,
        writes: &[DspWrite],
    ) -> Result<(), SessionError> {
        let mut first = None;
        let mut bulk = bulk.map(|mode| Bulk {
            mode,
            parameters: Default::default(),
            bypass: Default::default(),
        });
        for write in writes {
            let result = match (write.clone(), &mut bulk) {
                (
                    DspWrite::Parameter {
                        target,
                        index,
                        value_bits,
                    },
                    Some(bulk),
                ) => {
                    let queue = &mut bulk.parameters[target.core as usize];
                    queue.push(BulkRecord {
                        slot: target.slot,
                        parameter_index: index,
                        value_bits,
                    });
                    if queue.len() == BULK_PARAMETER_RECORDS_MAX {
                        self.flush_bulk_parameters(bulk, &mut first)?;
                    }
                    Ok(())
                }
                (DspWrite::Bypass { target, state }, Some(bulk)) => {
                    bulk.bypass[target.core as usize].push(BypassRecord {
                        slot: target.slot,
                        state,
                    });
                    Ok(())
                }
                (write, _) => self.write(windows, tables, write),
            };
            tolerate(&mut first, result)?;
        }
        if let Some(mut bulk) = bulk {
            self.flush_bulk_parameters(&mut bulk, &mut first)?;
            let mut sent = false;
            for (core, records) in DSP_CORES.into_iter().zip(bulk.bypass) {
                if records.is_empty() {
                    continue;
                }
                let frame = BulkFrame::new(BulkRequest::Bypass { core, records })?;
                let result = self.send(ControlPacket::Bulk(&frame), BULK_SEND_POLICY);
                tolerate(&mut first, result)?;
                sent = true;
            }
            if sent {
                let result = self.sync_all_cores();
                tolerate(&mut first, result)?;
                let result = self.request(Request::BulkModelBypassSync);
                tolerate(&mut first, result)?;
            }
        }
        first.map_or(Ok(()), Err)
    }

    fn write<W: SharedMemory>(
        &mut self,
        windows: &mut CoreWindows<W>,
        tables: &impl StockTables,
        write: DspWrite,
    ) -> Result<(), SessionError> {
        match write {
            DspWrite::Parameter {
                target,
                index,
                value_bits,
            } => self
                .set_parameter(ParameterUpdate {
                    target,
                    index,
                    value_bits,
                })
                .map(drop),
            DspWrite::IntParameter {
                target,
                index,
                value,
            } => self
                .request(Request::IntParameterChange {
                    target,
                    parameter: index,
                    value,
                })
                .map(drop),
            DspWrite::Bypass { target, state } => {
                self.set_bypass(BypassUpdate { target, state }).map(drop)
            }
            DspWrite::StockData {
                target,
                buffer,
                payload,
            } => {
                let bytes = tables
                    .read(payload)
                    .map_err(|error| SessionError::Platform(Box::new(error)))?;
                self.send_model_data(windows, target, buffer, &bytes)
            }
            DspWrite::Data {
                target,
                buffer,
                payload,
            } => self.send_model_data(windows, target, buffer, &payload),
            DspWrite::Control(request) => {
                let frame = tetrad_dsp_protocol::controls::RequestFrame::new(request)?;
                self.send(
                    ControlPacket::Controls(&frame),
                    crate::session::SendPolicy::new(false, 100),
                )
                .map(drop)
            }
            DspWrite::Request(request) => self.request(request).map(drop),
            DspWrite::Tuner(request) => {
                let frame = tetrad_dsp_protocol::tuner_looper::RequestFrame::new(request);
                self.send(
                    ControlPacket::TunerLooper(&frame),
                    crate::session::SendPolicy::new(true, 100),
                )
                .map(drop)
            }
            DspWrite::SyncAllCores { settle } => {
                let synced = self.sync_all_cores();
                self.clock_mut().sleep(settle);
                synced
            }
        }
    }

    fn flush_bulk_parameters(
        &mut self,
        bulk: &mut Bulk,
        first: &mut Option<SessionError>,
    ) -> Result<(), SessionError> {
        for (core, queue) in DSP_CORES.into_iter().zip(bulk.parameters.iter_mut()) {
            if queue.is_empty() {
                continue;
            }
            let frame = BulkFrame::new(BulkRequest::Parameters {
                core,
                mode: bulk.mode,
                records: std::mem::take(queue),
            })?;
            let result = self.send(ControlPacket::Bulk(&frame), BULK_SEND_POLICY);
            tolerate(first, result)?;
        }
        Ok(())
    }
}

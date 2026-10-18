//! The HID transmit queue: stock `neural::cortex::USBMessageSender` (ctor `0x29b0e4`, C).
//!
//! - Stock queues each 144-byte `0x27` frame in a deque (`0x29abb0` Control messages,
//!   `0x29affc` raw reports) and sends from the slot `sendHIDReportsSlot` (`0x29ae64`) of a
//!   zero-interval `QTimer` (`+8`, `timeout()` → slot, `0x29b0e4`): at most 12 frames per
//!   call, on the main thread, between the other events. An enqueue on an idle sender starts
//!   the timer and sends at once (`0x29ab34`); the timer stops when both deques are empty.
//! - Back-pressure: a send that fails (`0x6dfbf8`, ring wait 0, timeout 100 ms) ends the call
//!   and its frame stays first; the next call tries it again. Stock drops nothing and has no
//!   queue bound.
//! - Disconnect and ResetComms (`SessionEvent::FlushTransmit`, `0x29ad14`): both deques are
//!   cleared and the reset marker (byte 2 = `0xff`) is queued first.
//!
//! Tetrad keeps one deque (the link builds the reports in send order) and the device sends from
//! it once per pass ([`super::super::Device::step`]), so a 400-report reply no longer holds
//! one pass (QC: about 0.63 ms per report, `apply: ControlReport x462 293ms`, A3).

use std::collections::VecDeque;

/// `0x29ae64`: frames per slot call (`iVar1 = 0xc`).
pub const FRAMES_PER_PASS: usize = 12;

/// One frame of the queue, in send order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame<'a> {
    /// The transport reset marker (`0x29ad14`).
    Marker,
    Report(&'a [u8; 128]),
}

#[derive(Debug, Default)]
pub struct Outgoing {
    /// The reset marker goes before the queued reports.
    marker: bool,
    reports: VecDeque<Box<[u8; 128]>>,
    /// The last send failed; a send that works clears it.
    failing: bool,
}

/// What one [`Outgoing::send`] did.
#[derive(Debug, PartialEq, Eq)]
pub struct Sent<E> {
    pub frames: usize,
    /// The send that ended the call, only for the first failure of a run (later failures of
    /// the same frame are not news).
    pub error: Option<E>,
}

impl Outgoing {
    pub fn push(&mut self, report: Box<[u8; 128]>) {
        self.reports.push_back(report);
    }

    /// `0x29ad14`: drops the queued reports; the reset marker goes first.
    pub fn flush(&mut self) {
        self.reports.clear();
        self.marker = true;
    }

    /// A frame waits (the stock timer runs).
    pub fn pending(&self) -> bool {
        self.marker || !self.reports.is_empty()
    }

    /// The queued reports (the marker is not one).
    pub fn report_count(&self) -> usize {
        self.reports.len()
    }

    pub fn marker_pending(&self) -> bool {
        self.marker
    }

    /// Sends up to `limit` frames in queue order. A failed frame stays first and ends the
    /// call.
    pub fn send<E>(
        &mut self,
        limit: usize,
        mut send: impl FnMut(Frame<'_>) -> Result<(), E>,
    ) -> Sent<E> {
        let mut frames = 0;
        while frames < limit {
            let result = if self.marker {
                send(Frame::Marker).map(|()| self.marker = false)
            } else if let Some(report) = self.reports.front() {
                send(Frame::Report(report)).map(|()| _ = self.reports.pop_front())
            } else {
                break;
            };
            match result {
                Ok(()) => {
                    frames += 1;
                    self.failing = false;
                }
                Err(error) => {
                    let first = !std::mem::replace(&mut self.failing, true);
                    return Sent {
                        frames,
                        error: first.then_some(error),
                    };
                }
            }
        }
        Sent {
            frames,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(tag: u8) -> Box<[u8; 128]> {
        Box::new([tag; 128])
    }

    fn tags(frames: &[Frame<'_>]) -> Vec<Option<u8>> {
        (frames.iter())
            .map(|frame| match frame {
                Frame::Marker => None,
                Frame::Report(report) => Some(report[0]),
            })
            .collect()
    }

    /// The writer keeps the report order across calls, sends at most the limit per call, and
    /// a failed frame stays first: the next call sends it before the later ones, and only the
    /// first failure of a run is reported.
    #[test]
    fn order_holds_across_calls_and_a_failed_frame_is_sent_again_first() {
        let mut queue = Outgoing::default();
        (0..30).for_each(|tag| queue.push(report(tag)));
        let mut wire = Vec::new();
        let sent = queue.send(FRAMES_PER_PASS, |frame| {
            wire.push(tags(&[frame])[0]);
            Ok::<_, ()>(())
        });
        assert_eq!(
            sent,
            Sent {
                frames: 12,
                error: None
            }
        );
        let mut calls = 0;
        let fail = |frame: Frame<'_>, calls: &mut usize, wire: &mut Vec<Option<u8>>| {
            *calls += 1;
            if *calls == 3 {
                return Err("ring full");
            }
            wire.push(tags(&[frame])[0]);
            Ok(())
        };
        let sent = queue.send(FRAMES_PER_PASS, |frame| fail(frame, &mut calls, &mut wire));
        assert_eq!(
            sent,
            Sent {
                frames: 2,
                error: Some("ring full")
            }
        );
        let sent = queue.send(1, |_| Err("ring full"));
        assert_eq!(
            sent,
            Sent {
                frames: 0,
                error: None
            },
            "the same run of failures"
        );
        while queue.pending() {
            queue.send(FRAMES_PER_PASS, |frame| {
                wire.push(tags(&[frame])[0]);
                Ok::<_, ()>(())
            });
        }
        assert_eq!(wire, (0..30).map(Some).collect::<Vec<_>>());
    }

    /// A disconnect or ResetComms drops what waits; the marker goes before the next reports.
    #[test]
    fn a_flush_drops_the_queue_and_the_marker_goes_first() {
        let mut queue = Outgoing::default();
        (0..5).for_each(|tag| queue.push(report(tag)));
        queue.flush();
        queue.push(report(9));
        let mut wire = Vec::new();
        queue.send(FRAMES_PER_PASS, |frame| {
            wire.push(tags(&[frame])[0]);
            Ok::<_, ()>(())
        });
        assert_eq!(wire, [None, Some(9)]);
        assert!(!queue.pending());
    }
}

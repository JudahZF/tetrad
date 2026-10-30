//! The Tetrad Link service: the GATT service of the Tetrad app (docs/bluetooth.md). The app
//! writes frames to [`REQUEST`]; the unit notifies frames on [`REPLY`]. A frame is one flags
//! byte ([`START`], [`END`]) and a piece of a message; a message runs from a START frame to an
//! END frame. The message itself is the Tetrad message format of the USB link (a JSON header
//! and a body), which this layer does not read.

use crate::Uuid;

/// `5162a6fe-ad05-405f-b22e-6e4047a5b524`.
pub const SERVICE: Uuid = Uuid::from_u128(0x5162a6fe_ad05_405f_b22e_6e4047a5b524);
/// App → unit: write without response (or write).
pub const REQUEST: Uuid = Uuid::from_u128(0x5162a6fe_ad05_405f_b22e_6e4047a5b525);
/// Unit → app: notify.
pub const REPLY: Uuid = Uuid::from_u128(0x5162a6fe_ad05_405f_b22e_6e4047a5b526);

pub const START: u8 = 0x01;
pub const END: u8 = 0x02;
/// The largest message the unit takes: a file piece of 48 KiB with room for its header.
pub const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("a Tetrad Link frame with no START before it")]
    NoStart,
    #[error("a Tetrad Link message longer than {MAX_MESSAGE} bytes")]
    TooLong,
    #[error("an empty Tetrad Link frame")]
    Empty,
}

/// Joins frames into messages.
#[derive(Debug, Default)]
pub struct Reassembler {
    message: Option<Vec<u8>>,
}

impl Reassembler {
    /// One frame. Returns a complete message. A START drops a message that did not end.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, FrameError> {
        let (&flags, piece) = frame.split_first().ok_or(FrameError::Empty)?;
        if flags & START != 0 {
            self.message = Some(Vec::new());
        }
        let message = self.message.as_mut().ok_or(FrameError::NoStart)?;
        if message.len() + piece.len() > MAX_MESSAGE {
            self.message = None;
            return Err(FrameError::TooLong);
        }
        message.extend_from_slice(piece);
        if flags & END != 0 {
            return Ok(self.message.take());
        }
        Ok(None)
    }
}

/// The frames of `message`, each at most `max` bytes with its flags byte.
pub fn frames(message: &[u8], max: usize) -> Vec<Vec<u8>> {
    let room = max.saturating_sub(1).max(1);
    let pieces: Vec<&[u8]> = if message.is_empty() {
        vec![&[]]
    } else {
        message.chunks(room).collect()
    };
    let last = pieces.len() - 1;
    (pieces.into_iter().enumerate())
        .map(|(i, piece)| {
            let mut flags = 0;
            if i == 0 {
                flags |= START;
            }
            if i == last {
                flags |= END;
            }
            let mut frame = vec![flags];
            frame.extend_from_slice(piece);
            frame
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_cut_into_frames_and_join_again() {
        let message: Vec<u8> = (0..=255).collect();
        let frames = frames(&message, 20);
        assert_eq!(frames.len(), 14);
        assert!(frames.iter().all(|frame| frame.len() <= 20));
        assert_eq!(frames[0][0], START);
        assert_eq!(frames[13][0], END);
        let mut r = Reassembler::default();
        let joined: Vec<_> = frames.iter().filter_map(|f| r.push(f).unwrap()).collect();
        assert_eq!(joined, vec![message]);
        // An empty message is one frame with both flags.
        assert_eq!(super::frames(&[], 20), vec![vec![START | END]]);
        assert_eq!(r.push(&[START | END]), Ok(Some(Vec::new())));
    }

    #[test]
    fn a_frame_out_of_place_is_refused() {
        let mut r = Reassembler::default();
        assert_eq!(r.push(&[0, 1, 2]), Err(FrameError::NoStart));
        assert_eq!(r.push(&[]), Err(FrameError::Empty));
        let big = vec![0; MAX_MESSAGE];
        assert_eq!(r.push(&[START, 0]), Ok(None));
        let mut frame = vec![0];
        frame.extend_from_slice(&big);
        assert_eq!(r.push(&frame), Err(FrameError::TooLong));
        // A new START begins again.
        assert_eq!(r.push(&[START | END, 7]), Ok(Some(vec![7])));
    }
}

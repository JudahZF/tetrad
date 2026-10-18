//! Shared bounded protobuf reader. Scene error mapping stays unchanged.

use crate::scene::{SceneLimits, SceneWireError};
use crate::{Error, Result};

pub(crate) struct Cursor<'a> {
    pub(crate) payload: &'a [u8],
    pub(crate) offset: usize,
    pub(crate) fields: usize,
    pub(crate) limits: SceneLimits,
}

impl<'a> Cursor<'a> {
    fn malformed(&self, problem: SceneWireError) -> Error {
        Error::SceneWire {
            offset: self.offset,
            problem,
        }
    }

    pub(crate) fn varint(&mut self) -> Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let byte = *self
                .payload
                .get(self.offset)
                .ok_or_else(|| self.malformed(SceneWireError::Truncated))?;
            self.offset += 1;
            if shift == 63 && byte > 1 {
                return Err(self.malformed(SceneWireError::VarintOverflow));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        unreachable!("tenth byte either terminates or overflows")
    }

    pub(crate) fn key(&mut self) -> Result<(u32, u8)> {
        if self.fields >= self.limits.max_fields {
            return Err(Error::SceneFieldLimit(self.limits.max_fields));
        }
        self.fields += 1;
        let key = self.varint()?;
        let number = key >> 3;
        if number == 0 || number > 0x1fff_ffff {
            return Err(self.malformed(SceneWireError::FieldNumber));
        }
        let wire = (key & 7) as u8;
        if wire > 5 {
            return Err(self.malformed(SceneWireError::WireType(wire)));
        }
        Ok((number as u32, wire))
    }

    fn advance(&mut self, length: usize) -> Result<()> {
        let end = self
            .offset
            .checked_add(length)
            .filter(|end| *end <= self.payload.len())
            .ok_or_else(|| self.malformed(SceneWireError::Truncated))?;
        self.offset = end;
        Ok(())
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let start = self.offset;
        self.advance(length)?;
        Ok(&self.payload[start..self.offset])
    }

    pub(crate) fn bytes(&mut self) -> Result<&'a [u8]> {
        let length = usize::try_from(self.varint()?)
            .map_err(|_| self.malformed(SceneWireError::LengthOverflow))?;
        self.take(length)
    }

    pub(crate) fn skip(&mut self, number: u32, wire: u8, depth: usize) -> Result<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => self.advance(8)?,
            2 => {
                let length = usize::try_from(self.varint()?)
                    .map_err(|_| self.malformed(SceneWireError::LengthOverflow))?;
                self.advance(length)?;
            }
            3 => {
                if depth >= self.limits.max_group_depth {
                    return Err(Error::SceneDepthLimit(self.limits.max_group_depth));
                }
                loop {
                    let (nested, wire) = self.key()?;
                    if wire == 4 {
                        if nested != number {
                            return Err(self.malformed(SceneWireError::MismatchedEndGroup));
                        }
                        break;
                    }
                    self.skip(nested, wire, depth + 1)?;
                }
            }
            4 => return Err(self.malformed(SceneWireError::UnexpectedEndGroup)),
            5 => self.advance(4)?,
            _ => unreachable!("key validates wire type"),
        }
        Ok(())
    }
}

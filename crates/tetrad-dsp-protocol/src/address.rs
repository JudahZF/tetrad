//! Section units are explicit; no physical-memory alias mapping is inferred.
use crate::Error;
use std::marker::PhantomData;

mod sealed {
    pub trait Sealed {}
}

pub trait Unit: sealed::Sealed + Copy + Eq {
    const BYTE_WIDTH: u32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bytes {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parcels16 {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Words48 {}
impl sealed::Sealed for Bytes {}
impl sealed::Sealed for Parcels16 {}
impl sealed::Sealed for Words48 {}
impl Unit for Bytes {
    const BYTE_WIDTH: u32 = 1;
}
impl Unit for Parcels16 {
    const BYTE_WIDTH: u32 = 2;
}
impl Unit for Words48 {
    const BYTE_WIDTH: u32 = 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address<U: Unit> {
    value: u32,
    unit: PhantomData<U>,
}

impl<U: Unit> Address<U> {
    pub const fn new(value: u32) -> Self {
        Self {
            value,
            unit: PhantomData,
        }
    }
    pub const fn value(self) -> u32 {
        self.value
    }
    pub fn byte_offset(self) -> Result<Address<Bytes>, Error> {
        self.value
            .checked_mul(U::BYTE_WIDTH)
            .map(Address::new)
            .ok_or(Error::Overflow("section byte offset"))
    }
    pub fn from_byte_offset(offset: Address<Bytes>) -> Result<Self, Error> {
        if !offset.value.is_multiple_of(U::BYTE_WIDTH) {
            return Err(Error::Invalid("section alignment"));
        }
        Ok(Self::new(offset.value / U::BYTE_WIDTH))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionAddress {
    Bytes(Address<Bytes>),
    Parcels16(Address<Parcels16>),
    Words48(Address<Words48>),
}

/// Inclusive source range. Units must match at compile time.
#[derive(Debug, Clone, Copy)]
pub struct Relocation<U: Unit> {
    start: Address<U>,
    end: Address<U>,
    placed_base: Address<U>,
}

impl<U: Unit> Relocation<U> {
    pub fn new(start: Address<U>, end: Address<U>, placed_base: Address<U>) -> Result<Self, Error> {
        if end.value < start.value {
            return Err(Error::Invalid("inclusive relocation range"));
        }
        placed_base
            .value
            .checked_add(end.value - start.value)
            .ok_or(Error::Overflow("relocation range"))?;
        Ok(Self {
            start,
            end,
            placed_base,
        })
    }
    pub fn apply(self, target: Address<U>) -> Result<Address<U>, Error> {
        if target.value < self.start.value || target.value > self.end.value {
            return Ok(target);
        }
        self.placed_base
            .value
            .checked_add(target.value - self.start.value)
            .map(Address::new)
            .ok_or(Error::Overflow("relocated target"))
    }
}

//! Typed row I/O ports. Values are the stock `InputPortId` / `OutputPortId` enums from
//! the `cortex_protobuf_v2` descriptor embedded in ZenUI (file offset `0x120dac8`);
//! `Chain.in_portid` / `out_portid` store them. `0x2e186c` treats output values
//! 0x10..=0x12 as next-row routing, which matches `NEXT_ROW_*` here.

use crate::PortId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum InputPort {
    Empty = 0,
    Input1 = 1,
    Input2 = 2,
    Input1And2 = 3,
    Return1 = 4,
    Return2 = 5,
    Return1And2 = 6,
    /// The previous row's output (paired with a `NEXT_ROW_*` output).
    PrevRow = 7,
    UsbIn5 = 8,
    UsbIn6 = 9,
    UsbIn7 = 10,
    UsbIn8 = 11,
    UsbIn5And6 = 12,
    UsbIn7And8 = 13,
    SidechainBuffer = 14,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum OutputPort {
    Empty = 0,
    Xlr1And2 = 1,
    Output3And4 = 2,
    Send1And2 = 3,
    Xlr1 = 4,
    Xlr2 = 5,
    Output3 = 6,
    Output4 = 7,
    Send1 = 8,
    Send2 = 9,
    UsbOut5 = 10,
    UsbOut6 = 11,
    UsbOut7 = 12,
    UsbOut8 = 13,
    UsbOut5And6 = 14,
    UsbOut7And8 = 15,
    NextRow3 = 16,
    NextRow4 = 17,
    NextRow3And4 = 18,
    /// "1/2 + 3/4 + USB 3/4" (port table `_INIT_123`, `0x48b5c`).
    MultipleOuts = 19,
    UsbOut3 = 20,
    UsbOut4 = 21,
    UsbOut3And4 = 22,
}

impl InputPort {
    pub const ALL: [Self; 15] = [
        Self::Empty,
        Self::Input1,
        Self::Input2,
        Self::Input1And2,
        Self::Return1,
        Self::Return2,
        Self::Return1And2,
        Self::PrevRow,
        Self::UsbIn5,
        Self::UsbIn6,
        Self::UsbIn7,
        Self::UsbIn8,
        Self::UsbIn5And6,
        Self::UsbIn7And8,
        Self::SidechainBuffer,
    ];

    pub fn from_raw(raw: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|port| *port as u32 == raw)
    }

    pub fn id(self) -> PortId {
        PortId(self as u32)
    }

    /// Hardware or USB channels the port reads; `Empty`, `PrevRow` and the
    /// sidechain buffer read other rows instead.
    pub fn is_external(self) -> bool {
        !matches!(self, Self::Empty | Self::PrevRow | Self::SidechainBuffer)
    }

    pub fn is_stereo(self) -> bool {
        matches!(
            self,
            Self::Input1And2 | Self::Return1And2 | Self::UsbIn5And6 | Self::UsbIn7And8
        )
    }
}

impl OutputPort {
    pub const ALL: [Self; 23] = [
        Self::Empty,
        Self::Xlr1And2,
        Self::Output3And4,
        Self::Send1And2,
        Self::Xlr1,
        Self::Xlr2,
        Self::Output3,
        Self::Output4,
        Self::Send1,
        Self::Send2,
        Self::UsbOut5,
        Self::UsbOut6,
        Self::UsbOut7,
        Self::UsbOut8,
        Self::UsbOut5And6,
        Self::UsbOut7And8,
        Self::NextRow3,
        Self::NextRow4,
        Self::NextRow3And4,
        Self::MultipleOuts,
        Self::UsbOut3,
        Self::UsbOut4,
        Self::UsbOut3And4,
    ];

    pub fn from_raw(raw: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|port| *port as u32 == raw)
    }

    pub fn id(self) -> PortId {
        PortId(self as u32)
    }

    /// The physical or USB ports this output drives.
    pub fn targets(self) -> &'static [Self] {
        match self {
            Self::MultipleOuts => &[Self::Xlr1And2, Self::Output3And4, Self::UsbOut3And4],
            Self::Empty | Self::NextRow3 | Self::NextRow4 | Self::NextRow3And4 => &[],
            _ => std::slice::from_ref(&Self::ALL[self as usize]),
        }
    }

    /// Feeds a lower-pair row (`NEXT_ROW_3` → row index 2, `NEXT_ROW_4` → 3).
    pub fn is_next_row(self) -> bool {
        matches!(self, Self::NextRow3 | Self::NextRow4 | Self::NextRow3And4)
    }

    pub fn is_stereo(self) -> bool {
        matches!(
            self,
            Self::Xlr1And2
                | Self::Output3And4
                | Self::Send1And2
                | Self::UsbOut5And6
                | Self::UsbOut7And8
                | Self::NextRow3And4
                | Self::MultipleOuts
                | Self::UsbOut3And4
        )
    }
}

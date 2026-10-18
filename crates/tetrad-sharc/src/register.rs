//! Scalar data registers, DAG1 and DAG2. F and R names share register storage.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{class} register index {index} is outside {minimum}..={maximum}")]
pub struct RegisterError {
    pub class: &'static str,
    pub index: u8,
    pub minimum: u8,
    pub maximum: u8,
}

macro_rules! register {
    ($name:ident, $class:literal, $maximum:literal) => {
        register!($name, $class, 0, $maximum);
    };
    ($name:ident, $class:literal, $minimum:literal, $maximum:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(u8);

        impl $name {
            pub const fn new(index: u8) -> Result<Self, RegisterError> {
                if index.wrapping_sub($minimum) <= $maximum - $minimum {
                    Ok(Self(index))
                } else {
                    Err(RegisterError {
                        class: $class,
                        index,
                        minimum: $minimum,
                        maximum: $maximum,
                    })
                }
            }

            /// A fixed register for constants: in a `const` an index out of
            /// range fails the build; at run time it panics.
            pub const fn of(index: u8) -> Self {
                match Self::new(index) {
                    Ok(register) => register,
                    Err(_) => panic!("register index out of range"),
                }
            }

            pub const fn index(self) -> u8 {
                self.0
            }
        }
    };
}

register!(DataReg, "R", 15);
register!(FloatReg, "F", 15);
register!(Dag1Index, "I (DAG1)", 7);
register!(Dag1Modify, "M (DAG1)", 7);
register!(Dag1Base, "B (DAG1)", 7);
register!(Dag1Length, "L (DAG1)", 7);
// DAG2 keeps architectural numbers 8..=15. PM transfers through DAG2 stay
// unsupported; these registers serve indirect branches and moves.
register!(Dag2Index, "I (DAG2)", 8, 15);
register!(Dag2Modify, "M (DAG2)", 8, 15);

impl From<FloatReg> for DataReg {
    fn from(value: FloatReg) -> Self {
        Self(value.0)
    }
}

impl From<DataReg> for FloatReg {
    fn from(value: DataReg) -> Self {
        Self(value.0)
    }
}

/// Only independently checked universal-register classes are representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniversalReg {
    Data(DataReg),
    Index(Dag1Index),
    Modify(Dag1Modify),
    Base(Dag1Base),
    Length(Dag1Length),
    Dag2Index(Dag2Index),
    Dag2Modify(Dag2Modify),
    System(SystemRegister),
}

/// Type17 system UREG codes used by the owned resident (PRM UREG table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemRegister {
    LoopCounter,
    CurrentLoopCounter,
    EmulatorClock,
    User1,
    User2,
    InterruptLatch,
    InterruptMask,
}

impl SystemRegister {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::CurrentLoopCounter => 0x67,
            Self::LoopCounter => 0x68,
            Self::EmulatorClock => 0x69,
            Self::User1 => 0x70,
            Self::User2 => 0x71,
            Self::InterruptLatch => 0x7a,
            Self::InterruptMask => 0x7b,
        }
    }
    /// Read-only counters cannot be immediate or move destinations.
    pub const fn writable(self) -> bool {
        !matches!(self, Self::CurrentLoopCounter | Self::EmulatorClock)
    }
}

impl UniversalReg {
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Data(r) => r.index(),
            Self::Index(r) => 0x10 + r.index(),
            Self::Modify(r) => 0x20 + r.index(),
            Self::Length(r) => 0x30 + r.index(),
            Self::Base(r) => 0x40 + r.index(),
            Self::Dag2Index(r) => 0x10 + r.index(),
            Self::Dag2Modify(r) => 0x20 + r.index(),
            Self::System(r) => r.code(),
        }
    }
}

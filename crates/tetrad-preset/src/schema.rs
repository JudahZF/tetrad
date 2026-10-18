//! Current descriptor field tables. The raw wire tree remains authoritative.
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Kind {
    Int32,
    Uint32,
    Bool,
    Float,
    String,
    Message(&'static str),
    Enum(&'static str),
}

impl Kind {
    pub(crate) fn wire(self) -> u8 {
        match self {
            Self::Int32 | Self::Uint32 | Self::Bool | Self::Enum(_) => 0,
            Self::Float => 5,
            Self::String | Self::Message(_) => 2,
        }
    }

    pub(crate) fn packable(self) -> bool {
        !matches!(self, Self::String | Self::Message(_))
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct FieldSchema {
    pub number: u32,
    pub name: &'static str,
    pub kind: Kind,
    pub repeated: bool,
    pub optional: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MessageSchema {
    pub name: &'static str,
    pub fields: &'static [FieldSchema],
}

#[path = "schema_data.rs"]
mod data;

/// Includes 15 top-level messages and three synthetic map entry messages.
pub fn messages() -> &'static [MessageSchema] {
    data::MESSAGES
}

pub(crate) fn message(name: &str) -> &'static MessageSchema {
    data::MESSAGES
        .iter()
        .find(|m| m.name == name)
        .expect("complete recovered schema")
}

impl MessageSchema {
    pub(crate) fn field(self, number: u32) -> Option<&'static FieldSchema> {
        self.fields.iter().find(|f| f.number == number)
    }
}

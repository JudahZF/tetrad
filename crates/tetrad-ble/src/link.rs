//! The Tetrad Link service: the GATT service of the Tetrad app (docs/bluetooth.md).

use crate::Uuid;

/// `5162a6fe-ad05-405f-b22e-6e4047a5b524`.
pub const SERVICE: Uuid = Uuid::from_u128(0x5162a6fe_ad05_405f_b22e_6e4047a5b524);
/// App → unit: write without response.
pub const REQUEST: Uuid = Uuid::from_u128(0x5162a6fe_ad05_405f_b22e_6e4047a5b525);
/// Unit → app: notify.
pub const REPLY: Uuid = Uuid::from_u128(0x5162a6fe_ad05_405f_b22e_6e4047a5b526);

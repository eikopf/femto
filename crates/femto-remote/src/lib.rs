//! Bounded HID input decoding, page-turn bindings and notification sessions.
//!
//! Bluetooth transport and platform synchronization are supplied by the caller.

pub mod bindings;
mod descriptor;
pub mod discovery;
pub mod gatt;
pub mod inbox;
pub mod input;
pub mod reconnect;
pub mod scan;
pub mod shutdown;
pub mod subscriptions;

pub use bindings::{Bindings, PageTurn};
pub use input::{Presses, RemoteInput};

pub(crate) use descriptor::{frame_ble_input, preflight};

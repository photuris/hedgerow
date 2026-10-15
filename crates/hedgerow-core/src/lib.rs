//! Hedgerow core: every piece of logic in the MeshCore client lives in this
//! crate (model, text rules, config, the SQLite message store, the radio
//! layer, and the session actor), and the GUI is only a view over it.
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod fixtures;
pub mod model;
pub mod notify;
pub mod radio;
pub mod session;
pub mod store;
pub mod text;

pub use error::{Error, Result};

//! Retkerno core: every piece of logic in the MeshCore client lives in this
//! crate (model, text rules, config, and the SQLite message store), and the
//! GUI is only a view over it.
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod model;
pub mod store;
pub mod text;

pub use error::{Error, Result};

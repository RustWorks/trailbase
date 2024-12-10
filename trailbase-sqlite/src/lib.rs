#![allow(clippy::needless_return)]
#![warn(
  unsafe_code,
  clippy::await_holding_lock,
  clippy::empty_enum,
  clippy::enum_glob_use,
  clippy::inefficient_to_string,
  clippy::mem_forget,
  clippy::mutex_integer,
  clippy::needless_continue
)]

mod extension;

pub mod connection;
pub mod error;
pub mod geoip;
pub mod params;
pub mod schema;

pub use connection::{AsyncConnection, Connection, Row, Rows, Value, ValueType};
pub use error::Error;
pub use extension::connect_sqlite;
pub use params::Params;

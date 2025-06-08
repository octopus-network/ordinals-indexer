mod bitcoin_api;
mod chain;
pub mod config;
mod error;
pub mod index;
mod inscriptions;
mod into_usize;
pub mod logs;
mod properties;
pub mod rpc;

use anyhow::Error;
use bitcoin::OutPoint;
use bitcoin::hashes::Hash;
use chrono::{DateTime, TimeZone, Utc};

type Result<T = (), E = Error> = std::result::Result<T, E>;

fn timestamp(seconds: u64) -> DateTime<Utc> {
  Utc
    .timestamp_opt(seconds.try_into().unwrap_or(i64::MAX), 0)
    .unwrap()
}

fn default<T: Default>() -> T {
  Default::default()
}

pub fn unbound_outpoint() -> OutPoint {
  OutPoint {
    txid: Hash::all_zeros(),
    vout: 0,
  }
}

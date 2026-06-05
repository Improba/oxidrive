//! Persistent key-value store ([`RedbStore`]) and in-memory sync session state ([`Store`]).

pub mod db;

mod session;

pub use db::RedbStore;
pub use session::{get_or_create_device_id, Store};

//! Chapter 5: strict two-phase locking and undo-only recovery.
//!
//! `Transaction` exposes the book's pin/unpin, get/set, size/append, commit,
//! rollback, and recover operations. Recovery is separated into `recovery.rs`
//! without a self-reference from a recovery manager back to its transaction.
//!
//! Pass the same `Arc<LockTable>` to every transaction of a database; it replaces
//! Java's static lock table without sharing locks between unrelated databases.
//! At startup, create a transaction, call `recover`, then `commit` before accepting
//! clients. Every client transaction must explicitly commit or roll back, including
//! after timeout errors. Dropping it does not perform disk I/O or commit changes.

mod concurrency;
mod log_record;
mod recovery;
mod transaction;

pub use concurrency::LockTable;
pub use transaction::Transaction;

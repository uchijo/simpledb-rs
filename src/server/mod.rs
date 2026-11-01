//! Database startup: recover, initialize metadata, then admit transactions.
//!
//! ```no_run
//! use std::io;
//! use std::sync::Arc;
//! use simpledb_rs::record::Schema;
//! use simpledb_rs::server::SimpleDb;
//!
//! # fn main() -> io::Result<()> {
//! let database = SimpleDb::new("example-db")?;
//! let transaction = database.new_transaction()?;
//! let mut schema = Schema::new();
//! schema.add_int_field("id");
//! {
//!     let metadata = database.metadata_manager();
//!     metadata.lock().unwrap().create_table("student", schema, Arc::clone(&transaction))?;
//! }
//! transaction.lock().unwrap().commit()?;
//! # Ok(())
//! # }
//! ```

mod simple_db;

pub use simple_db::SimpleDb;

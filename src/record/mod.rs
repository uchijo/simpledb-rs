//! Chapter 6: schemas, fixed-size record slots, and table cursors.
//! Share a transaction through `Arc<Mutex<Transaction>>` and layouts through `Arc`.
//! Pages and scans lock the transaction internally; release transaction guards
//! before calling their methods or dropping a page.
//! Close pages and scans before committing or rolling back their transaction.

mod layout;
mod record_page;
mod rid;
mod schema;
mod table_scan;

pub use layout::Layout;
pub use record_page::RecordPage;
pub use rid::Rid;
pub use schema::{FieldType, Schema};
pub use table_scan::TableScan;

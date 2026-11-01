//! Chapter 7: persistent catalogs and cached, approximate table statistics.
//! Use one metadata manager per database. Catalog changes belong to the caller's
//! transaction; commit or roll back explicitly, including after an error.
//! Acquire the metadata mutex before transaction mutexes; release transaction
//! guards before calling these methods. Successful calls close their scans;
//! transaction locks remain until commit or rollback.

mod index_info;
mod index_manager;
mod metadata_manager;
mod stat_info;
mod stat_manager;
mod table_manager;
mod view_manager;

pub use index_info::IndexInfo;
pub use index_manager::IndexManager;
pub use metadata_manager::MetadataManager;
pub use stat_info::StatInfo;
pub use stat_manager::StatManager;
pub use table_manager::{MAX_NAME, TableManager};
pub use view_manager::{MAX_VIEW_DEFINITION, ViewManager};

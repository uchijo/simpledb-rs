use std::collections::HashMap;
use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use super::{IndexInfo, IndexManager, StatInfo, StatManager, TableManager, ViewManager};
use crate::record::{Layout, Schema};
use crate::sync::lock;
use crate::tx::Transaction;

/// The chapter 7 facade. Share through a mutex when multiple clients use it.
pub struct MetadataManager {
    tables: Arc<TableManager>,
    views: ViewManager,
    statistics: StatManager,
    indexes: IndexManager,
}

impl MetadataManager {
    pub fn new(is_new: bool, transaction: Arc<Mutex<Transaction>>) -> io::Result<Self> {
        Self::check_block_size(lock(&transaction)?.block_size()?)?;
        let tables = Arc::new(TableManager::new(is_new, Arc::clone(&transaction))?);
        let views = ViewManager::new(is_new, &tables, Arc::clone(&transaction))?;
        let statistics = StatManager::new(Arc::clone(&tables), Arc::clone(&transaction))?;
        let indexes = IndexManager::new(is_new, Arc::clone(&tables), transaction)?;
        Ok(Self {
            tables,
            views,
            statistics,
            indexes,
        })
    }

    pub fn create_table(
        &self,
        table_name: &str,
        schema: Schema,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<()> {
        self.tables.create_table(table_name, schema, transaction)
    }

    pub fn get_layout(
        &self,
        table_name: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<Layout> {
        self.tables.get_layout(table_name, transaction)
    }

    pub fn create_view(
        &self,
        view_name: &str,
        definition: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<()> {
        self.views.create_view(view_name, definition, transaction)
    }

    pub fn get_view_def(
        &self,
        view_name: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<Option<String>> {
        self.views.get_view_def(view_name, transaction)
    }

    pub fn create_index(
        &self,
        index_name: &str,
        table_name: &str,
        field_name: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<()> {
        self.indexes
            .create_index(index_name, table_name, field_name, transaction)
    }

    pub fn get_index_info(
        &mut self,
        table_name: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<HashMap<String, IndexInfo>> {
        self.indexes
            .get_index_info(table_name, &mut self.statistics, transaction)
    }

    pub fn get_stat_info(
        &mut self,
        table_name: &str,
        layout: Arc<Layout>,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<StatInfo> {
        self.statistics
            .get_stat_info(table_name, layout, transaction)
    }

    pub(crate) fn check_block_size(block_size: usize) -> io::Result<()> {
        // Check the largest catalog before creating any catalog records.
        if Layout::new(ViewManager::schema())?.slot_size() > block_size {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "view catalog record exceeds block size",
            ));
        }
        Ok(())
    }
}

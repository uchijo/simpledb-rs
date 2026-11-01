use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use super::{StatInfo, TableManager};
use crate::record::{Layout, TableScan};
use crate::tx::Transaction;

const REFRESH_CALL_LIMIT: usize = 100;

pub struct StatManager {
    tables: Arc<TableManager>,
    table_stats: HashMap<String, StatInfo>,
    calls_since_refresh: usize,
}

impl StatManager {
    pub fn new(
        tables: Arc<TableManager>,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<Self> {
        let mut manager = Self {
            tables,
            table_stats: HashMap::new(),
            calls_since_refresh: 0,
        };
        manager.refresh_statistics(transaction)?;
        Ok(manager)
    }

    /// Estimates may remain stale until the 101st request, as in the book.
    pub fn get_stat_info(
        &mut self,
        table_name: &str,
        layout: Arc<Layout>,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<StatInfo> {
        self.calls_since_refresh += 1;
        if self.calls_since_refresh > REFRESH_CALL_LIMIT {
            self.refresh_statistics(Arc::clone(&transaction))?;
        }
        if let Some(info) = self.table_stats.get(table_name) {
            return Ok(*info);
        }
        let info = Self::calculate_table_stats(table_name, layout, transaction)?;
        self.table_stats.insert(table_name.to_owned(), info);
        Ok(info)
    }

    fn refresh_statistics(&mut self, transaction: Arc<Mutex<Transaction>>) -> io::Result<()> {
        let layout = Arc::new(self.tables.get_layout("tblcat", Arc::clone(&transaction))?);
        let mut catalog = TableScan::new(Arc::clone(&transaction), "tblcat", layout)?;
        let mut table_names = Vec::new();
        while catalog.next()? {
            table_names.push(catalog.get_string("tblname")?);
        }
        // Release the catalog pin before opening other scans, even with a 1-buffer pool.
        catalog.close()?;

        let mut table_stats = HashMap::new();
        for table_name in table_names {
            let layout = Arc::new(
                self.tables
                    .get_layout(&table_name, Arc::clone(&transaction))?,
            );
            let info = Self::calculate_table_stats(&table_name, layout, Arc::clone(&transaction))?;
            table_stats.insert(table_name, info);
        }
        self.table_stats = table_stats;
        self.calls_since_refresh = 0;
        Ok(())
    }

    fn calculate_table_stats(
        table_name: &str,
        layout: Arc<Layout>,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<StatInfo> {
        let mut scan = TableScan::new(transaction, table_name, layout)?;
        let mut record_count = 0;
        let mut block_count = 0;
        while scan.next()? {
            record_count += 1;
            // The book counts through the last occupied block, excluding empty trailing blocks.
            block_count = scan.current_rid()?.block_number() + 1;
        }
        scan.close()?;
        Ok(StatInfo::new(block_count, record_count))
    }
}

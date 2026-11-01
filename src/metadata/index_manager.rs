use std::collections::HashMap;
use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use super::index_info::create_index_layout;
use super::table_manager::validate_name;
use super::{IndexInfo, MAX_NAME, StatManager, TableManager};
use crate::record::{Layout, Schema, TableScan};
use crate::sync::lock;
use crate::tx::Transaction;

pub struct IndexManager {
    tables: Arc<TableManager>,
    layout: Arc<Layout>,
}

impl IndexManager {
    pub fn new(
        is_new: bool,
        tables: Arc<TableManager>,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<Self> {
        if is_new {
            let mut schema = Schema::new();
            schema.add_string_field("indexname", MAX_NAME);
            schema.add_string_field("tablename", MAX_NAME);
            schema.add_string_field("fieldname", MAX_NAME);
            tables.create_table("idxcat", schema, Arc::clone(&transaction))?;
        }
        let layout = Arc::new(tables.get_layout("idxcat", transaction)?);
        Ok(Self { tables, layout })
    }

    /// Saves a definition; building and opening index files is introduced in chapter 12.
    pub fn create_index(
        &self,
        index_name: &str,
        table_name: &str,
        field_name: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<()> {
        validate_name(index_name)?;
        validate_name(table_name)?;
        validate_name(field_name)?;
        let table_layout = self
            .tables
            .get_layout(table_name, Arc::clone(&transaction))?;
        create_index_layout(
            table_layout.schema(),
            field_name,
            lock(&transaction)?.block_size()?,
        )?;

        let mut indexes = TableScan::new(transaction, "idxcat", Arc::clone(&self.layout))?;
        while indexes.next()? {
            if indexes.get_string("indexname")? == index_name {
                return Err(Error::new(ErrorKind::AlreadyExists, "index already exists"));
            }
        }
        indexes.before_first()?;
        indexes.insert()?;
        indexes.set_string("indexname", index_name)?;
        indexes.set_string("tablename", table_name)?;
        indexes.set_string("fieldname", field_name)?;
        indexes.close()
    }

    pub fn get_index_info(
        &self,
        table_name: &str,
        statistics: &mut StatManager,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<HashMap<String, IndexInfo>> {
        let mut indexes =
            TableScan::new(Arc::clone(&transaction), "idxcat", Arc::clone(&self.layout))?;
        let mut definitions = Vec::new();
        while indexes.next()? {
            if indexes.get_string("tablename")? == table_name {
                definitions.push((
                    indexes.get_string("indexname")?,
                    indexes.get_string("fieldname")?,
                ));
            }
        }
        indexes.close()?;

        let mut result = HashMap::new();
        if definitions.is_empty() {
            return Ok(result);
        }
        let layout = Arc::new(
            self.tables
                .get_layout(table_name, Arc::clone(&transaction))?,
        );
        let stats =
            statistics.get_stat_info(table_name, Arc::clone(&layout), Arc::clone(&transaction))?;
        let block_size = lock(&transaction)?.block_size()?;
        for (index_name, field_name) in definitions {
            let info =
                IndexInfo::new(&index_name, &field_name, layout.schema(), block_size, stats)?;
            result.insert(field_name, info);
        }
        Ok(result)
    }
}

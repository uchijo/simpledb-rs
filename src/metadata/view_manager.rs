use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use super::table_manager::validate_name;
use super::{MAX_NAME, TableManager};
use crate::record::{Layout, Schema, TableScan};
use crate::sync::lock;
use crate::tx::Transaction;

pub const MAX_VIEW_DEFINITION: usize = 100;

pub struct ViewManager {
    layout: Arc<Layout>,
}

impl ViewManager {
    pub fn new(
        is_new: bool,
        tables: &TableManager,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<Self> {
        let schema = Self::schema();
        if Layout::new(schema.clone())?.slot_size() > lock(&transaction)?.block_size()? {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "view catalog record exceeds block size",
            ));
        }
        if is_new {
            tables.create_table("viewcat", schema, Arc::clone(&transaction))?;
        }
        Ok(Self {
            layout: Arc::new(tables.get_layout("viewcat", transaction)?),
        })
    }

    pub fn create_view(
        &self,
        view_name: &str,
        definition: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<()> {
        validate_name(view_name)?;
        if definition.chars().count() > MAX_VIEW_DEFINITION {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "view definition exceeds 100 characters",
            ));
        }
        let mut views = TableScan::new(transaction, "viewcat", Arc::clone(&self.layout))?;
        while views.next()? {
            if views.get_string("viewname")? == view_name {
                return Err(Error::new(ErrorKind::AlreadyExists, "view already exists"));
            }
        }
        views.before_first()?;
        views.insert()?;
        views.set_string("viewname", view_name)?;
        views.set_string("viewdef", definition)?;
        views.close()
    }

    pub fn get_view_def(
        &self,
        view_name: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<Option<String>> {
        let mut views = TableScan::new(transaction, "viewcat", Arc::clone(&self.layout))?;
        let mut definition = None;
        while views.next()? {
            if views.get_string("viewname")? == view_name {
                definition = Some(views.get_string("viewdef")?);
                break;
            }
        }
        views.close()?;
        Ok(definition)
    }

    pub(super) fn schema() -> Schema {
        let mut schema = Schema::new();
        schema.add_string_field("viewname", MAX_NAME);
        schema.add_string_field("viewdef", MAX_VIEW_DEFINITION);
        schema
    }
}

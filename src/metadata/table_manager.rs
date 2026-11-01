use std::collections::HashMap;
use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use crate::record::{FieldType, Layout, Schema, TableScan};
use crate::sync::lock;
use crate::tx::Transaction;

pub const MAX_NAME: usize = 16;
// java.sql.Types values used by the book's catalogs.
const INTEGER_TYPE: i32 = 4;
const VARCHAR_TYPE: i32 = 12;

pub struct TableManager {
    table_catalog_layout: Arc<Layout>,
    field_catalog_layout: Arc<Layout>,
}

impl TableManager {
    pub fn new(is_new: bool, transaction: Arc<Mutex<Transaction>>) -> io::Result<Self> {
        let mut table_catalog_schema = Schema::new();
        table_catalog_schema.add_string_field("tblname", MAX_NAME);
        table_catalog_schema.add_int_field("slotsize");

        let mut field_catalog_schema = Schema::new();
        field_catalog_schema.add_string_field("tblname", MAX_NAME);
        field_catalog_schema.add_string_field("fldname", MAX_NAME);
        field_catalog_schema.add_int_field("type");
        field_catalog_schema.add_int_field("length");
        field_catalog_schema.add_int_field("offset");

        let manager = Self {
            table_catalog_layout: Arc::new(Layout::new(table_catalog_schema)?),
            field_catalog_layout: Arc::new(Layout::new(field_catalog_schema)?),
        };
        let block_size = lock(&transaction)?.block_size()?;
        if manager.field_catalog_layout.slot_size() > block_size {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "catalog record exceeds block size",
            ));
        }
        if is_new {
            manager.create_table(
                "tblcat",
                manager.table_catalog_layout.schema().clone(),
                Arc::clone(&transaction),
            )?;
            manager.create_table(
                "fldcat",
                manager.field_catalog_layout.schema().clone(),
                transaction,
            )?;
        }
        Ok(manager)
    }

    pub fn create_table(
        &self,
        table_name: &str,
        schema: Schema,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<()> {
        validate_name(table_name)?;
        if table_name.starts_with("temp") {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "table names starting with temp are reserved",
            ));
        }
        let layout = Layout::new(schema)?;
        if layout.slot_size() > lock(&transaction)?.block_size()? {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "record does not fit in a block",
            ));
        }
        let slot_size = catalog_int(layout.slot_size())?;
        // Validate every field before inserting any catalog rows.
        for field_name in layout.schema().fields() {
            validate_name(field_name)?;
            catalog_int(layout.schema().length(field_name)?)?;
            catalog_int(layout.offset(field_name)?)?;
        }

        let mut table_catalog = TableScan::new(
            Arc::clone(&transaction),
            "tblcat",
            Arc::clone(&self.table_catalog_layout),
        )?;
        while table_catalog.next()? {
            if table_catalog.get_string("tblname")? == table_name {
                return Err(Error::new(ErrorKind::AlreadyExists, "table already exists"));
            }
        }
        table_catalog.before_first()?;
        table_catalog.insert()?;
        table_catalog.set_string("tblname", table_name)?;
        table_catalog.set_int("slotsize", slot_size)?;
        table_catalog.close()?;

        let mut field_catalog = TableScan::new(
            transaction,
            "fldcat",
            Arc::clone(&self.field_catalog_layout),
        )?;
        for field_name in layout.schema().fields() {
            field_catalog.insert()?;
            field_catalog.set_string("tblname", table_name)?;
            field_catalog.set_string("fldname", field_name)?;
            let type_code = match layout.schema().field_type(field_name)? {
                FieldType::Integer => INTEGER_TYPE,
                FieldType::Varchar(_) => VARCHAR_TYPE,
            };
            field_catalog.set_int("type", type_code)?;
            let length = catalog_int(layout.schema().length(field_name)?)?;
            field_catalog.set_int("length", length)?;
            let offset = catalog_int(layout.offset(field_name)?)?;
            field_catalog.set_int("offset", offset)?;
        }
        field_catalog.close()
    }

    pub fn get_layout(
        &self,
        table_name: &str,
        transaction: Arc<Mutex<Transaction>>,
    ) -> io::Result<Layout> {
        let mut table_catalog = TableScan::new(
            Arc::clone(&transaction),
            "tblcat",
            Arc::clone(&self.table_catalog_layout),
        )?;
        let mut slot_size = None;
        while table_catalog.next()? {
            if table_catalog.get_string("tblname")? == table_name {
                slot_size = Some(catalog_usize(table_catalog.get_int("slotsize")?)?);
                break;
            }
        }
        table_catalog.close()?;
        let slot_size = slot_size.ok_or_else(|| {
            Error::new(ErrorKind::NotFound, format!("unknown table: {table_name}"))
        })?;

        let mut schema = Schema::new();
        let mut offsets = HashMap::new();
        let mut field_catalog = TableScan::new(
            transaction,
            "fldcat",
            Arc::clone(&self.field_catalog_layout),
        )?;
        while field_catalog.next()? {
            if field_catalog.get_string("tblname")? != table_name {
                continue;
            }
            let field_name = field_catalog.get_string("fldname")?;
            let length = catalog_usize(field_catalog.get_int("length")?)?;
            let field_type = match field_catalog.get_int("type")? {
                INTEGER_TYPE if length == 0 => FieldType::Integer,
                VARCHAR_TYPE => FieldType::Varchar(length),
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "invalid catalog field type",
                    ));
                }
            };
            let offset = catalog_usize(field_catalog.get_int("offset")?)?;
            if offsets.insert(field_name.clone(), offset).is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "duplicate catalog field",
                ));
            }
            schema.add_field(field_name, field_type);
        }
        field_catalog.close()?;
        Layout::from_parts(schema, offsets, slot_size)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))
    }
}

pub(super) fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty() || name.chars().count() > MAX_NAME || name.contains(['/', '\\', '\0']) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid catalog name (maximum 16 characters)",
        ));
    }
    Ok(())
}

fn catalog_int(value: usize) -> io::Result<i32> {
    i32::try_from(value)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "catalog value exceeds i32"))
}

fn catalog_usize(value: i32) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| Error::new(ErrorKind::InvalidData, "negative catalog value"))
}

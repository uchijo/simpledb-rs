use std::collections::HashMap;
use std::io::{self, Error, ErrorKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Integer,
    /// Maximum character count (Unicode scalar values), encoded as UTF-8.
    Varchar(usize),
}

/// Logical fields in declaration order. Varchar lengths count Unicode scalar values.
#[derive(Clone, Debug, Default)]
pub struct Schema {
    fields: Vec<String>,
    field_types: HashMap<String, FieldType>,
}

impl Schema {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replacing an existing field's type preserves its declaration position.
    pub fn add_field(&mut self, field_name: impl Into<String>, field_type: FieldType) {
        let field_name = field_name.into();
        if !self.field_types.contains_key(&field_name) {
            self.fields.push(field_name.clone());
        }
        self.field_types.insert(field_name, field_type);
    }

    pub fn add_int_field(&mut self, field_name: impl Into<String>) {
        self.add_field(field_name, FieldType::Integer);
    }

    pub fn add_string_field(&mut self, field_name: impl Into<String>, char_count: usize) {
        self.add_field(field_name, FieldType::Varchar(char_count));
    }

    pub fn add(&mut self, field_name: &str, source: &Schema) -> io::Result<()> {
        self.add_field(field_name, source.field_type(field_name)?);
        Ok(())
    }

    pub fn add_all(&mut self, source: &Schema) {
        for field_name in source.fields() {
            self.add_field(field_name.clone(), source.field_types[field_name]);
        }
    }

    pub fn fields(&self) -> &[String] {
        &self.fields
    }

    pub fn has_field(&self, field_name: &str) -> bool {
        self.field_types.contains_key(field_name)
    }

    pub fn field_type(&self, field_name: &str) -> io::Result<FieldType> {
        self.field_types.get(field_name).copied().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("unknown field: {field_name}"),
            )
        })
    }

    pub fn length(&self, field_name: &str) -> io::Result<usize> {
        Ok(match self.field_type(field_name)? {
            FieldType::Integer => 0,
            FieldType::Varchar(length) => length,
        })
    }
}

use crate::error::{Error, Result};

const MAX_COLLECTION_NAME: usize = 64;
const MAX_ID: usize = 128;

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
}

fn is_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn validate_group_name(kind: &str, name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_COLLECTION_NAME
        && bytes[0] != b'_'
        && bytes.iter().copied().all(is_name_byte);
    if valid {
        Ok(())
    } else {
        Err(Error::invalid_name(kind, name))
    }
}

pub fn validate_db_name(name: &str) -> Result<()> {
    validate_group_name("db", name)
}

pub fn validate_collection_name(name: &str) -> Result<()> {
    validate_group_name("collection", name)
}

fn refuse_leading_dash(kind: &str, name: &str) -> Result<()> {
    if name.starts_with('-') {
        Err(Error::invalid_name(kind, name).with_detail("reason", "leading_dash"))
    } else {
        Ok(())
    }
}

pub fn validate_new_db_name(name: &str) -> Result<()> {
    validate_db_name(name)?;
    refuse_leading_dash("db", name)
}

pub fn validate_new_collection_name(name: &str) -> Result<()> {
    validate_collection_name(name)?;
    refuse_leading_dash("collection", name)
}

pub fn validate_id(id: &str) -> Result<()> {
    let bytes = id.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_ID
        && bytes[0] != b'_'
        && bytes[0] != b'-'
        && bytes.iter().copied().all(is_id_byte);
    if valid {
        Ok(())
    } else {
        Err(Error::invalid_name("id", id))
    }
}

pub fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

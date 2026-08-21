//! Encodes and decodes `TableInfo` rows for the catalog's own persisted
//! table heap, using `types`'s tuple encoder against a fixed row schema:
//! `(table_id INTEGER, name TEXT, first_page INTEGER, schema_blob TEXT)`.
//!
//! `schema_blob` is this module's own hand-rolled encoding of a `Schema` —
//! column count, then per column its name, type tag, `Varchar` length (if
//! applicable), and nullability — hex-encoded so it round-trips safely
//! through a `TEXT` column, which requires valid UTF-8. No serde.

use common::{PageId, TableId};
use types::{DataType, Encode, Tuple, Value};

use crate::column::Column;
use crate::error::CatalogError;
use crate::schema::Schema;
use crate::table_info::TableInfo;

/// The fixed column types of a catalog row, in the order fields are
/// encoded. The `Varchar` length parameter is not meaningful for decoding
/// (only the length-prefixed bytes matter), so it's left at 0.
const CATALOG_ROW_SCHEMA: [DataType; 4] =
    [DataType::Integer, DataType::Varchar(0), DataType::Integer, DataType::Varchar(0)];

fn type_tag(data_type: DataType) -> u8 {
    match data_type {
        DataType::Boolean => 0,
        DataType::Integer => 1,
        DataType::BigInt => 2,
        DataType::Double => 3,
        DataType::Varchar(_) => 4,
    }
}

fn tag_to_type(tag: u8, varchar_len: u32) -> Result<DataType, CatalogError> {
    match tag {
        0 => Ok(DataType::Boolean),
        1 => Ok(DataType::Integer),
        2 => Ok(DataType::BigInt),
        3 => Ok(DataType::Double),
        4 => Ok(DataType::Varchar(varchar_len)),
        other => Err(CatalogError::Corrupt(format!("unknown column type tag {other}"))),
    }
}

fn encode_schema_blob(schema: &Schema) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(schema.columns().len() as u32).to_le_bytes());
    for column in schema.columns() {
        let name_bytes = column.name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(name_bytes);
        buf.push(type_tag(column.data_type));
        if let DataType::Varchar(len) = column.data_type {
            buf.extend_from_slice(&len.to_le_bytes());
        }
        buf.push(u8::from(column.nullable));
    }
    to_hex(&buf)
}

fn decode_schema_blob(blob: &str) -> Result<Schema, CatalogError> {
    let bytes = from_hex(blob)?;
    let mut offset = 0usize;

    let count_bytes = take(&bytes, offset, 4)?;
    let count =
        u32::from_le_bytes([count_bytes[0], count_bytes[1], count_bytes[2], count_bytes[3]]);
    offset += 4;

    let mut columns = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name_len_bytes = take(&bytes, offset, 4)?;
        let name_len = u32::from_le_bytes([
            name_len_bytes[0],
            name_len_bytes[1],
            name_len_bytes[2],
            name_len_bytes[3],
        ]) as usize;
        offset += 4;

        let name_bytes = take(&bytes, offset, name_len)?;
        let name = String::from_utf8(name_bytes.to_vec())
            .map_err(|_| CatalogError::Corrupt("invalid utf-8 column name".to_string()))?;
        offset += name_len;

        let tag = take(&bytes, offset, 1)?[0];
        offset += 1;

        let varchar_len = if tag == 4 {
            let len_bytes = take(&bytes, offset, 4)?;
            offset += 4;
            u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
        } else {
            0
        };
        let data_type = tag_to_type(tag, varchar_len)?;

        let nullable = take(&bytes, offset, 1)?[0] != 0;
        offset += 1;

        columns.push(Column::new(name, data_type, nullable));
    }

    Ok(Schema::new(columns))
}

/// Encodes `info` as a catalog row's tuple bytes.
pub(crate) fn encode_table_info(info: &TableInfo) -> Vec<u8> {
    let tuple = Tuple::new(vec![
        Value::Integer(info.table_id.0 as i32),
        Value::Varchar(info.name.clone()),
        Value::Integer(info.first_page_id.0 as i32),
        Value::Varchar(encode_schema_blob(&info.schema)),
    ]);
    let mut buf = Vec::new();
    tuple.encode(&mut buf);
    buf
}

/// Decodes a catalog row's tuple bytes back into a `TableInfo`.
pub(crate) fn decode_table_info(bytes: &[u8]) -> Result<TableInfo, CatalogError> {
    let tuple = Tuple::decode(bytes, &CATALOG_ROW_SCHEMA)
        .map_err(|err| CatalogError::Corrupt(err.to_string()))?;
    let values = tuple.values();

    let (
        Value::Integer(table_id),
        Value::Varchar(name),
        Value::Integer(first_page),
        Value::Varchar(blob),
    ) = (&values[0], &values[1], &values[2], &values[3])
    else {
        return Err(CatalogError::Corrupt("malformed catalog row".to_string()));
    };

    let schema = decode_schema_blob(blob)?;
    Ok(TableInfo::new(TableId(*table_id as u32), name.clone(), schema, PageId(*first_page as u32)))
}

fn take(bytes: &[u8], offset: usize, len: usize) -> Result<&[u8], CatalogError> {
    bytes
        .get(offset..offset + len)
        .ok_or_else(|| CatalogError::Corrupt("truncated schema blob".to_string()))
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        out.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn from_hex(text: &str) -> Result<Vec<u8>, CatalogError> {
    let digits = text.as_bytes();
    if digits.len() % 2 != 0 {
        return Err(CatalogError::Corrupt("odd-length hex blob".to_string()));
    }
    let mut out = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks_exact(2) {
        out.push((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?);
    }
    Ok(out)
}

fn hex_digit(digit: u8) -> Result<u8, CatalogError> {
    match digit {
        b'0'..=b'9' => Ok(digit - b'0'),
        b'a'..=b'f' => Ok(digit - b'a' + 10),
        _ => Err(CatalogError::Corrupt("invalid hex digit in schema blob".to_string())),
    }
}

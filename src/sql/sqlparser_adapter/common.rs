use crate::error::{KkdbError, Result};
use crate::types::DataType;
use sqlparser::ast as sa;

pub(super) fn unsupported(feature: impl Into<String>) -> KkdbError {
    KkdbError::ParseError(format!("unsupported SQL feature: {}", feature.into()))
}

pub(super) fn object_name_to_string(name: &sa::ObjectName) -> String {
    name.to_string()
}

pub(super) fn object_name_last_ident(name: &sa::ObjectName) -> Result<String> {
    for part in name.0.iter().rev() {
        if let Some(ident) = part.as_ident() {
            return Ok(ident.value.clone());
        }
    }
    Err(unsupported(format!("invalid object name `{name}`")))
}

pub(super) fn table_alias_to_string(alias: Option<sa::TableAlias>) -> Option<String> {
    alias.map(|a| a.name.value)
}

pub(super) fn convert_data_type(data_type: sa::DataType) -> DataType {
    if data_type == sa::DataType::Unspecified {
        return DataType::Blob;
    }

    let raw = data_type.to_string();
    let head: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .collect();

    if head.is_empty() {
        DataType::Text
    } else {
        DataType::from_str(&head)
    }
}

pub(super) fn parse_number_literal(raw: &str) -> Result<crate::sql::ast::Expr> {
    let is_real = raw.contains('.') || raw.contains('e') || raw.contains('E');
    if !is_real {
        if let Ok(v) = raw.parse::<i64>() {
            return Ok(crate::sql::ast::Expr::IntegerLiteral(v));
        }
    }
    let v = raw
        .parse::<f64>()
        .map_err(|_| KkdbError::ParseError(format!("invalid numeric literal `{raw}`")))?;
    Ok(crate::sql::ast::Expr::RealLiteral(v))
}

pub(super) fn parse_blob_hex(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return Err(KkdbError::ParseError(format!(
            "invalid blob literal (odd hex length): `{hex}`"
        )));
    }

    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let part = std::str::from_utf8(&bytes[i..i + 2]).map_err(|_| {
            KkdbError::ParseError(format!("invalid blob literal utf8 sequence: `{hex}`"))
        })?;
        let b = u8::from_str_radix(part, 16).map_err(|_| {
            KkdbError::ParseError(format!("invalid blob literal hex pair `{part}`"))
        })?;
        out.push(b);
        i += 2;
    }
    Ok(out)
}

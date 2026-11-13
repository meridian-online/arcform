//! A table's Frictionless descriptor as the rows the lake's `meta.descriptors` and
//! `meta.fields` tables hold.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::error::{Error, Result};

/// The reserved schema holding the lake's descriptor tables. A table may not be
/// published into it.
pub const META_SCHEMA: &str = "meta";

/// One table's descriptor, ready to be written to `meta.descriptors` and `meta.fields`.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDescriptor {
    pub table_name: String,
    /// The descriptor as stored: the package without the volatile `created`, its
    /// resource without `bytes` and `hash`.
    pub descriptor: Value,
    pub fields: Vec<FieldRow>,
    pub key: Vec<String>,
    pub table_comment: Option<String>,
}

/// One column of a table, as `meta.fields` holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldRow {
    pub column_name: String,
    pub ordinal: i32,
    pub r#type: String,
    pub finetype_label: Option<String>,
    pub description: Option<String>,
    pub constraints: Value,
    pub is_key: bool,
}

pub(super) fn failed(msg: impl std::fmt::Display) -> Error {
    Error::StepFailed {
        step: String::new(),
        code: 1,
        stderr: format!("ducklake_write: {msg}"),
    }
}

/// Reads `datapackage`'s single resource into the rows `meta` holds for `table_name`,
/// checking its field names against the Parquet file's `parquet_columns`.
pub fn table_descriptor(
    table_name: &str,
    datapackage: &Value,
    parquet_columns: &[String],
) -> Result<TableDescriptor> {
    let resources = datapackage
        .get("resources")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if resources.len() != 1 {
        return Err(failed(format!(
            "the descriptor for table '{table_name}' must have exactly one resource, found {}",
            resources.len()
        )));
    }
    let schema = &resources[0]["schema"];
    let raw_fields = schema
        .get("fields")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            failed(format!(
                "the descriptor for table '{table_name}' has no schema.fields"
            ))
        })?;

    let key: Vec<String> = match schema.get("primaryKey") {
        Some(Value::String(k)) => vec![k.clone()],
        Some(Value::Array(ks)) => ks
            .iter()
            .map(|k| {
                k.as_str().map(String::from).ok_or_else(|| {
                    failed(format!(
                        "the primaryKey of table '{table_name}' has an entry that is not a string: {k}"
                    ))
                })
            })
            .collect::<Result<_>>()?,
        None | Some(Value::Null) => Vec::new(),
        Some(other) => {
            return Err(failed(format!(
                "the primaryKey of table '{table_name}' must be a string or an array of strings, found {other}"
            )));
        }
    };

    let mut fields = Vec::with_capacity(raw_fields.len());
    for (i, f) in raw_fields.iter().enumerate() {
        let name = f.get("name").and_then(Value::as_str).ok_or_else(|| {
            failed(format!(
                "field {i} of the descriptor for table '{table_name}' has no name"
            ))
        })?;
        fields.push(FieldRow {
            column_name: name.to_string(),
            ordinal: i32::try_from(i).map_err(|_| failed("too many fields"))?,
            r#type: f
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("any")
                .to_string(),
            finetype_label: f
                .get("x-finetype-label")
                .and_then(Value::as_str)
                .map(String::from),
            description: f
                .get("description")
                .and_then(Value::as_str)
                .map(String::from),
            constraints: f
                .get("constraints")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default())),
            is_key: key.iter().any(|k| k == name),
        });
    }

    let mut seen = BTreeSet::new();
    for f in &fields {
        if !seen.insert(f.column_name.as_str()) {
            return Err(failed(format!(
                "the descriptor for table '{table_name}' lists field '{}' more than once",
                f.column_name
            )));
        }
    }
    if let Some(missing) = key.iter().find(|k| !seen.contains(k.as_str())) {
        return Err(failed(format!(
            "the primaryKey of table '{table_name}' names '{missing}', which is not in schema.fields"
        )));
    }

    let described: BTreeSet<&str> = fields.iter().map(|f| f.column_name.as_str()).collect();
    let in_file: BTreeSet<&str> = parquet_columns.iter().map(String::as_str).collect();
    if described != in_file {
        let list = |s: BTreeSet<&str>| s.into_iter().collect::<Vec<_>>().join(", ");
        return Err(failed(format!(
            "the descriptor for table '{table_name}' disagrees with its Parquet file: \
             descriptor only: [{}]; file only: [{}]",
            list(described.difference(&in_file).copied().collect()),
            list(in_file.difference(&described).copied().collect()),
        )));
    }

    let mut descriptor = datapackage.clone();
    if let Some(o) = descriptor.as_object_mut() {
        o.remove("created");
    }
    if let Some(r) = descriptor["resources"][0].as_object_mut() {
        r.remove("bytes");
        r.remove("hash");
    }

    Ok(TableDescriptor {
        table_name: table_name.to_string(),
        descriptor,
        fields,
        key,
        table_comment: datapackage
            .get("description")
            .and_then(Value::as_str)
            .map(String::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn sample() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/lake/soc_sample.datapackage.json"
        ))
        .unwrap()
    }

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fields_come_from_the_descriptor_in_order() {
        let d =
            table_descriptor("soc", &sample(), &cols(&["level", "definition", "code"])).unwrap();
        assert_eq!(d.table_name, "soc");
        let names: Vec<_> = d.fields.iter().map(|f| f.column_name.as_str()).collect();
        assert_eq!(names, ["code", "level", "definition"]);
        let ords: Vec<_> = d.fields.iter().map(|f| f.ordinal).collect();
        assert_eq!(ords, [0, 1, 2]);
        let code = &d.fields[0];
        assert!(code.is_key);
        assert!(!d.fields[1].is_key);
        assert_eq!(code.r#type, "string");
        assert_eq!(
            code.finetype_label.as_deref(),
            Some("representation.identifier.numeric_code")
        );
        assert_eq!(code.constraints["pattern"], "^[0-9]{2}-[0-9]{4}$");
        assert_eq!(d.key, ["code"]);
        assert!(d.table_comment.is_some());
        assert!(code.description.is_some());
    }

    #[test]
    fn the_stored_descriptor_drops_bytes_hash_and_created() {
        let d =
            table_descriptor("soc", &sample(), &cols(&["code", "level", "definition"])).unwrap();
        assert!(d.descriptor.get("created").is_none());
        let r = &d.descriptor["resources"][0];
        assert!(r.get("bytes").is_none());
        assert!(r.get("hash").is_none());
        assert_eq!(r["format"], "parquet");
    }

    #[test]
    fn a_string_primary_key_is_one_column() {
        let mut p = sample();
        p["resources"][0]["schema"]["primaryKey"] = json!("code");
        let d = table_descriptor("soc", &p, &cols(&["code", "level", "definition"])).unwrap();
        assert_eq!(d.key, ["code"]);
        assert!(d.fields[0].is_key);
    }

    #[test]
    fn a_descriptor_naming_a_missing_column_is_refused() {
        let err = table_descriptor("soc", &sample(), &cols(&["code", "level", "extra_in_file"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("definition"), "{err}");
        assert!(err.contains("extra_in_file"), "{err}");
    }

    #[test]
    fn a_package_with_two_resources_is_refused() {
        let mut p = sample();
        let r = p["resources"][0].clone();
        p["resources"].as_array_mut().unwrap().push(r);
        assert!(table_descriptor("soc", &p, &cols(&["code", "level", "definition"])).is_err());
    }

    #[test]
    fn a_primary_key_naming_a_missing_field_is_refused() {
        let mut p = sample();
        p["resources"][0]["schema"]["primaryKey"] = json!(["nope"]);
        let err = table_descriptor("soc", &p, &cols(&["code", "level", "definition"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn a_non_string_primary_key_entry_is_refused() {
        let mut p = sample();
        p["resources"][0]["schema"]["primaryKey"] = json!([1]);
        let err = table_descriptor("soc", &p, &cols(&["code", "level", "definition"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a string"), "{err}");
    }

    #[test]
    fn a_repeated_field_name_is_refused() {
        let mut p = sample();
        let f = p["resources"][0]["schema"]["fields"][0].clone();
        p["resources"][0]["schema"]["fields"]
            .as_array_mut()
            .unwrap()
            .push(f);
        let err = table_descriptor("soc", &p, &cols(&["code", "level", "definition"]))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'code'") && err.contains("more than once"),
            "{err}"
        );
    }

    #[test]
    fn a_resource_without_fields_is_refused() {
        let mut p = sample();
        p["resources"][0]["schema"]
            .as_object_mut()
            .unwrap()
            .remove("fields");
        assert!(table_descriptor("soc", &p, &cols(&["code"])).is_err());
    }
}

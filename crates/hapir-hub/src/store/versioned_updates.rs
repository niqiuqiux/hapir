use rusqlite::Connection;
use serde_json::Value;

use super::types::VersionedUpdateResult;

fn safe_json_parse(value: Option<String>) -> Option<Value> {
    value.and_then(|s| serde_json::from_str(&s).ok())
}

/// Generic optimistic-concurrency update for a versioned JSON field.
pub fn update_versioned_field(
    conn: &Connection,
    table: &str,
    id: &str,
    namespace: &str,
    field: &str,
    version_field: &str,
    expected_version: i64,
    encoded_value: Option<&str>,
    extra_set_clauses: &[String],
    extra_params: &[(&str, &dyn rusqlite::types::ToSql)],
) -> VersionedUpdateResult<Option<Value>> {
    let result = (|| -> Result<VersionedUpdateResult<Option<Value>>, rusqlite::Error> {
        // Build SET clause
        let mut set_parts = vec![
            format!("{field} = :field_value"),
            format!("{version_field} = {version_field} + 1"),
        ];
        set_parts.extend(extra_set_clauses.iter().cloned());

        let sql = format!(
            "UPDATE {table} SET {} WHERE id = :id AND namespace = :namespace AND {version_field} = :expected_version",
            set_parts.join(", ")
        );

        let mut stmt = conn.prepare(&sql)?;

        // Build named params
        let mut named: Vec<(&str, &dyn rusqlite::types::ToSql)> = vec![
            (":field_value", &encoded_value),
            (":id", &id),
            (":namespace", &namespace),
            (":expected_version", &expected_version),
        ];
        named.extend_from_slice(extra_params);

        let changes = stmt.execute(named.as_slice())?;

        if changes == 1 {
            let decoded = encoded_value.and_then(|s| serde_json::from_str(s).ok());
            return Ok(VersionedUpdateResult::Success {
                version: expected_version + 1,
                value: decoded,
            });
        }

        // Version mismatch — read current
        let select_sql = format!(
            "SELECT {field}, {version_field} FROM {table} WHERE id = :id AND namespace = :namespace"
        );
        let mut select_stmt = conn.prepare(&select_sql)?;
        let result = select_stmt.query_row(
            &[(":id", &id as &dyn rusqlite::types::ToSql), (":namespace", &namespace)],
            |row| {
                let raw: Option<String> = row.get(0)?;
                let version: i64 = row.get(1)?;
                Ok((raw, version))
            },
        );

        match result {
            Ok((raw, version)) => Ok(VersionedUpdateResult::VersionMismatch {
                version,
                value: safe_json_parse(raw),
            }),
            Err(_) => Ok(VersionedUpdateResult::Error),
        }
    })();

    result.unwrap_or(VersionedUpdateResult::Error)
}

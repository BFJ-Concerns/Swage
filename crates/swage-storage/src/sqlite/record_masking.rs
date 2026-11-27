//! Registered secret values are masked out of every table the store keeps.
//!
//! The conversation writers mask their records before they are written, so
//! what they hand back to their callers is masked too. Every other writer is
//! covered here, on the connection itself: for each text column of each
//! table, a temporary trigger rewrites a row that was inserted or updated
//! holding a registered value. The triggers live in the connection's own
//! `temp` schema, so the store's schema is what it was, and a writer added
//! later is covered without being touched.

use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Value, ValueRef};
use swage_core::RegisteredSecretValues;

/// The tables that hold registered values by design.
const SECRET_TABLES: &[&str] = &["secrets", "managed_secret_values", "_swage_migrations"];

/// Install the masking functions and triggers on `conn`.
pub(super) fn install(conn: &Connection, values: &RegisteredSecretValues) -> rusqlite::Result<()> {
    let mask = values.clone();
    conn.create_scalar_function("swage_mask", 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
        if let ValueRef::Text(bytes) = ctx.get_raw(0) {
            let mut text = String::from_utf8_lossy(bytes).into_owned();
            mask.mask(&mut text);
            return Ok(Value::Text(text));
        }
        ctx.get::<Value>(0)
    })?;
    let holds = values.clone();
    conn.create_scalar_function(
        "swage_holds_secret",
        1,
        FunctionFlags::SQLITE_UTF8,
        move |ctx| {
            Ok(match ctx.get_raw(0) {
                ValueRef::Text(bytes) => holds.holds_any(&String::from_utf8_lossy(bytes)),
                _ => false,
            })
        },
    )?;

    for (table, column) in text_columns(conn)?
        .into_iter()
        .filter(|(_, column)| !swage_core::secret_masking::is_key_field(column))
    {
        // The body names its table unqualified: some SQLite releases (3.46.1
        // among them) refuse a qualified name there, and with no temp table of
        // that name it resolves to the main table the trigger is on.
        for (event, name) in [("INSERT", "insert"), ("UPDATE OF \"{column}\"", "update")] {
            let event = event.replace("{column}", &column);
            conn.execute_batch(&format!(
                "CREATE TEMP TRIGGER \"swage_mask_{table}_{column}_{name}\"
                 AFTER {event} ON main.\"{table}\"
                 WHEN swage_holds_secret(NEW.\"{column}\")
                 BEGIN
                     UPDATE \"{table}\" SET \"{column}\" = swage_mask(\"{column}\")
                     WHERE rowid = NEW.rowid;
                 END;"
            ))?;
        }
    }
    Ok(())
}

/// Whether `candidate` is held in one of the store's own key columns — an
/// identifier or slug column of a table masking covers — and so is a key
/// the product reads back rather than text anyone wrote.
pub(super) fn is_stored_key(conn: &Connection, candidate: &str) -> rusqlite::Result<bool> {
    for (table, column) in text_columns(conn)?
        .into_iter()
        .filter(|(_, column)| swage_core::secret_masking::is_key_field(column))
    {
        let found = conn
            .prepare_cached(&format!(
                "SELECT 1 FROM main.\"{table}\" WHERE \"{column}\" = ?1 LIMIT 1"
            ))?
            .exists([candidate])?;
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Each ordinary table's text columns, leaving out the secret store itself
/// and full-text indexes, whose shadow tables only the index may write.
fn text_columns(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut statement = conn.prepare(
        "SELECT name, sql FROM main.sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let tables: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let virtual_tables: Vec<&str> = tables
        .iter()
        .filter(|(_, sql)| sql.trim_start().starts_with("CREATE VIRTUAL"))
        .map(|(name, _)| name.as_str())
        .collect();

    let mut found = Vec::new();
    for (table, _) in &tables {
        let is_index = virtual_tables
            .iter()
            .any(|index| table == index || table.starts_with(&format!("{index}_")));
        if is_index || SECRET_TABLES.contains(&table.as_str()) {
            continue;
        }
        let mut columns = conn.prepare(&format!(
            "SELECT name FROM pragma_table_info('{table}')
             WHERE upper(type) LIKE '%TEXT%'"
        ))?;
        for column in columns.query_map([], |row| row.get::<_, String>(0))? {
            found.push((table.clone(), column?));
        }
    }
    Ok(found)
}

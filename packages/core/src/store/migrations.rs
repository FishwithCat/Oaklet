use rusqlite::{Connection, params};

use super::{has_table, now_ms};

const NEWER_DATABASE: &str =
    "This Oaklet data was created by a newer version. Update Oaklet to open it.";

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

pub(super) fn migrate(conn: &Connection) -> Result<(), String> {
    // The runner never drops tables. A pre-V0 database has data but no
    // migration history; reset it manually from the shell instead of letting
    // V0 fail halfway through on an existing schema.
    if !has_table(conn, "schema_migrations")? && has_table(conn, "sessions")? {
        return Err(
            "This Oaklet database predates schema migrations. Move it aside and restart Oaklet."
                .into(),
        );
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
           version    INTEGER PRIMARY KEY,
           name       TEXT NOT NULL,
           checksum   TEXT NOT NULL,
           applied_at INTEGER NOT NULL
         );",
    )
    .map_err(|error| format!("Could not create the migration history: {error}"))?;

    let applied = read_applied(conn)?;
    validate(&applied)?;

    // -1 so V0 is pending on a fresh database.
    let current = applied.last().map_or(-1, |row| row.0);
    let pending: Vec<_> = MIGRATIONS
        .iter()
        .filter(|migration| migration.version > current)
        .collect();

    for migration in pending {
        let tx = conn.unchecked_transaction().map_err(|error| {
            format!(
                "Could not prepare migration V{}: {error}",
                migration.version
            )
        })?;
        tx.execute_batch(migration.sql).map_err(|error| {
            format!("Could not apply migration V{}: {error}", migration.version)
        })?;
        record(&tx, migration, now_ms())?;
        tx.commit().map_err(|error| {
            format!("Could not commit migration V{}: {error}", migration.version)
        })?;
    }

    conn.pragma_update(None, "user_version", latest_version())
        .map_err(|error| format!("Could not record the schema version: {error}"))?;
    Ok(())
}

fn read_applied(conn: &Connection) -> Result<Vec<(i64, String, String)>, String> {
    let mut statement = conn
        .prepare("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
        .map_err(|error| format!("Could not read the migration history: {error}"))?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(|error| format!("Could not read the migration history: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read the migration history: {error}"))
}

fn record(conn: &Connection, migration: &Migration, now: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![
            migration.version,
            migration.name,
            checksum_of(migration.sql),
            now
        ],
    )
    .map(|_| ())
    .map_err(|error| format!("Could not record migration V{}: {error}", migration.version))
}

fn validate(applied: &[(i64, String, String)]) -> Result<(), String> {
    for (version, name, checksum) in applied {
        let Some(migration) = MIGRATIONS
            .iter()
            .find(|migration| migration.version == *version)
        else {
            return if *version > latest_version() {
                Err(NEWER_DATABASE.into())
            } else {
                Err(format!(
                    "Migration V{version} is missing from this build. Restore it or update Oaklet."
                ))
            };
        };
        if migration.name != name.as_str() || checksum_of(migration.sql) != checksum.as_str() {
            return Err(format!(
                "Migration V{version} was modified after it was applied. Add a new migration instead."
            ));
        }
    }

    let current = applied.last().map_or(-1, |row| row.0);
    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= current)
    {
        if !applied.iter().any(|row| row.0 == migration.version) {
            return Err(format!(
                "Migration V{} is missing from the database. Restore it or add a repair step.",
                migration.version
            ));
        }
    }
    Ok(())
}

fn checksum_of(sql: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in sql.replace("\r\n", "\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn latest_version() -> i64 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

#[cfg(test)]
mod tests {
    use super::{MIGRATIONS, latest_version};
    use crate::store::Store;

    #[test]
    fn applies_v0_to_a_fresh_database() {
        let store = Store::in_memory().unwrap();
        let conn = store.connection().unwrap();
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(applied, MIGRATIONS.len() as i64);
        assert!(crate::store::has_table(&conn, "session_events").unwrap());
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, latest_version());
    }

    #[test]
    fn rejects_a_modified_migration() {
        let root = crate::tools::test_support::temp_root("store-migrate-checksum");
        let path = root.join("oaklet.db");
        let store = Store::open(&path).unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "UPDATE schema_migrations SET checksum = '0' WHERE version = 0",
                [],
            )
            .unwrap();
        drop(store);

        let error = Store::open(&path).err().expect("must fail");
        assert!(error.contains("modified"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_a_newer_database() {
        let root = crate::tools::test_support::temp_root("store-migrate-newer");
        let path = root.join("oaklet.db");
        let store = Store::open(&path).unwrap();
        store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO schema_migrations (version, name, checksum, applied_at) \
                 VALUES (99, 'future', '0', 0)",
                [],
            )
            .unwrap();
        drop(store);

        let error = Store::open(&path).err().expect("must fail");
        assert!(error.contains("newer"), "{error}");
        std::fs::remove_dir_all(&root).ok();
    }
}

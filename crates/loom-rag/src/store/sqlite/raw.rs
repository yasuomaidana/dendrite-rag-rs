//! The only module that writes SQL by hand (`diesel::sql_query`) for SQLite: PRAGMAs, which
//! the Diesel DSL cannot express. Everything else goes through the DSL, and clippy rejects
//! `sql_query` outside this module (`clippy.toml`).
#![allow(clippy::disallowed_methods)]

use diesel::prelude::*;
use diesel::sql_types::Integer;
use diesel::sqlite::SqliteConnection;

#[derive(QueryableByName)]
struct Version {
    #[diesel(sql_type = Integer)]
    user_version: i32,
}

/// `PRAGMA user_version`, the DDL version recorded in the file.
pub(super) fn user_version(conn: &mut SqliteConnection) -> QueryResult<i32> {
    Ok(diesel::sql_query("PRAGMA user_version")
        .get_result::<Version>(conn)?
        .user_version)
}

/// Records the DDL version in the file.
pub(super) fn set_user_version(conn: &mut SqliteConnection, version: i32) -> QueryResult<()> {
    diesel::sql_query(format!("PRAGMA user_version = {version}")).execute(conn)?;
    Ok(())
}

/// Switches the file to WAL mode, which readers in other processes rely on.
pub(super) fn wal(conn: &mut SqliteConnection) -> QueryResult<()> {
    // `journal_mode` answers with a row, so it must be read, not executed.
    #[derive(QueryableByName)]
    struct Mode {
        #[diesel(sql_type = diesel::sql_types::Text)]
        #[allow(dead_code, reason = "the row has to be read; its value is not needed")]
        journal_mode: String,
    }
    diesel::sql_query("PRAGMA journal_mode = WAL").get_result::<Mode>(conn)?;
    Ok(())
}

/// Settings of every connection.
pub(super) fn settings(conn: &mut SqliteConnection) -> QueryResult<()> {
    for pragma in [
        "PRAGMA synchronous = NORMAL",
        "PRAGMA foreign_keys = OFF",
        "PRAGMA busy_timeout = 5000",
    ] {
        diesel::sql_query(pragma).execute(conn)?;
    }
    Ok(())
}

/// Makes a read connection refuse writes.
pub(super) fn query_only(conn: &mut SqliteConnection) -> QueryResult<()> {
    diesel::sql_query("PRAGMA query_only = ON").execute(conn)?;
    Ok(())
}

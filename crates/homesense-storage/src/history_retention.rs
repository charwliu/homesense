//! Bounded, conservative command-history archival. SQL/embedded references are
//! roots, never cascade-delete permissions. Runtime timestamps are monotonic;
//! an unchanged row must instead be observed for at least 30 wall-clock days.
//! No VACUUM: light_setting and history cursors depend on stable SQLite rowids.
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, TransactionBehavior, params, types::ValueRef,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

pub const RETENTION_MS: i64 = 30 * 86_400_000;
const BATCH: i64 = 128;
const MAX_GAP_MS: i64 = 2 * 86_400_000;
const MIN_CLOCK_MS: i64 = 1_577_836_800_000; // 2020-01-01
const ARCHIVE_ID: i64 = 0x48534152; // HSAR; deliberately not a runnable state DB

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub scanned: usize,
    pub archived: usize,
    pub deleted: usize,
    pub clock_reset: bool,
}
#[derive(Debug, Clone)]
struct Row {
    rowid: i64,
    boot: String,
    id: i64,
    revision: i64,
    device: String,
    intent: Vec<u8>,
    status: String,
    run: Option<i64>,
    payload: String,
    fingerprint: String,
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn limit(db: &Connection) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(50);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(error)
}
fn table_exists(db: &Connection, name: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .map_err(error)
}
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
// Tagged values retain INTEGER/BLOB/NULL identity without SQL text interpolation.
fn payload(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<String> {
    let mut fields = Vec::new();
    for i in start..row.as_ref().column_count() {
        fields.push(match row.get_ref(i)? {
            ValueRef::Null => json!(["null"]),
            ValueRef::Integer(v) => json!(["integer", v]),
            ValueRef::Real(v) => json!(["real", v]),
            ValueRef::Text(v) => json!([
                "text",
                std::str::from_utf8(v).map_err(|_| rusqlite::Error::InvalidQuery)?
            ]),
            ValueRef::Blob(v) => json!(["blob", v]),
        });
    }
    serde_json::to_string(&fields).map_err(|_| rusqlite::Error::InvalidQuery)
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let payload = payload(r, 1)?;
    let fingerprint = format!("{:x}", Sha256::digest(payload.as_bytes()));
    Ok(Row {
        rowid: r.get(0)?,
        boot: r.get(1)?,
        id: r.get(2)?,
        revision: r.get(3)?,
        device: r.get(4)?,
        intent: r.get(6)?,
        status: r.get(12)?,
        run: r.get(11)?,
        payload,
        fingerprint,
    })
}
fn read_row(db: &Connection, rowid: i64) -> Result<Option<Row>, String> {
    db.query_row(
        "SELECT rowid,* FROM command_journal WHERE rowid=?1",
        [rowid],
        row,
    )
    .optional()
    .map_err(error)
}
/// Discover every declared reference, including new tables, rather than freezing
/// a list that could silently miss a later schema addition. Fail closed on an
/// unsupported parent key. The supported product schema is checked on entry.
fn foreign_references(db: &Connection) -> Result<Vec<String>, String> {
    let tables = db
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .map_err(error)?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut result = Vec::new();
    for table in tables {
        let mut groups: BTreeMap<i64, Vec<(String, String)>> = BTreeMap::new();
        let mut q = db.prepare("SELECT id,\"from\",\"to\" FROM pragma_foreign_key_list(?1) WHERE \"table\"='command_journal'").map_err(error)?;
        for r in q
            .query_map([&table], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(error)?
        {
            let (id, from, to) = r.map_err(error)?;
            groups.entry(id).or_default().push((from, to));
        }
        for fields in groups.values() {
            let mut clauses = Vec::new();
            for (from, to) in fields {
                let parameter = match to.as_str() {
                    "boot_id" => "?1",
                    "command_id" => "?2",
                    _ => return Err("unsupported history reference".into()),
                };
                clauses.push(format!("{}={parameter}", quote(from)));
            }
            result.push(format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE {})",
                quote(&table),
                clauses.join(" AND ")
            ));
        }
    }
    Ok(result)
}
fn referenced(db: &Connection, r: &Row, fks: &[String], newest_boot: &str) -> Result<bool, String> {
    // Unfinished/uncertain work and current-boot actuator fences remain intact,
    // even if no FK refers to them. Query/LED history is eligible when superseded.
    if !matches!(
        r.status.as_str(),
        "written" | "observed" | "cancelled" | "failed"
    ) || r.run.is_some()
        || (r.boot == newest_boot
            && !matches!(
                r.intent.as_slice(),
                [1, 0, 0, 0] | [1, 6, 0 | 1, 0] | [1, 10, 0, 0]
            ))
    {
        return Ok(true);
    }
    let exists = |sql: &str| {
        db.query_row(sql, params![r.boot, r.id], |v| v.get::<_, bool>(0))
            .map_err(error)
    };
    for fk in fks {
        if exists(fk)? {
            return Ok(true);
        }
    }
    // MAX(command_id) is a durable high-water mark; keep it in every boot.
    if !exists("SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND command_id>?2)")? {
        return Ok(true);
    }
    // Preserve the last receipt of each exact device/intent/scope, including LED
    // requests used for deduplication. Never renumber retained rowids.
    let newer: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND device_id=?2 AND revision=?3 AND intent=?4 AND transport_epoch=(SELECT transport_epoch FROM command_journal WHERE rowid=?5) AND control_epoch=(SELECT control_epoch FROM command_journal WHERE rowid=?5) AND gateway_id IS (SELECT gateway_id FROM command_journal WHERE rowid=?5) AND gateway_epoch IS (SELECT gateway_epoch FROM command_journal WHERE rowid=?5) AND command_id>?6)", params![r.boot,r.device,r.revision,r.intent,r.rowid,r.id], |v|v.get(0)).map_err(error)?;
    if !newer {
        return Ok(true);
    }
    for sql in [
        "SELECT EXISTS(SELECT 1 FROM light_setting WHERE journal_row=?3)",
        "SELECT EXISTS(SELECT 1 FROM publication_cancellation WHERE boot_id=?1 AND command_id=?2)",
        "SELECT EXISTS(SELECT 1 FROM command_journal WHERE boot_id=?1 AND run_id=?2)",
        "SELECT EXISTS(SELECT 1 FROM native_scene_request WHERE boot_id=?1 AND (manual_fence=?2 OR instr(','||commands||',',','||?2||',')>0 OR instr(','||initial_commands||',',','||?2||',')>0))",
    ] {
        // Numbered bindings need exactly their highest parameter index.
        let yes: bool = if sql.contains("?3") {
            db.query_row(sql, params![r.boot, r.id, r.rowid], |v| v.get(0))
        } else {
            db.query_row(sql, params![r.boot, r.id], |v| v.get(0))
        }
        .map_err(error)?;
        if yes {
            return Ok(true);
        }
    }
    for (table, condition) in [
        (
            "component_mode_audit",
            "instr(','||tickets||',',','||?2||',')>0",
        ),
        (
            "keypad_completion_audit",
            "command_fence=?2 OR instr(','||tickets||',',','||?2||',')>0 OR instr(','||led_commands||',',','||?2||',')>0",
        ),
    ] {
        if table_exists(db, table)?
            && exists(&format!(
                "SELECT EXISTS(SELECT 1 FROM {table} WHERE boot=?1 AND ({condition}))"
            ))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn private_directory(path: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(error)?;
    if !std::fs::symlink_metadata(path).map_err(error)?.is_dir() {
        return Err("archive directory must be real".into());
    }
    Ok(())
}
fn archive(db: &Connection, path: &Path, now: i64, rows: &[Row]) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            // A previous attempt may have stopped at fsync, not at DELETE.
            // Reuse requires durability again, not just readable bytes.
            std::fs::File::open(path)
                .and_then(|file| file.sync_all())
                .map_err(error)?;
            std::fs::File::open(path.parent().ok_or("archive parent missing")?)
                .and_then(|directory| directory.sync_all())
                .map_err(error)?;
            return verify_archive(path, rows);
        }
        Ok(_) => return Err("archive must be a regular file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(error(e)),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(error)?;
    // A failed/partial artifact is retained for diagnosis, never authorizes delete.
    let mut a = Connection::open(path).map_err(error)?;
    a.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE history_rows(table_name TEXT NOT NULL,source_rowid INTEGER NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(table_name,source_rowid));").map_err(error)?;
    a.pragma_update(None, "application_id", ARCHIVE_ID)
        .map_err(error)?;
    a.pragma_update(None, "user_version", 1).map_err(error)?;
    let tx = a.transaction().map_err(error)?;
    for (key, value) in [
        ("created_unix_ms", now.to_string()),
        ("retention_ms", RETENTION_MS.to_string()),
        ("source_schema", crate::SCHEMA_VERSION.to_string()),
        (
            "format",
            "tagged-sql-values-v1; audit-only, no replay".into(),
        ),
    ] {
        tx.execute("INSERT INTO metadata VALUES(?1,?2)", params![key, value])
            .map_err(error)?;
    }
    let columns: Vec<String> = db
        .prepare("SELECT name FROM pragma_table_info('command_journal') ORDER BY cid")
        .map_err(error)?
        .query_map([], |r| r.get(0))
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    tx.execute(
        "INSERT INTO metadata VALUES('command_journal_columns',?1)",
        [serde_json::to_string(&columns).map_err(error)?],
    )
    .map_err(error)?;
    for r in rows {
        tx.execute(
            "INSERT INTO history_rows VALUES('command_journal',?1,?2)",
            params![r.rowid, r.payload],
        )
        .map_err(error)?;
    }
    // Include immutable configuration context for independently readable audit.
    let revisions: std::collections::BTreeSet<_> = rows.iter().map(|r| r.revision).collect();
    for revision in revisions {
        for table in ["configuration_revision", "automation_revision"] {
            let mut q = db
                .prepare(&format!("SELECT rowid,* FROM {table} WHERE revision=?1"))
                .map_err(error)?;
            for record in q
                .query_map([revision], |r| Ok((r.get::<_, i64>(0)?, payload(r, 1)?)))
                .map_err(error)?
            {
                let (id, data) = record.map_err(error)?;
                tx.execute(
                    "INSERT INTO history_rows VALUES(?1,?2,?3)",
                    params![table, id, data],
                )
                .map_err(error)?;
            }
        }
    }
    tx.commit().map_err(error)?;
    a.close().map_err(|(_, e)| error(e))?;
    file.sync_all().map_err(error)?;
    std::fs::File::open(path.parent().ok_or("archive parent missing")?)
        .and_then(|d| d.sync_all())
        .map_err(error)?;
    verify_archive(path, rows)
}
fn verify_archive(path: &Path, rows: &[Row]) -> Result<(), String> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(error)?;
    let app: i64 = db
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(error)?;
    if app != ARCHIVE_ID {
        return Err("foreign history archive".into());
    }
    let check: String = db
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(error)?;
    if check != "ok" {
        return Err("archive integrity failed".into());
    }
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM history_rows WHERE table_name='command_journal'",
            [],
            |r| r.get(0),
        )
        .map_err(error)?;
    if n != rows.len() as i64 {
        return Err("archive count mismatch".into());
    }
    for r in rows {
        let data:String=db.query_row("SELECT payload FROM history_rows WHERE table_name='command_journal' AND source_rowid=?1",[r.rowid],|v|v.get(0)).map_err(error)?;
        if data != r.payload {
            return Err("archive content mismatch".into());
        }
    }
    Ok(())
}
/// Run on a dedicated blocking worker, at most once per minute. A pass visits at
/// most 128 rows and uses fail-fast locks plus a 50ms SQLite progress deadline.
/// Existing rows start their retention clock on first observation, not migration
/// guesses. A >48h clock gap resets ages conservatively (including long downtime).
/// Archives are private, compact, fsynced and independently reopened/verified
/// before any source deletion. A crash may leave an extra archive, never loss.
pub fn maintain(source: &Path, directory: &Path, now_unix_ms: i64) -> Result<Report, String> {
    maintain_inner(source, directory, now_unix_ms, || {})
}
fn maintain_inner(
    source: &Path,
    directory: &Path,
    now: i64,
    before_delete: impl FnOnce(),
) -> Result<Report, String> {
    if now < MIN_CLOCK_MS {
        return Err("history retention clock invalid".into());
    }
    if !std::fs::symlink_metadata(source).map_err(error)?.is_file() {
        return Err("history source must be a regular file".into());
    }
    let mut db =
        Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(error)?;
    db.busy_timeout(Duration::ZERO).map_err(error)?;
    db.pragma_update(None, "foreign_keys", "ON")
        .map_err(error)?;
    db.pragma_update(None, "synchronous", "FULL")
        .map_err(error)?;
    let app: i64 = db
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(error)?;
    let schema: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(error)?;
    if app != crate::APPLICATION_ID || schema != crate::SCHEMA_VERSION {
        return Err("unsupported history source".into());
    }
    limit(&db)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    // Compatible auxiliary metadata; no changes to product row formats, foreign
    // keys or immutable audit triggers and no migration/replay of product state.
    tx.execute_batch("CREATE TABLE IF NOT EXISTS history_retention_clock(singleton INTEGER PRIMARY KEY CHECK(singleton=1),last_unix_ms INTEGER NOT NULL,scan_after INTEGER NOT NULL,archive_id TEXT NOT NULL,age_floor_ms INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS history_retention_command(boot_id TEXT NOT NULL,command_id INTEGER NOT NULL,source_rowid INTEGER NOT NULL,snapshot TEXT NOT NULL,first_seen_unix_ms INTEGER NOT NULL,PRIMARY KEY(boot_id,command_id));").map_err(error)?;
    let old: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT last_unix_ms,scan_after,age_floor_ms FROM history_retention_clock WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let mut report = Report::default();
    if let Some((last, _, _)) = old {
        if now < last {
            return Err("history retention clock moved backwards".into());
        }
        if now - last > MAX_GAP_MS {
            report.clock_reset = true;
        }
    }
    // Reset age in O(1), not by rewriting every historical marker under a lock.
    let floor = if report.clock_reset {
        now
    } else {
        old.map_or(now, |(_, _, floor)| floor)
    };
    let cursor = old.map_or(0, |(_, c, _)| c);
    let rows = tx
        .prepare("SELECT rowid,* FROM command_journal WHERE rowid>?1 ORDER BY rowid LIMIT ?2")
        .map_err(error)?
        .query_map(params![cursor, BATCH], row)
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let mut aged = Vec::new();
    for r in &rows {
        tx.execute("INSERT INTO history_retention_command VALUES(?1,?2,?3,?4,?5) ON CONFLICT(boot_id,command_id) DO UPDATE SET first_seen_unix_ms=CASE WHEN snapshot=excluded.snapshot AND source_rowid=excluded.source_rowid THEN first_seen_unix_ms ELSE excluded.first_seen_unix_ms END,source_rowid=excluded.source_rowid,snapshot=excluded.snapshot",params![r.boot,r.id,r.rowid,r.fingerprint,now]).map_err(error)?;
        let first:i64=tx.query_row("SELECT first_seen_unix_ms FROM history_retention_command WHERE boot_id=?1 AND command_id=?2",params![r.boot,r.id],|v|v.get(0)).map_err(error)?;
        if now.saturating_sub(first.max(floor)) >= RETENTION_MS {
            aged.push(r.clone());
        }
    }
    let next = if rows.len() < BATCH as usize {
        0
    } else {
        rows.last().map_or(0, |r| r.rowid)
    };
    tx.execute("INSERT INTO history_retention_clock VALUES(1,?1,?2,lower(hex(randomblob(16))),?3) ON CONFLICT(singleton) DO UPDATE SET last_unix_ms=excluded.last_unix_ms,scan_after=excluded.scan_after,age_floor_ms=excluded.age_floor_ms",params![now,next,floor]).map_err(error)?;
    report.scanned = rows.len();
    tx.commit().map_err(error)?;
    if aged.is_empty() {
        return Ok(report);
    }
    limit(&db)?;
    let read = db.unchecked_transaction().map_err(error)?;
    let fks = foreign_references(&read)?;
    let newest: String = read
        .query_row(
            "SELECT boot_id FROM command_journal ORDER BY rowid DESC LIMIT 1",
            [],
            |v| v.get(0),
        )
        .optional()
        .map_err(error)?
        .unwrap_or_default();
    let mut candidates = Vec::new();
    for r in aged {
        if !referenced(&read, &r, &fks, &newest)? {
            candidates.push(r);
        }
    }
    read.commit().map_err(error)?;
    db.progress_handler(0, None::<fn() -> bool>)
        .map_err(error)?;
    if candidates.is_empty() {
        return Ok(report);
    }
    private_directory(directory.parent().ok_or("archive parent missing")?)?;
    private_directory(directory)?;
    let source_id: String = db
        .query_row(
            "SELECT archive_id FROM history_retention_clock WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(error)?;
    if source_id.len() != 32 || !source_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid history archive identity".into());
    }
    let mut digest = Sha256::new();
    for r in &candidates {
        digest.update(r.rowid.to_be_bytes());
        digest.update(r.fingerprint.as_bytes());
    }
    let path = directory.join(format!(
        "history-{source_id}-{:x}.sqlite",
        digest.finalize()
    ));
    // An interrupted delete retries against the same verified artifact rather
    // than filling disk with duplicate archives. Foreign/partial files fail closed.
    archive(&db, &path, now, &candidates)?;
    report.archived = candidates.len();
    before_delete();
    limit(&db)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    let fks = foreign_references(&tx)?;
    let newest: String = tx
        .query_row(
            "SELECT boot_id FROM command_journal ORDER BY rowid DESC LIMIT 1",
            [],
            |v| v.get(0),
        )
        .optional()
        .map_err(error)?
        .unwrap_or_default();
    for r in candidates {
        // References may have appeared, or the row changed during disk I/O.
        // Recheck in the same source write transaction as DELETE (FKs stay ON).
        if let Some(current) = read_row(&tx, r.rowid)? {
            let aged:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM history_retention_command WHERE boot_id=?1 AND command_id=?2 AND snapshot=?3 AND first_seen_unix_ms<=?4 AND (SELECT age_floor_ms FROM history_retention_clock WHERE singleton=1)<=?4)",params![r.boot,r.id,r.fingerprint,now-RETENTION_MS],|v|v.get(0)).map_err(error)?;
            if current.payload == r.payload && aged && !referenced(&tx, &current, &fks, &newest)? {
                report.deleted += tx
                    .execute("DELETE FROM command_journal WHERE rowid=?1", [r.rowid])
                    .map_err(error)?;
                tx.execute(
                    "DELETE FROM history_retention_command WHERE boot_id=?1 AND command_id=?2",
                    params![r.boot, r.id],
                )
                .map_err(error)?;
            }
        }
    }
    tx.commit().map_err(error)?;
    Ok(report)
}

#[cfg(test)]
mod tests;

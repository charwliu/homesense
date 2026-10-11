//! A configuration-wide calendar high-water mark. Commit before dispatching any
//! occurrence; a crash in between may skip work, but must never replay it.
use crate::{Error, Store, sql_revision};
use rusqlite::{OptionalExtension, params};
#[derive(Debug, Clone, Copy)]
pub struct Checkpoint {
    pub expected: Option<i64>,
    pub next: i64,
}
impl Store {
    pub fn calendar_watermark(
        &mut self,
        revision: u64,
        update: Option<Checkpoint>,
    ) -> Result<Option<i64>, Error> {
        let revision = sql_revision(revision)?;
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let active: Option<i64> = tx
            .query_row(
                "SELECT revision FROM active_configuration WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if active != Some(revision) {
            return Err(Error::Conflict);
        }
        let current: Option<i64> = tx
            .query_row(
                "SELECT unix_ms FROM calendar_watermark WHERE revision=?1",
                [revision],
                |r| r.get(0),
            )
            .optional()?;
        let result = if let Some(update) = update {
            if update.next < 946_684_800_000 || update.next > i64::MAX - 115_200_000 {
                return Err(Error::InvalidAudit);
            }
            if current != update.expected || current.is_some_and(|value| update.next < value) {
                return Err(Error::Conflict);
            }
            tx.execute("INSERT INTO calendar_watermark(revision,unix_ms) VALUES(?1,?2) ON CONFLICT(revision) DO UPDATE SET unix_ms=excluded.unix_ms", params![revision,update.next])?;
            Some(update.next)
        } else {
            current
        };
        tx.commit()?;
        Ok(result)
    }
}

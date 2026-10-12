//! Bounded run summaries in insertion order, never executable replay plans.
use crate::{Error,Store,sql_revision};
use crate::scene_run::Status;
use rusqlite::{params,OptionalExtension};
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct Cursor {pub boot:String,pub run:String}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct Summary {pub identity:Cursor,pub started_ms:u64,pub updated_ms:u64,pub version:u64,pub status:Status}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct Page {pub revision:u64,pub definition:String,pub runs:Vec<Summary>,pub next_before:Option<Cursor>}
impl Store {
    /// Cursor identity is resolved within the requested revision and scene.
    /// New inserts between pages do not shift the older-page boundary.
    pub fn scene_history(&self,revision:u64,definition:&str,before:Option<&Cursor>,limit:usize)->Result<Page,Error> {
        let revision_sql=sql_revision(revision)?;
        if definition.is_empty() || definition.len()>256 || definition.chars().any(char::is_control) || !(1..=100).contains(&limit) {return Err(Error::InvalidAudit);}
        let tx=self.connection.unchecked_transaction()?;
        let boundary=if let Some(cursor)=before {
            for key in [&cursor.boot,&cursor.run] {if key.is_empty() || key.len()>128 || key.chars().any(char::is_control) {return Err(Error::InvalidAudit);}}
            Some(tx.query_row("SELECT rowid FROM scene_run WHERE revision=?1 AND definition=?2 AND boot_id=?3 AND run_id=?4",params![revision_sql,definition,cursor.boot,cursor.run],|r|r.get::<_,i64>(0)).optional()?.ok_or(Error::InvalidAudit)?)
        } else {None};
        let mut query=tx.prepare("SELECT boot_id,run_id,started_ms,updated_ms,version,status FROM scene_run WHERE revision=?1 AND definition=?2 AND (?3 IS NULL OR rowid<?3) ORDER BY rowid DESC LIMIT ?4")?;
        let rows=query.query_map(params![revision_sql,definition,boundary,(limit+1) as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?)))?;
        let mut runs=Vec::new();
        for row in rows {
            let (boot,run,start,updated,version,status)=row?;
            runs.push(Summary{identity:Cursor{boot,run},started_ms:u64::try_from(start).map_err(|_|Error::InvalidAudit)?,updated_ms:u64::try_from(updated).map_err(|_|Error::InvalidAudit)?,version:u64::try_from(version).map_err(|_|Error::InvalidAudit)?,status:Status::parse(&status)?});
        }
        let next_before=if runs.len()>limit {runs.truncate(limit);runs.last().map(|r|r.identity.clone())} else {None};
        drop(query);tx.commit()?;
        Ok(Page{revision,definition:definition.into(),runs,next_before})
    }
}

//! Immutable, private configuration drafts. Saving never changes the active revision.
use homesense_core::ExecutionContext;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use uuid::Uuid;

pub const MAX_BYTES: usize = 256 * 1024;
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct Draft {
    pub id: Uuid,
    pub expected_revision: u64,
    pub revision: u64,
    pub devices: serde_json::Value,
    pub automation: serde_json::Value,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceMetadataEdit {
    pub id:Uuid,
    pub expected_revision:String,
    pub revision:String,
    pub device:String,
    pub name:String,
    pub room:Option<String>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceBindingEdit {
    pub id:Uuid,
    pub expected_revision:String,
    pub revision:String,
    pub device:String,
    pub gateway:String,
    pub address:String,
    pub channel:u8,
    pub enabled:bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomEdit {
    pub id:Uuid,
    pub expected_revision:String,
    pub revision:String,
    pub room:homesense_config::Room,
    #[serde(default)]
    pub remove:bool,
}
type StoredDraft = (String, i64, i64, Vec<u8>, Vec<u8>);
#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    pub id: Uuid,
    pub expected_revision: u64,
    pub revision: u64,
}
pub struct Drafts(Connection);
impl Drafts {
    pub fn open(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path).map_err(|e| e.to_string())?;
        }
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| e.to_string())?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS configuration_draft (
          id TEXT PRIMARY KEY, principal TEXT NOT NULL, expected_revision INTEGER NOT NULL,
          revision INTEGER NOT NULL, devices BLOB NOT NULL, automation BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS archived_draft(id TEXT PRIMARY KEY);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self(db))
    }
    pub fn save(
        &mut self,
        principal: Uuid,
        draft: &Draft,
        context: ExecutionContext,
    ) -> Result<(), String> {
        let devices = serde_json::to_vec(&draft.devices).map_err(|e| e.to_string())?;
        let automation = serde_json::to_vec(&draft.automation).map_err(|e| e.to_string())?;
        if devices.len() + automation.len() > MAX_BYTES
            || draft.expected_revision == 0
            || draft.revision <= draft.expected_revision
            || draft.revision > i64::MAX as u64
        {
            return Err("invalid draft size or revision".into());
        }
        let config =
            homesense_config::Configuration::parse(&devices, context).map_err(|e| e.to_string())?;
        if config.revision() != draft.revision {
            return Err("draft revision mismatch".into());
        }
        crate::configuration_publish::validate_automation(&config, &automation, context)
            .map_err(|e| e.to_string())?;
        let tx = self
            .0
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let archived:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM archived_draft WHERE id=?1)",[draft.id.to_string()],|r|r.get(0)).map_err(|e|e.to_string())?;
        if archived{return Err("draft is archived; use a new id".into());}
        let existing:Option<StoredDraft>=tx.query_row("SELECT principal,expected_revision,revision,devices,automation FROM configuration_draft WHERE id=?1",[draft.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|e|e.to_string())?;
        if let Some(old) = existing {
            if old
                != (
                    principal.to_string(),
                    draft.expected_revision as i64,
                    draft.revision as i64,
                    devices,
                    automation,
                )
            {
                return Err("draft id conflict".into());
            }
            return Ok(());
        }
        let count: i64 = tx
            .query_row("SELECT count(*) FROM configuration_draft WHERE id NOT IN (SELECT id FROM archived_draft)", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if count >= 128 {
            return Err("draft capacity exceeded".into());
        }
        tx.execute(
            "INSERT INTO configuration_draft VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                draft.id.to_string(),
                principal.to_string(),
                draft.expected_revision as i64,
                draft.revision as i64,
                devices,
                automation
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }
    /// Bounded metadata only, scoped to the same principal as get/save.
    pub fn list(&self, principal:Uuid)->Result<Vec<Summary>,String> {
        let mut query=self.0.prepare("SELECT id,expected_revision,revision FROM configuration_draft WHERE principal=?1 AND id NOT IN (SELECT id FROM archived_draft) ORDER BY revision DESC,id ASC LIMIT 129").map_err(|e|e.to_string())?;
        let rows=query.query_map([principal.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).map_err(|e|e.to_string())?;
        let mut result=vec![];
        for row in rows {
            let (id,expected,revision)=row.map_err(|e|e.to_string())?;
            if expected<=0 || revision<=expected || result.len()==128 {return Err("invalid draft catalog".into());}
            result.push(Summary{id:Uuid::parse_str(&id).map_err(|e|e.to_string())?,expected_revision:expected as u64,revision:revision as u64});
        }
        Ok(result)
    }
    /// Derive a new immutable draft; transport bindings and automation remain intact.
    pub fn edit_device(&mut self,principal:Uuid,source:Uuid,edit:DeviceMetadataEdit,context:ExecutionContext)->Result<Uuid,String> {
        if edit.id==source || edit.name.trim().is_empty() || edit.name.len()>256 || edit.name.chars().any(char::is_control) {return Err("invalid device metadata".into());}
        let mut draft=self.get(principal,source)?.ok_or("draft not found")?;
        let integer=|value:&str|->Result<u64,String>{
            if value.is_empty()||value.starts_with('0')||!value.bytes().all(|v|v.is_ascii_digit()){return Err("invalid revision".into());}
            value.parse::<u64>().map_err(|_|"invalid revision".into())
        };
        draft.id=edit.id;draft.expected_revision=integer(&edit.expected_revision)?;draft.revision=integer(&edit.revision)?;
        let devices=draft.devices.get_mut("devices").and_then(|v|v.as_array_mut()).ok_or("devices missing")?;
        let device=devices.iter_mut().find(|v|v["id"]==edit.device).and_then(|v|v.as_object_mut()).ok_or("device not found")?;
        device.insert("name".into(),edit.name.into());
        if let Some(room)=edit.room {device.insert("room".into(),room.into());} else {device.remove("room");}
        draft.devices["revision"]=draft.revision.into();draft.automation["configuration_revision"]=draft.revision.into();
        self.save(principal,&draft,context)?;Ok(draft.id)
    }
    /// Derive a validated draft; changed physical identity invalidates motor evidence.
    pub fn edit_binding(&mut self,principal:Uuid,source:Uuid,edit:DeviceBindingEdit,context:ExecutionContext)->Result<Uuid,String> {
        if edit.id==source{return Err("new draft id required".into());}
        let mut draft=self.get(principal,source)?.ok_or("draft not found")?;
        draft.id=edit.id;draft.expected_revision=edit.expected_revision.parse().map_err(|_|"invalid revision")?;draft.revision=edit.revision.parse().map_err(|_|"invalid revision")?;
        if edit.expected_revision!=draft.expected_revision.to_string() || edit.revision!=draft.revision.to_string(){return Err("invalid revision".into());}
        let devices=draft.devices.get_mut("devices").and_then(|v|v.as_array_mut()).ok_or("devices missing")?;
        let device=devices.iter_mut().find(|v|v["id"]==edit.device).and_then(|v|v.as_object_mut()).ok_or("device not found")?;
        let changed=device.get("gateway").and_then(|v|v.as_str())!=Some(edit.gateway.as_str())
            || device.get("address").and_then(|v|v.as_str())!=Some(edit.address.as_str())
            || device.get("channel").and_then(|v|v.as_u64())!=Some(u64::from(edit.channel));
        if changed {device.remove("motor_calibration");}
        device.insert("gateway".into(),edit.gateway.into());device.insert("address".into(),edit.address.into());
        device.insert("channel".into(),edit.channel.into());device.insert("enabled".into(),edit.enabled.into());
        draft.devices["revision"]=draft.revision.into();draft.automation["configuration_revision"]=draft.revision.into();
        self.save(principal,&draft,context)?;Ok(draft.id)
    }
    /// Upsert room metadata while keeping device membership and bindings intact.
    pub fn edit_room(&mut self,principal:Uuid,source:Uuid,edit:RoomEdit,context:ExecutionContext)->Result<Uuid,String> {
        if edit.id==source{return Err("new draft id required".into());}
        let mut draft=self.get(principal,source)?.ok_or("draft not found")?;
        draft.id=edit.id;draft.expected_revision=edit.expected_revision.parse().map_err(|_|"invalid revision")?;draft.revision=edit.revision.parse().map_err(|_|"invalid revision")?;
        if edit.expected_revision!=draft.expected_revision.to_string() || edit.revision!=draft.revision.to_string(){return Err("invalid revision".into());}
        if edit.remove && draft.devices.get("devices").and_then(|v|v.as_array()).is_some_and(|devices|devices.iter().any(|d|d.get("room").and_then(|v|v.as_str())==Some(edit.room.id.as_str()))) {return Err("room_in_use".into());}
        let rooms=draft.devices.as_object_mut().ok_or("invalid device document")?.entry("rooms").or_insert_with(||serde_json::json!([])).as_array_mut().ok_or("invalid rooms")?;
        if edit.remove {
            let index=rooms.iter().position(|r|r["id"]==edit.room.id).ok_or("room not found")?;
            rooms.remove(index);
        } else {
            let value=serde_json::to_value(&edit.room).map_err(|e|e.to_string())?;
            if let Some(old)=rooms.iter_mut().find(|r|r["id"]==edit.room.id){*old=value;}else{rooms.push(value);}
        }
        draft.devices["revision"]=draft.revision.into();draft.automation["configuration_revision"]=draft.revision.into();
        self.save(principal,&draft,context)?;Ok(draft.id)
    }
    /// Hide a draft without deleting its immutable source or recycling its ID.
    /// Already admitted publication requests are independent and are not cancelled.
    pub fn archive(&self,principal:Uuid,id:Uuid)->Result<bool,String> {
        let owned:bool=self.0.query_row("SELECT EXISTS(SELECT 1 FROM configuration_draft WHERE principal=?1 AND id=?2)",params![principal.to_string(),id.to_string()],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !owned{return Ok(false);}
        self.0.execute("INSERT OR IGNORE INTO archived_draft(id) VALUES (?1)",[id.to_string()]).map_err(|e|e.to_string())?;
        Ok(true)
    }
    pub fn get(&self, principal: Uuid, id: Uuid) -> Result<Option<Draft>, String> {
        let row:Option<(i64,i64,Vec<u8>,Vec<u8>)>=self.0.query_row("SELECT expected_revision,revision,devices,automation FROM configuration_draft WHERE principal=?1 AND id=?2 AND id NOT IN (SELECT id FROM archived_draft)",params![principal.to_string(),id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?;
        row.map(|(expected_revision, revision, devices, automation)| {
            Ok(Draft {
                id,
                expected_revision: u64::try_from(expected_revision)
                    .map_err(|_| "invalid stored revision")?,
                revision: u64::try_from(revision).map_err(|_| "invalid stored revision")?,
                devices: serde_json::from_slice(&devices).map_err(|e| e.to_string())?,
                automation: serde_json::from_slice(&automation).map_err(|e| e.to_string())?,
            })
        })
        .transpose()
    }
}

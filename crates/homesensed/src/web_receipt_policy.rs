//! Explicit bounded policy; expiration never releases an execution fence.
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path};
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub max_records: u32,
    pub expiry: Expiry,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expiry {
    Never,
    MarkExpiredPreserveFence { window_ms: u64 },
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            max_records: 100_000,
            expiry: Expiry::Never,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=100_000).contains(&self.max_records) {
            return Err("receipt capacity must be 1..100000".into());
        }
        if let Expiry::MarkExpiredPreserveFence { window_ms } = self.expiry
            && !(1..=31_536_000_000).contains(&window_ms)
        {
            return Err("receipt window must be 1ms..365days".into());
        }
        Ok(())
    }
    pub fn load(directory: &Path) -> Result<Self, String> {
        let p = directory.join("web-receipt-policy.json");
        let file = match std::fs::File::open(p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.to_string()),
        };
        let mut bytes = vec![];
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 4096 {
            return Err("receipt policy too large".into());
        }
        let policy: Self = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        policy.validate()?;
        Ok(policy)
    }
    pub(crate) fn expiry_ms(&self, now: u64) -> Result<Option<u64>, String> {
        match self.expiry {
            Expiry::Never => Ok(None),
            Expiry::MarkExpiredPreserveFence { window_ms } => now
                .checked_add(window_ms)
                .filter(|n| *n <= i64::MAX as u64)
                .map(Some)
                .ok_or_else(|| "receipt expiry overflow".into()),
        }
    }
}

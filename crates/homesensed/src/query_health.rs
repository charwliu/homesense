//! Consecutive failed query turns. A retry is part of its original turn.
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Failure {
    pub code: &'static str,
    pub consecutive_cycles: u32,
    pub first_ms: u64,
    pub last_ms: u64,
}
#[derive(Default)]
pub(crate) struct Health(BTreeMap<(String,u8,u8),Failure>);
impl Health {
    pub fn missing(&mut self,bus:&str,address:u8,channel:u8,at:u64) {
        let f=self.0.entry((bus.into(),address,channel)).or_insert(Failure {
            code:"module_feedback_timeout",consecutive_cycles:0,first_ms:at,last_ms:at,
        });
        f.consecutive_cycles=f.consecutive_cycles.saturating_add(1);f.last_ms=at;
        log::warn!("module_feedback_timeout gateway={:?} address={:02X} channel={} consecutive_cycles={} first_ms={} last_ms={} alert={}",bus,address,channel,f.consecutive_cycles,f.first_ms,at,f.consecutive_cycles>=3);
    }
    pub fn received(&mut self,bus:&str,address:u8,channel:u8) {
        if let Some(f)=self.0.remove(&(bus.into(),address,channel)) {
            log::info!("module_feedback_recovered gateway={:?} address={:02X} channel={} failed_cycles={}",bus,address,channel,f.consecutive_cycles);
        }
    }
    pub fn alert(&self,bus:&str,address:u8,channel:u8)->Option<Failure> {
        self.0.get(&(bus.into(),address,channel)).filter(|f|f.consecutive_cycles>=3).cloned()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn three_failed_cycles_raise_scoped_error_and_only_received_feedback_clears_it() {
        let mut h=Health::default();
        h.missing("bus",3,1,100);h.missing("bus",3,1,200);
        assert!(h.alert("bus",3,1).is_none());h.missing("bus",3,1,400);
        assert_eq!(h.alert("bus",3,1).unwrap().consecutive_cycles,3);
        assert!(h.alert("other",3,1).is_none());h.received("bus",3,2);
        assert!(h.alert("bus",3,1).is_some());h.received("bus",3,1);
        assert!(h.alert("bus",3,1).is_none());h.missing("bus",3,1,900);
        assert_eq!(h.0[&("bus".into(),3,1)].first_ms,900);
    }
}

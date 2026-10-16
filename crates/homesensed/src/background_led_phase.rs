//! Bounded observation of already accepted background LED work.
use crate::admission::{Admission,TaskGeneration};
use homesense_storage::journal::Status;
pub(super) struct Phase {
    generation:TaskGeneration,
    batches:Vec<Vec<u64>>,
    commands:Vec<u64>,
    deadline:u64,
}
impl Phase {
    pub(super) fn new(generation:TaskGeneration,batches:Vec<Vec<u64>>,deadline:u64)->Self {
        let commands=batches.iter().flatten().copied().collect();
        Self {generation,batches,commands,deadline}
    }
    pub(super) fn deadline(&self)->u64 {self.deadline}
    #[cfg(test)]
    pub(super) async fn finished(&self,admission:&Admission,controller:Option<uuid::Uuid>)->bool {
        self.completion(admission,controller).await.is_some()
    }
    pub(super) async fn completion(&self,admission:&Admission,controller:Option<uuid::Uuid>)->Option<bool> {
        let Some(controller)=controller else{return Some(false);};
        // One owner snapshot for the ordinary fanout, preserving the existing
        // per-request128-ID bound for larger configured queues. No wire batching.
        let mut terminal=!self.batches.iter().any(Vec::is_empty);
        let mut written=terminal;
        for commands in self.commands.chunks(128) {
            match admission.command_statuses(controller,self.generation.clone(),commands.to_vec()).await {
                Ok(statuses)=>{terminal &= statuses.iter().all(|status|matches!(status,Status::Written|Status::Observed|Status::Cancelled|Status::Failed|Status::Uncertain));
                    written &= statuses.iter().all(|status|matches!(status,Status::Written|Status::Observed));},
                Err(_)=>{terminal=false;written=false;},
            }
        }
        if admission.monotonic_ms()>=self.deadline {
            for batch in &self.batches {if let Some(run)=batch.first(){let _=admission.cancel_scene_run(controller,self.generation.clone(),*run).await;}}
            return Some(terminal && written);
        }
        terminal.then_some(written)
    }
}
#[cfg(test)]
#[path="background_led_phase_tests.rs"]
mod tests;

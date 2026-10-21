//! Bounded metadata priority. This queue never authorizes or emits device I/O.
use crate::admission::TaskGeneration;
use std::collections::VecDeque;

#[derive(Default)]
pub(crate) struct Queue {
    generation: Option<TaskGeneration>,
    names: VecDeque<String>,
}
impl Queue {
    const CAPACITY: usize = 256;

    fn scope(&mut self, generation: &TaskGeneration) {
        if self.generation.as_ref() != Some(generation) {
            self.names.clear();
            self.generation = Some(generation.clone());
        }
    }
    pub(crate) fn request(&mut self, generation: &TaskGeneration, name: &str) {
        self.scope(generation);
        if self.names.len() < Self::CAPACITY && !self.names.iter().any(|n| n == name) {
            self.names.push_back(name.to_owned());
        }
    }
    pub(crate) fn completed(&mut self, generation: &TaskGeneration, name: &str) {
        self.scope(generation);
        self.names.retain(|n| n != name);
    }
    /// Keep the existing two-group work budget. At most one priority group
    /// displaces background work; the rotating catalog always gets a slot.
    pub(crate) fn batch(&mut self, generation: &TaskGeneration, names: &[&str], cursor: &mut usize) -> Vec<usize> {
        self.scope(generation);
        if names.is_empty() { *cursor = 0; return vec![]; }
        let mut batch = vec![];
        while let Some(name) = self.names.pop_front() {
            if let Some(index) = names.iter().position(|n| *n == name) {
                batch.push(index);
                break;
            }
        }
        // Advance past a coincident priority group, rather than spending both
        // slots on it or starving its next background neighbor.
        for _ in 0..names.len() {
            let index = *cursor % names.len();
            *cursor = (index + 1) % names.len();
            if !batch.contains(&index) { batch.push(index); }
            if batch.len() == 2 { break; }
        }
        batch
    }
}

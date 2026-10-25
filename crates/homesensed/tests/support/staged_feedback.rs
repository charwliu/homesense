//! Synthetic responses only after completed writes. Original receipt, byte,
//! partial-I/O and source-order assertions remain in each integration test.
#![allow(dead_code)] // Each integration target exercises a different wire boundary.
use homesense_core::{Command, ExecutionContext};
use homesensed::{
    admission::{Admission, DispatchTicket},
    feedback::Feedback,
};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    time::Duration,
};
use uuid::Uuid;

pub struct Replies {
    config: homesense_config::Configuration,
    feedback: Feedback,
    epoch: u64,
}
impl Replies {
    pub fn new(actor: &Admission, config: &homesense_config::Configuration) -> Self {
        let epoch = 1; // Every caller opens an epoch-1 synthetic transport.
        let feedback = Feedback::new(
            config,
            ExecutionContext::Simulation,
            Uuid::parse_str(actor.boot_id()).unwrap().as_u128(),
            epoch,
        )
        .unwrap();
        Self {
            config: config.clone(),
            feedback,
            epoch,
        }
    }
    pub async fn written(&mut self, actor: &Admission, controller: Uuid, ticket: &DispatchTicket) {
        let value = match ticket.command.command {
            Command::Switch(on) => {
                if on {
                    0xf1
                } else {
                    0xf2
                }
            }
            Command::SetLevel { level, .. } => level.get(),
            _ => return,
        };
        let (binding, module) = self.config.binding(&ticket.command.device_id).unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
        let mut owner = homesensed::transport_owner::TransportOwner::new(
            actor.clone(),
            [(
                binding.gateway.clone(),
                hb_transport::Connection::for_simulation(
                    std::io::Cursor::new(vec![
                        0xae,
                        module.address().get(),
                        0xd0 + binding.channel.get(),
                        value,
                        0xee,
                    ]),
                    self.epoch,
                ),
            )]
            .into(),
        );
        self.feedback
            .apply(&owner.receive(&binding.gateway).await.unwrap())
            .unwrap();
        for _ in 0..2 {
            self.feedback
                .reconcile_control_phases(actor, controller)
                .await
                .unwrap();
        }
    }
    pub async fn received<S: Read + Write + Send + 'static>(
        &mut self,
        actor: &Admission,
        controller: Uuid,
        owner: &mut homesensed::transport_owner::TransportOwner<S>,
        gateway: &str,
    ) {
        tokio::time::sleep(Duration::from_millis(3)).await;
        self.feedback
            .apply(&owner.receive(gateway).await.unwrap())
            .unwrap();
        for _ in 0..2 {
            self.feedback
                .reconcile_control_phases(actor, controller)
                .await
                .unwrap();
        }
        // Exercise the real receive turnaround; another idle gateway is not
        // proof that this bus has already finished its quiet interval.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub struct Echo<W> {
    inner: W,
    tx: Vec<u8>,
    rx: VecDeque<u8>,
}
impl<W> Echo<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            tx: vec![],
            rx: VecDeque::new(),
        }
    }
}
impl<W: Read> Read for Echo<W> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.rx.is_empty() {
            return self.inner.read(out);
        }
        let n = out.len().min(self.rx.len());
        for byte in &mut out[..n] {
            *byte = self.rx.pop_front().unwrap();
        }
        Ok(n)
    }
}
impl<W: Write> Write for Echo<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.tx.extend_from_slice(&bytes[..n]);
        while self.tx.len() >= 5 {
            let frame = self.tx.drain(..5).collect::<Vec<_>>();
            if frame[0] == 0xae && frame[4] == 0xee && frame[2] <= 8 {
                let channels = if frame[2] == 0 {
                    (1..=8).collect::<Vec<_>>()
                } else {
                    vec![frame[2]]
                };
                for channel in channels {
                    self.rx
                        .extend([0xae, frame[1], 0xd0 + channel, frame[3], 0xee]);
                }
            }
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

//! One bridge publication for light and explicitly selected scene handles.
use crate::{runtime_hap::RuntimeHap, runtime_scene_hap::RuntimeSceneHap};
use hap::server::{IpServer, WriteBatchHandler};
use std::sync::Arc;
pub struct UnifiedHap {
    pub lights: RuntimeHap,
    pub scenes: Option<RuntimeSceneHap>,
    pub motors: Option<crate::runtime_motor_hap::RuntimeMotorHap>,
}
impl UnifiedHap {
    pub async fn install(&self, server: &IpServer) -> Result<(), String> {
        let mut handles = vec![self.lights.bridge.clone()];
        let mut groups = vec![];
        let mut addresses = vec![];
        for (binding, handle) in &self.lights.lights {
            handles.push(handle.clone());
            addresses.push((binding.aid, binding.on_iid));
            if let Some(iid) = binding.brightness_iid {
                addresses.push((binding.aid, iid));
            }
        }
        if !addresses.is_empty() {
            groups.push((addresses, self.lights.write_handler.clone()));
        }
        if let Some(scenes) = &self.scenes {
            let addresses = scenes
                .accessories
                .iter()
                .map(|(b, _)| (b.aid, b.on_iid))
                .collect();
            handles.extend(scenes.accessories.iter().map(|(_, h)| h.clone()));
            groups.push((addresses, scenes.write_handler.clone()));
        }
        if let Some(motors)=&self.motors {
            let addresses=motors.accessories.iter().map(|(b,_)|(b.aid,b.on_iid)).collect();
            handles.extend(motors.accessories.iter().map(|(_,h)|h.clone()));
            groups.push((addresses,motors.write_handler.clone()));
        }
        let handler: Arc<dyn WriteBatchHandler> = Arc::new(
            homesense_hap::write_router::WriteRouter::new(groups).map_err(|e| format!("{e:?}"))?,
        );
        server
            .install_startup_graph(handles, handler)
            .await
            .map_err(|e| e.to_string())
    }
    /// Independent from the automation owner so a writer holding HAP locks can
    /// still receive runtime acceptance while projection waits for those locks.
    pub async fn project(
        &mut self,
        mut snapshots: tokio::sync::watch::Receiver<homesense_state::Snapshot>,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), String> {
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            tokio::select! {
                result=snapshots.changed()=>{if result.is_err(){return Ok(());}},
                result=stop.changed()=>{if result.is_err() || *stop.borrow(){return Ok(());}}
            }
            let snapshot = snapshots.borrow_and_update().clone();
            self.lights.publish_snapshot(snapshot).await?;
            if let Some(motors)=&mut self.motors {motors.flush_resets().await?;}
            if let Some(scenes) = &mut self.scenes {
                scenes.flush_resets().await?;
            }
        }
    }
}

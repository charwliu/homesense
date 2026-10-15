//! Register shutdown signals before starting any live service operation.
#[cfg(unix)]
pub struct Signals {terminate:tokio::signal::unix::Signal,interrupt:tokio::signal::unix::Signal}
#[cfg(unix)]
impl Signals {
    pub fn install()->std::io::Result<Self>{use tokio::signal::unix::{signal,SignalKind};Ok(Self{terminate:signal(SignalKind::terminate())?,interrupt:signal(SignalKind::interrupt())?})}
    pub async fn wait(&mut self)->Result<(),String>{tokio::select!{value=self.terminate.recv()=>value.ok_or_else(||"SIGTERM stream ended".into()),value=self.interrupt.recv()=>value.ok_or_else(||"SIGINT stream ended".into())}}
}
#[cfg(not(unix))]
pub struct Signals;
#[cfg(not(unix))]
impl Signals {
    pub fn install()->std::io::Result<Self>{Ok(Self)}
    pub async fn wait(&mut self)->Result<(),String>{tokio::signal::ctrl_c().await.map_err(|error|error.to_string())}
}

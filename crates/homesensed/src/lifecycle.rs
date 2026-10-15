//! Explicit service lifetime; forever never manufactures an artificial deadline.
use std::{str::FromStr,time::Duration};
use tokio::{sync::watch,time::Instant};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum RunMode {For(Duration),Forever}
impl RunMode {
    pub fn validate(self)->Result<(),String>{match self {Self::Forever=>Ok(()),Self::For(value) if !value.is_zero() && value<=Duration::from_secs(86400)=>Ok(()),_=>Err("automation duration must be in (0,24h], or explicit forever".into())}}
}
impl FromStr for RunMode {
    type Err=String;
    fn from_str(value:&str)->Result<Self,String>{
        if value=="forever"{return Ok(Self::Forever);}
        let mode=Self::For(Duration::from_secs(value.parse::<u64>().map_err(|_|"expected seconds in 1..86400 or forever")?));mode.validate()?;Ok(mode)
    }
}
#[derive(Clone)]
pub struct RunControl {
    pub mode:RunMode,
    pub(crate) stop:watch::Receiver<bool>,
    #[cfg(test)] pub(crate) stop_after_barrier:Option<watch::Sender<bool>>,
    #[cfg(test)] pub(crate) queue_stop_before_barrier:Option<homesense_core::DeviceId>,
}
impl RunControl {
    pub fn new(mode:RunMode,stop:watch::Receiver<bool>)->Self{Self{mode,stop,#[cfg(test)] stop_after_barrier:None,#[cfg(test)] queue_stop_before_barrier:None}}
    pub(crate) fn stopped(&self)->bool{*self.stop.borrow() || self.stop.has_changed().is_err()}
}
pub(crate) async fn wait_for_stop(mut stop:watch::Receiver<bool>){let _=stop.wait_for(|value|*value).await;}
#[derive(Clone,Copy,Debug)]
pub(crate) struct Deadline(Option<Instant>);
impl Deadline {
    #[cfg(test)]
    pub(crate) fn at(value:Instant)->Self{Self(Some(value))}
    pub(crate) fn start(mode:RunMode)->Self{Self(match mode{RunMode::For(duration)=>Some(Instant::now()+duration),RunMode::Forever=>None})}
    pub(crate) fn raw(self)->Option<Instant>{self.0}
    pub(crate) fn expired(self)->bool{self.0.is_some_and(|value|Instant::now()>=value)}
    pub(crate) async fn wait(self){match self.0{Some(value)=>tokio::time::sleep_until(value).await,None=>std::future::pending().await}}
    /// An accepted publication ignores a shutdown request until bounded STOP
    /// draining and durable commit resolve. A fatal timeout remains uncertain.
    pub(crate) fn operation(self,bound:Duration)->Instant{self.0.map_or_else(||Instant::now()+bound,|deadline|deadline.min(Instant::now()+bound))}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused=true)]
    async fn forever_has_no_deadline_after_many_days_while_finite_still_expires(){
        use futures_util::FutureExt;
        let forever=Deadline::start(RunMode::Forever);let finite=Deadline::start(RunMode::For(Duration::from_secs(86400)));
        tokio::time::advance(Duration::from_secs(86400*30)).await;
        assert!(!forever.expired());assert!(forever.wait().now_or_never().is_none());assert!(finite.expired());assert!(finite.wait().now_or_never().is_some());
    }
    #[test]
    fn lifetime_parser_preserves_bounded_contract_and_requires_explicit_forever(){
        assert_eq!("forever".parse::<RunMode>().unwrap(),RunMode::Forever);
        for valid in ["1","86400"]{assert!(valid.parse::<RunMode>().is_ok());}
        for invalid in ["0","86401","18446744073709551615","-1","1.5","Forever",""]{assert!(invalid.parse::<RunMode>().is_err());}
    }
}

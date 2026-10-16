//! Exact Web source authority retained by an approved logical operation and
//! its derived work. Physical expansion never broadens the originating grant.
use crate::{automation_control::Action, web_auth::ModuleOffLease};
use serde_json::{Value, json};
use uuid::Uuid;
/// One immutable proof is shared by all derived work. Keep the handle small:
/// Admission is retained repeatedly through nested async control operations.
#[derive(Clone)]
pub struct Source {
    proof: std::sync::Arc<Proof>,
}
struct Proof {
    lease: ModuleOffLease,
    controller: Uuid,
    revision: u64,
    signature: Value,
    administrator: bool,
}
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommandSource(<opaque>)")
    }
}
type Description = (Uuid, u64, Value, Vec<String>, Vec<String>, bool);
fn describe(action: &Action) -> Option<Description> {
    Some(match action {
        Action::DeviceCommand {
            controller,
            revision,
            device,
            action,
        } => (
            *controller,
            *revision,
            json!({"kind":"device","id":device.as_str(),"action":action}),
            vec![device.as_str().into()],
            vec![],
            false,
        ),
        Action::ModuleCommand {
            controller,
            revision,
            module_id,
            channel,
            allowed_devices,
            action,
        } => (
            *controller,
            *revision,
            json!({"kind":"module","module":module_id,"channel":channel,"allowed":allowed_devices,"action":action}),
            allowed_devices.iter().cloned().collect(),
            vec![],
            true,
        ),
        Action::SceneAction {
            controller,
            revision,
            scene,
            execute,
        } => (
            *controller,
            *revision,
            json!({"kind":"scene","scene":scene,"execute":execute}),
            vec![],
            vec![scene.clone()],
            false,
        ),
        Action::SceneRelative {
            controller,
            revision,
            scene,
            request_id,
        } => (
            *controller,
            *revision,
            json!({"kind":"relative","scene":scene,"request":request_id}),
            vec![],
            vec![scene.clone()],
            false,
        ),
        Action::SceneActions {
            controller,
            revision,
            scenes,
        } => (
            *controller,
            *revision,
            json!({"kind":"scenes","scenes":scenes}),
            vec![],
            scenes.iter().map(|(id, _)| id.clone()).collect(),
            false,
        ),
        Action::SceneWrites {
            controller,
            revision,
            scenes,
        }
        | Action::SceneActivityWrites {
            controller,
            revision,
            scenes,
        } => (
            *controller,
            *revision,
            json!({"kind":if matches!(action,Action::SceneWrites{..}){"writes"}else{"activity_writes"},"scenes":scenes}),
            vec![],
            scenes.iter().map(|(id, _)| id.clone()).collect(),
            false,
        ),
        Action::SceneCancel {
            controller,
            revision,
            run,
        } => (
            *controller,
            *revision,
            json!({"kind":"cancel","run":run}),
            vec![],
            vec![],
            false,
        ),
        Action::ModuleQuery {
            controller,
            revision,
            module_id,
            scope,
            submit,
        } => (
            *controller,
            *revision,
            json!({"kind":"module_query","module":module_id,"scope":scope,"submit":submit}),
            vec![],
            vec![],
            true,
        ),
        _ => return None,
    })
}
impl Source {
    pub(crate) fn supports(action: &Action) -> bool {
        describe(action).is_some()
    }
    pub(crate) fn capture(
        lease: ModuleOffLease,
        action: &Action,
        scene_hint: Option<&str>,
    ) -> Option<Self> {
        let (controller, revision, signature, devices, mut scenes, administrator) =
            describe(action)?;
        if matches!(action, Action::SceneCancel { .. }) {
            scenes.push(scene_hint?.to_owned());
        }
        if !lease.valid()
            || lease.controller() != controller
            || (administrator && !lease.is_administrator())
            || devices.iter().any(|id| !lease.devices().contains(id))
            || scenes.iter().any(|id| !lease.scenes().contains(id))
        {
            return None;
        }
        Some(Self {
            proof: std::sync::Arc::new(Proof {
                lease,
                controller,
                revision,
                signature,
                administrator,
            }),
        })
    }
    pub(crate) fn matches(&self, action: &Action) -> bool {
        describe(action).is_some_and(|(controller, revision, signature, ..)| {
            self.valid_for(controller, revision) && self.proof.signature == signature
        })
    }
    pub(crate) fn valid(&self) -> bool {
        self.proof.lease.valid()
            && (!self.proof.administrator || self.proof.lease.is_administrator())
    }
    pub(crate) fn permits_device(&self, device: &str) -> bool {
        self.valid() && self.proof.lease.devices().contains(device)
    }
    pub(crate) fn valid_for(&self, controller: Uuid, revision: u64) -> bool {
        self.proof.controller == controller && self.proof.revision == revision && self.valid()
    }
    pub(crate) fn with_valid<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        self.proof.lease.with_valid(operation)
    }
}

/// Hold both exact source generations through conditional queue acceptance.
/// An old Written source revoked after the decision cannot authorize its STOP.
pub(crate) fn commit_with_dependency<T>(source:Option<&Source>, dependency:Option<&Source>, operation:impl FnOnce()->T)->Option<T> {
    match (source,dependency) {
        (Some(source),Some(dependency))=>source.proof.lease.with_valid_dependency(&dependency.proof.lease,operation),
        (_,None)=>commit(source,operation),
        (None,Some(_))=>None,
    }
}

/// Internal mutation boundary. This takes no credential or scope from payloads.
/// The same immutable source read guard is used for durable admission and I/O.
pub(crate) fn commit<T>(source: Option<&Source>, operation: impl FnOnce() -> T) -> Option<T> {
    match source {
        Some(source) => source.with_valid(operation),
        None => Some(operation()),
    }
}

#[cfg(test)]
#[test]
fn source_proof_handle_stays_small_in_nested_control_futures() {
    assert_eq!(std::mem::size_of::<Source>(), std::mem::size_of::<usize>());
    assert!(std::mem::size_of::<Option<Source>>() <= 2 * std::mem::size_of::<usize>());
}

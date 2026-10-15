//! Reviewed immediate scene definitions. No implicit off policy and no timing
//! conversion. Delayed source members must use the timeline adapter instead.
use crate::rule::{Action, Node};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone)]
pub struct Definition {
    pub activate: Vec<Node>,
    pub deactivate: Option<Vec<Node>>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    MissingScene(String),
    UnknownDeactivation(String),
    Cycle(String),
    Budget,
    MissingSource,
    AmbiguousScene(String),
}
pub fn expand(
    nodes: &[Node],
    scenes: &BTreeMap<String, Definition>,
    max_nodes: usize,
) -> Result<Vec<Node>, Error> {
    expand_with_deferred(nodes, scenes, &BTreeSet::new(), max_nodes)
}
/// Retain explicitly linked deferred activations as AST actions. This does not
/// schedule them or authorize execution; the caller must durably accept their
/// variables, causal budget and delayed work together. Deactivation is rejected
/// until an explicit policy exists, never inferred as cancelling or turning off.
pub fn expand_with_deferred(
    nodes: &[Node],
    scenes: &BTreeMap<String, Definition>,
    deferred: &BTreeSet<String>,
    max_nodes: usize,
) -> Result<Vec<Node>, Error> {
    expand_with_deactivations(nodes, scenes, deferred, &BTreeMap::new(), max_nodes)
}
/// Reviewed source-off means the complete target activation, preceded by a
/// trusted guard. It never inverts source outputs or cancels source timelines.
pub fn expand_with_deactivations(nodes:&[Node], scenes:&BTreeMap<String,Definition>, deferred:&BTreeSet<String>, deactivations:&BTreeMap<String,String>, max_nodes:usize)->Result<Vec<Node>,Error> {
    if let Some(id) = deferred.iter().find(|id| scenes.contains_key(*id)) {
        return Err(Error::AmbiguousScene(id.clone()));
    }
    if max_nodes == 0 || max_nodes > 65536 {
        return Err(Error::Budget);
    }
    struct Links<'a> {scenes:&'a BTreeMap<String,Definition>,deferred:&'a BTreeSet<String>,deactivations:&'a BTreeMap<String,String>}
    fn visit(
        nodes: &[Node],
        links:&Links<'_>,
        remaining: &mut usize,
        path: &mut Vec<String>,
        depth: usize,
        prefix: &str,
    ) -> Result<Vec<Node>, Error> {
        let Links{scenes,deferred,deactivations}=links;
        if depth > 64 {
            return Err(Error::Budget);
        }
        let mut result = Vec::new();
        for node in nodes {
            if *remaining == 0 {
                return Err(Error::Budget);
            }
            *remaining -= 1;
            if node.source.is_empty() {
                return Err(Error::MissingSource);
            }
            let source = if prefix.is_empty() {
                node.source.clone()
            } else {
                format!("{prefix} -> {}", node.source)
            };
            if source.len() > 8192 {
                return Err(Error::Budget);
            }
            match &node.action {
                Action::Scene { id, activate } => {
                    if path.contains(id) {
                        return Err(Error::Cycle(id.clone()));
                    }
                    if *activate && deactivations.contains_key(id) {result.push(Node {source:source.clone(),action:Action::SceneActivation {scene:id.clone()}});}
                    if !activate && let Some(target)=deactivations.get(id) {
                        path.push(id.clone());
                        result.push(Node { source:format!("{source} -> deactivate:{id}"), action:Action::RequireQuiescence { scene:id.clone() } });
                        let call=Node { source:format!("deactivation-target:{target}"), action:Action::Scene { id:target.clone(), activate:true } };
                        result.extend(visit(&[call], links, remaining, path, depth+1, &source)?);
                        path.pop();
                        continue;
                    }
                    if deferred.contains(id) {
                        if !activate {return Err(Error::UnknownDeactivation(id.clone()));}
                        result.push(Node {source,action:node.action.clone()});
                        continue;
                    }
                    let scene = scenes
                        .get(id)
                        .ok_or_else(|| Error::MissingScene(id.clone()))?;
                    let members = if *activate {
                        &scene.activate
                    } else {
                        scene
                            .deactivate
                            .as_ref()
                            .ok_or_else(|| Error::UnknownDeactivation(id.clone()))?
                    };
                    path.push(id.clone());
                    result.extend(visit(members, links, remaining, path, depth + 1, &source)?);
                    path.pop();
                }
                Action::If {
                    condition,
                    then_actions,
                    else_actions,
                } => result.push(Node {
                    source,
                    action: Action::If {
                        condition: condition.clone(),
                        then_actions: visit(
                            then_actions,
                            links,
                            remaining,
                            path,
                            depth + 1,
                            prefix,
                        )?,
                        else_actions: visit(
                            else_actions,
                            links,
                            remaining,
                            path,
                            depth + 1,
                            prefix,
                        )?,
                    },
                }),
                action => result.push(Node {
                    source,
                    action: action.clone(),
                }),
            }
        }
        Ok(result)
    }
    visit(nodes, &Links{scenes,deferred,deactivations}, &mut max_nodes.clone(), &mut vec![], 0, "")
}

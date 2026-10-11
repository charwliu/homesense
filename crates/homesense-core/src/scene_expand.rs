//! Reviewed immediate scene definitions. No implicit off policy and no timing
//! conversion. Delayed source members must use the timeline adapter instead.
use crate::rule::{Action, Node};
use std::collections::BTreeMap;
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
}
pub fn expand(
    nodes: &[Node],
    scenes: &BTreeMap<String, Definition>,
    max_nodes: usize,
) -> Result<Vec<Node>, Error> {
    if max_nodes == 0 || max_nodes > 65536 {
        return Err(Error::Budget);
    }
    fn visit(
        nodes: &[Node],
        scenes: &BTreeMap<String, Definition>,
        remaining: &mut usize,
        path: &mut Vec<String>,
        depth: usize,
        prefix: &str,
    ) -> Result<Vec<Node>, Error> {
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
                    result.extend(visit(members, scenes, remaining, path, depth + 1, &source)?);
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
                            scenes,
                            remaining,
                            path,
                            depth + 1,
                            prefix,
                        )?,
                        else_actions: visit(
                            else_actions,
                            scenes,
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
    visit(nodes, scenes, &mut max_nodes.clone(), &mut vec![], 0, "")
}

//! Disabled source structure. Unknown actions are never translated to no-ops.
use roxmltree::Node;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
#[derive(Debug, Serialize)]
pub struct Program {
    pub owner: String,
    pub enabled: bool,
    pub nodes: Vec<Code>,
}
#[derive(Debug, Serialize)]
pub struct Code {
    pub id: String,
    pub source_type: String,
    pub source_container: Vec<String>,
    pub source_enabled: bool,
    pub kind: &'static str,
    pub target: Option<String>,
    pub payload_sha256: Option<String>,
    pub children: Vec<Code>,
}
pub(crate) fn code(
    node: Node<'_, '_>,
    root: bool,
    depth: usize,
    seen: &mut BTreeSet<String>,
) -> Result<Code, String> {
    if depth > 64 {
        return Err("program nesting exceeds 64".into());
    }
    let id = super::required(node, "id")?.to_owned();
    if !seen.insert(id.clone()) {
        return Err("duplicate code ID in event".into());
    }
    let source_type = super::required(node, "type")?.to_owned();
    let target = super::required(node, "device")?;
    let source_enabled = match super::text(node, "enabled") {
        Some("True") => true,
        Some("False") => false,
        _ => return Err("invalid code enabled field".into()),
    };
    let payload = node.children().find(|n| n.has_tag_name("cmdcond"));
    let empty = payload.is_some_and(|p| {
        !p.children()
            .any(|n| n.is_element() || n.text().is_some_and(|s| !s.trim().is_empty()))
    });
    let container = root && id == "0" && target == "0" && source_type == "1" && empty;
    let children = node
        .descendants()
        .skip(1)
        .filter(|n| {
            n.has_tag_name("codeitem")
                && n.ancestors().skip(1).find(|p| p.has_tag_name("codeitem")) == Some(node)
        })
        .map(|n| code(n, false, depth + 1, seen))
        .collect::<Result<Vec<_>, _>>()?;
    let mut source_container: Vec<_> = node
        .ancestors()
        .skip(1)
        .take_while(|n| !n.has_tag_name("codeitem") && !n.has_tag_name("event"))
        .filter(|n| n.is_element())
        .map(|n| n.tag_name().name().to_owned())
        .collect();
    source_container.reverse();
    Ok(Code {
        id,
        source_type,
        source_container,
        source_enabled,
        kind: if container {
            "sequence_container"
        } else {
            "unsupported"
        },
        target: if container { None } else { Some(target.into()) },
        payload_sha256: payload.map(|n| {
            format!(
                "{:x}",
                Sha256::digest(&n.document().input_text().as_bytes()[n.range()])
            )
        }),
        children,
    })
}

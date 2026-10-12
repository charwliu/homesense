//! Scope-filtered history reads. No command payloads or replay capability.
use crate::{automation_control::{Action,Reply},web_api::Runtime,web_auth::Grant};
use hyper::{Request,StatusCode,body::Bytes};
use serde_json::{Value,json};
use homesense_storage::scene_history::Cursor;
struct Input {scene:String,revision:u64,before:Option<Cursor>,limit:usize}
fn decode(text:&str)->Result<String,()> {
    let mut bytes=Vec::new();let mut input=text.bytes();
    while let Some(c)=input.next(){bytes.push(match c {b'+'=>b' ',b'%'=>{let a=(input.next().ok_or(())? as char).to_digit(16).ok_or(())?;let b=(input.next().ok_or(())? as char).to_digit(16).ok_or(())?;(a*16+b) as u8},_=>c});}
    String::from_utf8(bytes).map_err(|_|())
}
fn parse(query:Option<&str>)->Result<Input,()> {
    let query=query.ok_or(())?;if query.len()>2048{return Err(());}
    let mut fields=std::collections::BTreeMap::new();
    for part in query.split('&') {let (k,v)=part.split_once('=').ok_or(())?;if !matches!(k,"scene"|"revision"|"limit"|"before_boot"|"before_run") || fields.insert(k,decode(v)?).is_some(){return Err(());}}
    let scene=fields.remove("scene").ok_or(())?;
    if scene.is_empty() || scene.len()>256 || scene.chars().any(char::is_control){return Err(());}
    let number=|s:&str|{if s.is_empty() || !s.bytes().all(|b|b.is_ascii_digit()){return Err(());}s.parse::<u64>().map_err(|_|()).and_then(|v|if v>0 && v<=i64::MAX as u64{Ok(v)}else{Err(())})};
    let revision=number(&fields.remove("revision").ok_or(())?)?;
    let limit=fields.remove("limit").map(|s|number(&s)).transpose()?.unwrap_or(20);if limit>100{return Err(());}
    let before=match (fields.remove("before_boot"),fields.remove("before_run")) {
        (None,None)=>None,
        (Some(boot),Some(run)) if uuid::Uuid::parse_str(&boot).is_ok() && !run.is_empty() && run.len()<=128 && run.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_')=>Some(Cursor{boot,run}),
        _=>return Err(()),
    };
    Ok(Input{scene,revision,before,limit:limit as usize})
}
pub async fn read(runtime:&Runtime,grant:&Grant,request:&Request<Bytes>)->(StatusCode,Value) {
    let error=|code,message|(code,json!({"error":message}));
    if request.method()!=hyper::Method::GET{return error(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
    if !request.body().is_empty(){return error(StatusCode::BAD_REQUEST,"unexpected_body");}
    let Ok(input)=parse(request.uri().query()) else{return error(StatusCode::BAD_REQUEST,"invalid_history_query");};
    if !grant.may_read(&input.scene) || !runtime.scenes.contains_key(&input.scene){return error(StatusCode::FORBIDDEN,"scene_not_permitted");}
    if input.revision!=runtime.revision{return error(StatusCode::CONFLICT,"configuration_changed");}
    match runtime.control.request(Action::SceneHistory{controller:grant.controller,revision:input.revision,scene:input.scene,before:input.before,limit:input.limit}).await {
        Ok(Reply::SceneHistory(page))=>(StatusCode::OK,json!({"revision":page.revision.to_string(),"scene":page.definition,"runs":page.runs.iter().map(|r|json!({"boot":r.identity.boot,"run":r.identity.run,"status":format!("{:?}",r.status),"started_ms":r.started_ms.to_string(),"updated_ms":r.updated_ms.to_string(),"version":r.version.to_string()})).collect::<Vec<_>>(),"next_before":page.next_before.map(|c|json!({"boot":c.boot,"run":c.run}))})),
        _=>error(StatusCode::SERVICE_UNAVAILABLE,"scene_history_unavailable"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn strict_query_decoding() {
        assert_eq!(parse(Some("scene=%E4%B9%A6%E6%88%BF%2B&revision=9223372036854775807")).unwrap().scene,"书房+");
        for s in ["scene=x&revision=1&scene=y","scene=%FF&revision=1","scene=%&revision=1","scene=%00&revision=1","scene=x&revision=0","scene=x&revision=1&limit=101","scene=x&revision=1&before_run=1","scene=x&revision=1&token=x"] {assert!(parse(Some(s)).is_err(),"{s}");}
    }
}

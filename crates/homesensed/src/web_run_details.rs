//! Paginated execution facts, with independent scene and device visibility.
use crate::{automation_control::{Action,Reply},web_api::Runtime,web_auth::Grant};
use hyper::{Request,StatusCode,body::Bytes};
use serde_json::{Value,json};
use homesense_storage::scene_run::Query;
fn parse(request:&Request<Bytes>)->Result<(u64,Query),()> {
    let key=request.uri().path().strip_prefix("/api/v1/runs/").and_then(|s|s.strip_suffix("/details")).ok_or(())?;
    let (boot,run)=key.split_once('/').ok_or(())?;
    if uuid::Uuid::parse_str(boot).is_err() || run.is_empty() || run.len()>128 || !run.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_'){return Err(());}
    let query=request.uri().query().ok_or(())?;if query.len()>256{return Err(());}
    let mut fields=std::collections::BTreeMap::new();
    for part in query.split('&'){let (k,v)=part.split_once('=').ok_or(())?;if !matches!(k,"revision"|"after"|"limit"|"kind") || fields.insert(k,v).is_some(){return Err(());}}
    let number=|s:&str|{if s.is_empty()||!s.bytes().all(|b|b.is_ascii_digit()){return Err(());}s.parse::<u64>().map_err(|_|()).and_then(|n|if n<=i64::MAX as u64{Ok(n)}else{Err(())})};
    let revision=number(fields.remove("revision").ok_or(())?)?;if revision==0{return Err(());}
    let after=number(fields.remove("after").unwrap_or("0"))?;
    let limit=number(fields.remove("limit").unwrap_or("20"))?;if !(1..=100).contains(&limit){return Err(());}
    let boot=boot.into();let run=run.into();let limit=limit as usize;
    let query=match fields.remove("kind").ok_or(())? {
        "commands"=>Query::Run{boot,run,after,limit},"cancellations"=>Query::Cancellations{boot,run,after,limit},"expiries"=>Query::Expiries{boot,run,after,limit},_=>return Err(()),
    };
    Ok((revision,query))
}
fn filter_devices(value:&mut Value,grant:&Grant)->bool {
    let mut redacted=false;
    for key in ["commands","cancellations","expiries"] {
        if let Some(rows)=value[key].as_array_mut(){rows.retain(|row|{let permitted=row["device"].as_str().is_some_and(|id|grant.devices.contains(id));redacted|=!permitted;permitted});}
    }
    redacted
}
pub async fn read(runtime:&Runtime,grant:&Grant,request:&Request<Bytes>)->(StatusCode,Value) {
    let error=|code,message|(code,json!({"error":message}));
    if request.method()!=hyper::Method::GET{return error(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
    if !request.body().is_empty(){return error(StatusCode::BAD_REQUEST,"unexpected_body");}
    let Ok((revision,query))=parse(request) else{return error(StatusCode::BAD_REQUEST,"invalid_detail_query");};
    if revision!=runtime.revision{return error(StatusCode::CONFLICT,"configuration_changed");}
    match runtime.control.request(Action::RunDetails{controller:grant.controller,revision,query}).await {
        Ok(Reply::RunDetails(Some((scene,mut value)))) if runtime.scenes.contains_key(&scene)&&grant.may_read(&scene)=>{
            let redacted=filter_devices(&mut value,grant);
            value["redacted"]=json!(redacted);value["scene"]=json!(scene);(StatusCode::OK,value)
        },
        Ok(Reply::RunDetails(_))=>error(StatusCode::NOT_FOUND,"run_not_found"),
        _=>error(StatusCode::SERVICE_UNAVAILABLE,"run_details_unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn detail_scope_and_query_bounds() {
        let grant=Grant{controller:uuid::Uuid::new_v4(),role:crate::web_auth::Role::Viewer,scenes:["light".into()].into(),devices:["allowed".into()].into()};
        let mut value=json!({"commands":[{"device":"allowed"},{"device":"secret"}],"cancellations":[{"device":"secret"}],"expiries":[{"device":"allowed"}],"next_after":"9223372036854775807"});
        assert!(filter_devices(&mut value,&grant));assert_eq!(value["commands"],json!([{"device":"allowed"}]));assert_eq!(value["cancellations"],json!([]));assert_eq!(value["expiries"],json!([{"device":"allowed"}]));assert_eq!(value["next_after"],"9223372036854775807");
        let base=format!("/api/v1/runs/{}/1/details?",grant.controller);
        for kind in ["commands","cancellations","expiries"] {assert!(parse(&Request::builder().uri(format!("{base}revision=1&kind={kind}")).body(Bytes::new()).unwrap()).is_ok());}
        for query in ["revision=1&kind=commands&after=-1","revision=1&kind=commands&limit=101","revision=1&kind=commands&kind=expiries","revision=1&kind=commands&token=x","revision=1&kind=other","revision=0&kind=commands"] {assert!(parse(&Request::builder().uri(format!("{base}{query}")).body(Bytes::new()).unwrap()).is_err());}
    }
}

#!/usr/bin/env python3
"""Private, read-only dependency review of original Control4 source.

This is an evidence worksheet, never an automation/configuration generator.
Names, saved values, driver Lua and identity databases are not read or emitted.
"""
import argparse
import collections
import hashlib
import io
import json
import os
from pathlib import Path
import xml.etree.ElementTree as ET
import zipfile


def xml_tree(data):
    if len(data) > 32 * 1024 * 1024 or b"<!DOCTYPE" in data.upper():
        raise ValueError("oversized XML or forbidden DTD")
    root = ET.fromstring(data)
    if sum(1 for _ in root.iter()) > 500_000:
        raise ValueError("XML node budget exceeded")
    return root


def source_bytes(data):
    if len(data) > 64 * 1024 * 1024:
        raise ValueError("input exceeds 64 MiB")
    if not data.startswith(b"PK"):
        return data, "xml"
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        matches = [entry for entry in entries if entry.filename == "project.xml"]
        if len(entries) > 10_000 or len(matches) != 1:
            raise ValueError("exactly one root project.xml required")
        entry = matches[0]
        if entry.is_dir() or entry.flag_bits & 1 or entry.file_size > 32 * 1024 * 1024:
            raise ValueError("invalid root project.xml")
        if entry.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
            raise ValueError("unsupported project compression")
        with archive.open(entry) as stream:
            return stream.read(32 * 1024 * 1024 + 1), "c4p"


def child(node, tag, required=True):
    values = node.findall(tag)
    if len(values) > 1 or required and not values:
        raise ValueError(f"missing or duplicate {tag}")
    return values[0] if values else None


def text(node, tag, required=True):
    value = child(node, tag, required)
    if value is None:
        return None
    if list(value):
        raise ValueError(f"non-scalar {tag}")
    return (value.text or "").strip()


def source_empty(event):
    roots = event.findall("codeitem")
    if len(roots) != 1 or any(n.tag not in ("deviceid", "eventid", "codeitem") for n in event):
        return False
    code = roots[0]
    tags = [n.tag for n in code]
    allowed = {"id", "device", "type", "enabled", "cmdcond", "subitems", "display", "creator", "creatorstate"}
    if code.attrib or len(tags) != len(set(tags)) or set(tags) - allowed:
        return False
    if any(n.attrib or list(n) for n in code if n.tag not in ("cmdcond", "subitems")):
        return False
    if any(text(code, k, False) != v for k, v in (("id", "0"), ("device", "0"), ("type", "1"), ("enabled", "True"))):
        return False
    return all((n := child(code, tag, False)) is not None and not n.attrib and not list(n)
               and not (n.text or "").strip() for tag in ("cmdcond", "subitems"))


def parameters(payload):
    result = []
    for param in payload.findall("params/param"):
        value = child(param, "value", False)
        static = child(value, "static", False) if value is not None else None
        white = lambda n: not (n.text or "").strip() and all(not (c.tail or "").strip() for c in n)
        unmodeled = (len(payload.findall("params")) != 1 or bool(payload.find("params").attrib)
                     or not white(payload.find("params"))
                     or bool(param.attrib) or len(param) != 2 or {n.tag for n in param} != {"name", "value"}
                     or bool(param.find("name").attrib)
                     or not white(param) or value is None or set(value.attrib) != {"type"}
                     or not white(value) or static is None or bool(static.attrib) or bool(list(static)))
        result.append({"name": text(param, "name", False),
                       "declared_type": value.get("type") if value is not None else None,
                       "static": (static.text or "").strip() if static is not None and not list(static) else None,
                       "shape": "static" if value is not None and len(value) == 1 and static is not None else "unreviewed_dynamic_or_missing",
                       "unmodeled": unmodeled})
    return result


def descriptor_metadata(data, filenames):
    """Read only selected plaintext descriptors; never open Lua payload entries."""
    if not data.startswith(b"PK"):
        return []
    def entry_bytes(archive, name):
        entries = [e for e in archive.infolist() if e.filename == name]
        if len(entries)!=1 or entries[0].is_dir() or entries[0].flag_bits & 1 or entries[0].file_size>32*1024*1024:
            raise ValueError("ambiguous or unreadable descriptor")
        with archive.open(entries[0]) as stream:
            return stream.read(32*1024*1024+1)
    result=[]
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for filename in sorted(filenames):
            path="drivers/"+filename
            if path not in archive.namelist():
                result.append({"filename":filename,"descriptor_available":False})
                continue
            raw=entry_bytes(archive,path)
            encrypted=False
            if filename.endswith(".c4z"):
                with zipfile.ZipFile(io.BytesIO(raw)) as package:
                    if len(package.infolist())>10_000:
                        raise ValueError("descriptor package entry budget exceeded")
                    encrypted=any(n.endswith(".encrypted") for n in package.namelist())
                    raw=entry_bytes(package,"driver.xml")
            descriptor=xml_tree(raw)
            capabilities=[]
            for section in descriptor.iter("capabilities"):
                capabilities.extend({"name":n.tag,"source_value":(n.text or "").strip()} for n in section if not list(n))
            ramp_properties=[]
            for prop in descriptor.iter("property"):
                if prop.findtext("name")=="Ramp Timer":
                    ramp_properties.append({"source_type":prop.findtext("type"),"source_default":prop.findtext("default"),
                        "options":[{"tag":n.tag,"source_value":(n.text or "").strip(),"attributes":dict(n.attrib)} for n in prop.iter() if n is not prop and not list(n) and n.tag not in ("name","description")],
                        "semantics_verified":False,"scope":"driver setting only, not archive scene rate"})
            result.append({"filename":filename,"descriptor_available":True,"declared_capabilities":capabilities,
                           "ramp_timer_properties":ramp_properties,"encrypted_payload_entries_present":encrypted,
                           "lua_read_or_executed":False,"runtime_conformance_verified":False})
    return result


def review(data, typed=None, prior=None, prior_variables=None):
    xml, source_format = source_bytes(data)
    root = xml_tree(xml)
    source = {"input_format": source_format, "input_sha256": hashlib.sha256(data).hexdigest(),
              "project_xml_sha256": hashlib.sha256(xml).hexdigest()}
    source_parents = {n: p for p in root.iter() for n in p}
    items = {}
    for item in child(root, "systemitems").iter("item"):
        key = text(item, "id")
        if key in items:
            raise ValueError("duplicate source item")
        items[key] = item
    driver = lambda key: (items[key].findtext("itemdata/config_data_file") or "").strip() if key in items else None
    def driver_chain(key):
        item = items.get(key)
        chain = []
        while item is not None:
            if item.tag == "item":
                filename = (item.findtext("itemdata/config_data_file") or "").strip()
                if filename:
                    chain.append(filename)
            item = source_parents.get(item)
        return chain
    scenes = []
    by_scene_id = collections.defaultdict(list)
    calendars = []
    agent_schedules = []
    for owner, item in items.items():
        raw = text(item, "state", False)
        if raw:
            state = xml_tree(raw.encode())
            for schedule in state.iter():
                if schedule.tag == "schedule" or schedule.tag.endswith("_schedule"):
                    agent_schedules.append({"source_item": owner, "driver": driver(owner), "source_tag": schedule.tag,
                                            "source_fields": [{"tag": n.tag, "value": (n.text or "").strip(), "attributes": dict(n.attrib)} for n in schedule.iter() if n is not schedule and not list(n)],
                                            "review_gates": ["agent_owned_behavior", "schedule_units_and_timezone", "dependency_without_event_program"],
                                            "runtime_task_generated": False})
            for node in state.iter("AdvScene"):
                scene_id = text(node, "scene_id")
                key = f"scene:{owner}:{scene_id}"
                if any(s["key"] == key for s in scenes):
                    raise ValueError("duplicate source scene")
                members = []
                for ordinal, member in enumerate(node.findall("all_members/AdvSceneMember")):
                    target = text(member, "device_id")
                    elements = [{"ordinal": index, "level": text(e, "level"),
                                 "delay": text(e, "delay"), "rate": text(e, "rate")}
                                for index, e in enumerate(member.findall("all_elements/element"))]
                    for element in elements:
                        for field in ("level", "delay", "rate"):
                            raw_number = element[field]
                            if not raw_number.isascii() or not raw_number.isdigit() or len(raw_number)>20 or int(raw_number)>2**64-1:
                                raise ValueError("invalid source scene number")
                        if int(element["level"])>100:
                            raise ValueError("source scene level outside 0..100")
                    members.append({"ordinal": ordinal, "source_item": target, "source_item_exists": target in items,
                                    "source_driver": driver(target), "source_driver_chain": driver_chain(target),
                                    "physical_binding_verified": False, "elements": elements,
                                    "tracking": {n.tag: (n.text or "").strip() for n in member
                                                 if n.tag in ("track_mode", "track_lvl", "flash", "ignore_ramp")}})
                scenes.append({"key": key, "source_scene_id": scene_id, "members": members,
                               "metadata": {n.tag: (n.text or "").strip() for n in node
                                            if n.tag not in ("name", "scene_id", "all_members") and not list(n)},
                               "referenced_by": [], "review_gates": ["physical_bindings", "source_order", "delay_and_rate_semantics", "tracking", "deactivation"],
                               "prior_limited_status": None, "semantic_approved": False})
                by_scene_id[scene_id].append(key)
        for value in item.findall("state_values/value"):
            if value.get("name") == "entry_data" and value.text:
                for entry in xml_tree(value.text.encode()):
                    event_id = text(entry, "eventid")
                    fields = []
                    def leaves(node, path):
                        for n in node:
                            if n.tag == "description":
                                continue
                            if list(n):
                                leaves(n, path + [n.tag])
                            else:
                                fields.append({"path": "/".join(path + [n.tag]), "source_value": (n.text or "").strip(), "attributes": dict(n.attrib)})
                    leaves(entry, [])
                    calendars.append({"source_event": f"event:{owner}:{event_id}", "driver": driver(owner), "source_fields": fields,
                                      "review_gates": ["calendar_units_and_timezone", "agent_owned_behavior", "trigger_semantics"],
                                      "runtime_task_generated": False})
    variables = {}
    for scene in scenes:
        toggle = scene["metadata"].get("toggle_id")
        targets = by_scene_id.get(toggle, [])
        scene["toggle_dependency"] = {"source_id": toggle, "candidates": targets, "resolved": len(targets) == 1,
                                      "execution_scope": "one target activation, not recursive toggle traversal",
                                      "reference": "https://snap-one.github.io/docs-driverworks-proxyprotocol-lightv2/#asynchronous-commands",
                                      "unresolved_sentinel_semantics_known": False}
        if len(targets) == 1:
            scene["closed_source_deactivation_subgate"] = "source_toggle_destination_resolved"
    section = child(root, "variables", False)
    for node in section if section is not None else []:
        if node.tag != "variable" or not node.get("variableid") or node.get("variableid") in variables:
            raise ValueError("invalid or duplicate variable declaration")
        key = node.get("variableid")
        variables[key] = {"source_id": key, "source_type": node.get("type"), "reads": [], "writes": [],
                          "source_owner": node.get("deviceid"),
                          "source_readonly": node.get("readonly"), "source_hidden": node.get("hidden"),
                          "documented_owner_interface": ({
                              "kind": "user_variable_agent",
                              "owner_reference": "https://snap-one.github.io/docs-driverworks-api/#getvariable",
                              "lifecycle_reference": "https://docs.control4.com/help/c4/software/cpro/dealer-composer-help/content/composerpro_userguide/variable_handling.htm",
                              "normal_restart_retention": "documented",
                              "abnormal_shutdown_retention": "not_guaranteed",
                              "external_setter_api": "C4:SetDeviceVariable",
                              "type_code_mapping_confirmed": False,
                              "runtime_migration_approved": False,
                          } if node.get("deviceid") == "100001" else None),
                          "context_operations": [], "review_gates": ["type_mapping", "lifetime", "initialization", "external_writers"],
                          "saved_value_imported": False, "runtime_range_complete": False,
                          "prior_limited_status": None, "semantic_approved": False}
    programs = []
    manager = child(root, "event_mgr", False)
    for event in manager if manager is not None else []:
        if event.tag != "event":
            raise ValueError("unreviewed event manager child")
        owner = text(event, "deviceid")
        key = f"event:{owner}:{text(event, 'eventid')}"
        if any(p["key"] == key for p in programs):
            raise ValueError("duplicate source event")
        parents = {n: p for p in event.iter() for n in p}
        code_ids = {text(n, "id") for n in event.iter("codeitem")}
        if len(code_ids) != sum(1 for _ in event.iter("codeitem")) or len(code_ids) > 4096:
            raise ValueError("duplicate code IDs or node limit exceeded")
        dependencies = []
        operations = []
        for payload in event.iter():
            if payload.tag not in ("devicecommand", "deviceconditional"):
                continue
            code = parents.get(payload)
            while code is not None and code.tag != "codeitem":
                code = parents.get(code)
            if code is None:
                raise ValueError("typed payload without code owner")
            enabled = True
            ancestor = code
            while ancestor is not None:
                if ancestor.tag == "codeitem":
                    flag = text(ancestor, "enabled")
                    if flag not in ("True", "False"):
                        raise ValueError("invalid source enabled flag")
                    enabled &= flag == "True"
                ancestor = parents.get(ancestor)
            operation = payload.get("name") if payload.get("owneridtype") == "variable" else text(payload, "command" if payload.tag == "devicecommand" else "name", False)
            site = {"code_id": text(code, "id"), "source_type": text(code, "type"), "enabled": enabled,
                    "kind": "action" if payload.tag == "devicecommand" else "condition", "operation": operation,
                    "parameters": parameters(payload)}
            if site["kind"] == "action" and operation == "RAMP_TO_LEVEL":
                args = site["parameters"]
                if len(args) == 2 and {p["name"] for p in args} == {"TIME", "LEVEL"} and all(p["shape"] == "static" and p["declared_type"] == "INTEGER" and not p["unmodeled"] for p in args):
                    duration = next(p["static"] for p in args if p["name"] == "TIME")
                    if duration and duration.isascii() and duration.isdigit() and len(duration) <= 18:
                        site["closed_parameter_subgate"] = {"parameter": "TIME", "api_unit": "milliseconds", "duration_ms": int(duration),
                            "reference": "https://snap-one.github.io/docs-driverworks-api/#sendtodevice",
                            "scope": "command API parameter only; not AdvScene rate or driver execution", "driver_execution_confirmed": False}
            operations.append(site)
            if payload.get("owneridtype") == "variable":
                variable = payload.get("owneriditem")
                param = child(payload, "param", False)
                use = {"event": key, "code_id": site["code_id"], "enabled": enabled, "operation": operation,
                       "operand": (param.text or "").strip() if param is not None and not list(param) else None,
                       "operand_type": param.get("type") if param is not None else None,
                       "implicit_read": site["kind"] == "action" and operation in ("+", "-")}
                if variable in variables:
                    variables[variable]["writes" if site["kind"] == "action" else "reads"].append(use)
                dependencies.append({"kind": "variable", "target": variable, "resolved": variable in variables,
                                     "code_id": site["code_id"], "enabled": enabled})
            elif operation in ("ACTIVATE_SCENE", "DEACTIVATE_SCENE", "IS_SCENE_ACTIVE", "IS_SCENE_INACTIVE"):
                refs = [p for p in site["parameters"] if p["name"] == "SCENE_ID" and p["shape"] == "static"]
                scene_id = refs[0]["static"] if len(refs) == 1 else None
                candidates = by_scene_id.get(scene_id, [])
                dependency = {"kind": "scene", "target": scene_id, "resolved": len(candidates) == 1,
                              "candidates": candidates, "code_id": site["code_id"], "enabled": enabled}
                if operation == "DEACTIVATE_SCENE" and len(candidates) == 1:
                    scene = next(s for s in scenes if s["key"] == candidates[0])
                    dependency["deactivation_target"] = scene["toggle_dependency"]
                    toggle = scene["toggle_dependency"]
                    if toggle["resolved"]:
                        target_scene = next(s for s in scenes if s["key"] == toggle["candidates"][0])
                        signature = lambda s, fields: [(m["source_item"], [[e[field] for field in fields] for e in m["elements"]]) for m in s["members"]]
                        dependency["source_target_comparison"] = {
                            "target_all_levels_zero": bool(target_scene["members"]) and all(m["elements"] for m in target_scene["members"]) and all(e["level"] == "0" for m in target_scene["members"] for e in m["elements"]),
                            "same_member_order": [m["source_item"] for m in scene["members"]] == [m["source_item"] for m in target_scene["members"]],
                            "same_element_order_and_delay": signature(scene, ["ordinal", "delay"]) == signature(target_scene, ["ordinal", "delay"]),
                            "same_rate_sequence": signature(scene, ["rate"]) == signature(target_scene, ["rate"]),
                            "use_complete_target_definition": True,
                            "source_rate_unit_confirmed": False}
                dependencies.append(dependency)
                for scene in scenes:
                    if scene["key"] in candidates:
                        scene["referenced_by"].append({"event": key, "code_id": site["code_id"], "operation": operation, "enabled": enabled})
            else:
                target = text(code, "device")
                dependencies.append({"kind": "device", "target": target, "resolved": target in items,
                                     "physical_binding_verified": False, "code_id": site["code_id"], "enabled": enabled})
        empty = source_empty(event)
        programs.append({"key": key, "trigger_driver": driver(owner), "trigger_source_item_exists": owner in items,
                         "source_empty_noop": empty, "operations": operations, "dependencies": dependencies,
                         "typed_review": None, "prior_limited_status": None, "semantic_approved": empty,
                         "runtime_rule_generated": False,
                         "review_gates": [] if empty else ["trigger_semantics", "branch_semantics", "dependency_semantics", "causal_scope"]})
        event_id = text(event,"eventid")
        if driver(owner)=="blind.c4i" and event_id in ("3","5"):
            programs[-1]["nominal_proxy_event"]={"id":event_id,"name":{"3":"Stopped","5":"Moving"}[event_id],
                "reference":"https://snap-one.github.io/docs-driverworks-proxyprotocol-blind/#blind-events",
                "basis":"published post-2.9 proxy interface; archived runtime applicability unverified",
                "physical_observation_confirmed":False,"migration_trigger_confirmed":False}
        event_ops = sorted({s["operation"] for s in operations if s["operation"] is not None})
        for variable in {d["target"] for d in dependencies if d["kind"] == "variable" and d["resolved"]}:
            variables[variable]["context_operations"] += event_ops
    if typed is not None:
        if typed.get("project_xml_sha256") != source["project_xml_sha256"]:
            raise ValueError("typed audit source fingerprint mismatch")
        audit = typed["review"]
        if audit.get("automatic_execution") is not False or audit.get("syntax_only") is not True:
            raise ValueError("typed audit must be read-only syntax evidence")
        entries = audit["events"]
        if len(entries) != len(programs) or {e["owner"] for e in entries} != {p["key"] for p in programs}:
            raise ValueError("typed event coverage mismatch")
        for program in programs:
            record = next(e for e in entries if e["owner"] == program["key"])
            if record["source_empty_noop"] != program["source_empty_noop"]:
                raise ValueError("source-empty audit mismatch")
            # All blockers are retained, not just the first compile failure.
            program["typed_review"] = record
        proofs = typed.get("direct_led_request_proofs", [])
        if len({p["owner"] for p in proofs}) != len(proofs):
            raise ValueError("duplicate LED request proof")
        for proof in proofs:
            matches = [p for p in programs if p["key"] == proof["owner"]]
            if len(matches) != 1 or proof.get("ordered_ast_request_equivalent") is not True or proof.get("semantic_approved") is not False or proof.get("runtime_rule_enabled") is not False:
                raise ValueError("LED proof must be limited source equivalence")
            program = matches[0]
            if not program["operations"] or any(not s["enabled"] or s["kind"] != "action" or s["operation"] not in ("Turn On", "Turn Off") for s in program["operations"]) or len(proof["source_request_sequence"]) != len(program["operations"]):
                raise ValueError("LED source subset coverage mismatch")
            program["source_request_equivalence"] = proof
            program["review_gates"].remove("branch_semantics")
            program["closed_source_subgate"] = "straight_line_led_request_sequence"
    def prior_map(value, section, key):
        if value is None:
            return {}
        if value.get("source_sha256") != source["input_sha256"]:
            raise ValueError("prior disposition source fingerprint mismatch")
        records = value[section]
        result = {key(r): r for r in records}
        if len(result) != len(records):
            raise ValueError("duplicate prior disposition")
        return result
    scene_prior = prior_map(prior, "scenes", lambda r: r["id"])
    program_prior = prior_map(prior, "programs", lambda r: f"event:{r['device_id']}:{r['event_id']}")
    variable_prior = prior_map(prior_variables, "variables", lambda r: r["id"])
    if set(scene_prior) - set(by_scene_id) or set(program_prior) - {p["key"] for p in programs} or set(variable_prior) - set(variables):
        raise ValueError("prior disposition contains unknown source object")
    if any(len(by_scene_id[key]) != 1 for key in scene_prior):
        raise ValueError("ambiguous prior scene ID needs owner-qualified review")
    for scene in scenes:
        scene["prior_limited_status"] = scene_prior.get(scene["source_scene_id"], {}).get("status")
    for program in programs:
        program["prior_limited_status"] = program_prior.get(program["key"], {}).get("status")
    for variable in variables.values():
        variable["prior_limited_status"] = variable_prior.get(variable["source_id"], {}).get("review_status")
        variable["context_operations"] = sorted(set(variable["context_operations"]))
        variable["integer_semantics_required"] = any(w["operation"] in ("+", "-") or w["operation"] == "=" and w["operand"] not in ("0", "1") for w in variable["writes"])
    for calendar in calendars:
        calendar["source_program_present"] = any(p["key"] == calendar["source_event"] for p in programs)
        calendar["source_program_empty"] = any(p["key"] == calendar["source_event"] and p["source_empty_noop"] for p in programs)
    rates = collections.defaultdict(collections.Counter)
    protocol_rates = collections.defaultdict(collections.Counter)
    delays = collections.Counter()
    ignores = collections.Counter()
    for scene in scenes:
        for member in scene["members"]:
            category = {"dimmer.c4i": "dimmer_descriptor", "light.c4i": "light_descriptor", "light_v2.c4i":"light_v2_descriptor"}.get(member["source_driver"], "other_descriptor")
            known={"GJ-TECH.CN_Switch_V2.c4z":"switch_package","GJ-TECH.CN_Dimmer_V2.c4z":"dimmer_package"}
            family=next((known[n] for n in member["source_driver_chain"] if n in known),"other_package")
            ignores[member["tracking"].get("ignore_ramp", "missing")] += 1
            for element in member["elements"]:
                rates[category][element["rate"]] += 1
                protocol_rates[family][element["rate"]] += 1
                delays[element["delay"]] += 1
    filenames={name for s in scenes for m in s["members"] for name in m["source_driver_chain"] if name in ("light_v2.c4i","GJ-TECH.CN_Switch_V2.c4z","GJ-TECH.CN_Dimmer_V2.c4z")}
    descriptors=descriptor_metadata(data,filenames)
    summary = {"scenes": len(scenes), "programs": len(programs), "variables": len(variables), "calendar_entries": len(calendars),
               "embedded_agent_schedules": len(agent_schedules),
               "source_empty_noops": sum(p["source_empty_noop"] for p in programs),
               "explicit_variable_reads": sum(len(v["reads"]) for v in variables.values()),
               "explicit_variable_writes": sum(len(v["writes"]) for v in variables.values()),
               "arithmetic_variables": sum(any(w["operation"] in ("+", "-") for w in v["writes"]) for v in variables.values()),
               "variables_with_documented_user_agent_owner": sum(v["documented_owner_interface"] is not None for v in variables.values()),
               "closed_source_led_request_sequences": sum("closed_source_subgate" in p for p in programs),
               "led_source_sequences_with_nominal_blind_event_identity": sum("closed_source_subgate" in p and "nominal_proxy_event" in p for p in programs),
               "closed_ramp_time_api_units": sum("closed_parameter_subgate" in s for p in programs for s in p["operations"]),
               "scene_members": sum(len(s["members"]) for s in scenes),
               "scene_elements": sum(len(m["elements"]) for s in scenes for m in s["members"]),
               "scene_delay_literals": dict(sorted(delays.items())),
               "scene_rate_literals_by_descriptor": {kind: dict(sorted(values.items())) for kind, values in sorted(rates.items())},
               "scene_rate_literals_by_protocol_source": {kind: dict(sorted(values.items())) for kind, values in sorted(protocol_rates.items())},
               "member_ignore_ramp_literals": dict(sorted(ignores.items())),
               "scenes_with_nonzero_source_rate": sum(any(int(e["rate"]) != 0 for m in s["members"] for e in m["elements"]) for s in scenes),
               "resolved_source_toggle_destinations": sum(s["toggle_dependency"]["resolved"] for s in scenes),
               "unresolved_source_toggle_destinations": sum(not s["toggle_dependency"]["resolved"] for s in scenes),
               "resolved_deactivate_calls": sum(d.get("deactivation_target", {}).get("resolved", False) for p in programs for d in p["dependencies"]),
               "unresolved_deactivate_calls": sum("deactivation_target" in d and not d["deactivation_target"]["resolved"] for p in programs for d in p["dependencies"]),
               "deactivate_target_all_zero_calls": sum(d.get("source_target_comparison", {}).get("target_all_levels_zero", False) for p in programs for d in p["dependencies"]),
               "deactivate_same_member_order_calls": sum(d.get("source_target_comparison", {}).get("same_member_order", False) for p in programs for d in p["dependencies"]),
               "deactivate_differing_source_target_rate_calls": sum("source_target_comparison" in d and not d["source_target_comparison"]["same_rate_sequence"] for p in programs for d in p["dependencies"]),
               "plaintext_protocol_full_als_declarations": sum(any(c["name"]=="advanced_scene_support" and c["source_value"]=="True" for c in d.get("declared_capabilities",[])) for d in descriptors if d["filename"].endswith(".c4z")),
               "unresolved_dependencies": sum(not d["resolved"] for p in programs for d in p["dependencies"]),
               "prior_limited_evidence_is_full_semantic_approval": False,
               "automatic_execution": False}
    return {"format": "homesense-private-semantic-review-v1", "source": source, "summary": summary,
            "scenes": scenes, "programs": programs, "variables": list(variables.values()), "calendars": calendars, "embedded_agent_schedules": agent_schedules,
            "selected_plaintext_descriptors": descriptors,"runtime_definitions": [], "stage_d_complete": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source")
    parser.add_argument("--private-dir", required=True, help="new private output directory; never overwritten")
    parser.add_argument("--typed-review", help="source-hashed envelope around Rust source_review output")
    parser.add_argument("--prior-dispositions")
    parser.add_argument("--prior-variables")
    args = parser.parse_args()
    def bounded(path, limit):
        with open(path, "rb") as stream:
            data = stream.read(limit + 1)
        if len(data) > limit:
            raise ValueError("input budget exceeded")
        return data
    load = lambda path: json.loads(bounded(path, 32 * 1024 * 1024)) if path else None
    result = review(bounded(args.source, 64 * 1024 * 1024), load(args.typed_review), load(args.prior_dispositions), load(args.prior_variables))
    directory = Path(args.private_dir)
    directory.mkdir(mode=0o700)
    for name, body in (("semantic-review.json", result), ("summary.json", result["summary"])):
        with os.fdopen(os.open(directory / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
            json.dump(body, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
    print(json.dumps(result["summary"], sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, ET.ParseError, zipfile.BadZipFile) as error:
        raise SystemExit(f"Source review failed: {error}") from None

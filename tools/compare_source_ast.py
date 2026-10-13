#!/usr/bin/env python3
"""Compare a strict source XML action tree with supplied production AST observations.

This is an independent read-only model, not a configuration generator. Actual
source identifiers belong in private output only. A match proves translation
structure, not trigger, driver, timing, ownership, or physical execution.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import xml.etree.ElementTree as ET


class Unreviewed(ValueError):
    pass


def container(node, tags, attributes=()):
    if node is None or set(node.attrib) - set(attributes):
        raise Unreviewed("missing or attributed container")
    if (node.text or "").strip() or any((n.tail or "").strip() for n in node):
        raise Unreviewed("mixed executable text")
    if any(n.tag not in tags for n in node):
        raise Unreviewed("unmodeled child")


def one(node, tag):
    values = node.findall(tag)
    if len(values) != 1:
        raise Unreviewed("missing or duplicate " + tag)
    return values[0]


def scalar(node, attributes=()):
    if node is None or list(node) or set(node.attrib) - set(attributes):
        raise Unreviewed("unmodeled scalar")
    return (node.text or "").strip()


def field(node, tag):
    return scalar(one(node, tag))


def integer(value):
    if not value or not value.isascii() or not value.lstrip("-").isdigit():
        raise Unreviewed("nonliteral integer")
    result = int(value)
    if not -(2**63) <= result < 2**63:
        raise Unreviewed("integer range")
    return result


def operand(payload):
    p = one(payload, "param")
    if p.attrib != {"name": "value", "type": "int"}:
        raise Unreviewed("variable operand declaration")
    return ["integer", integer(scalar(p, ("name", "type")))]


def parameter(payload, name, accepted=("INTEGER",)):
    params = one(payload, "params")
    container(params, {"param"})
    p = one(params, "param")
    container(p, {"name", "value"})
    if field(p, "name") != name:
        raise Unreviewed("parameter identity")
    value = one(p, "value")
    container(value, {"static"}, ("type",))
    if set(value.attrib) != {"type"} or value.get("type") not in accepted:
        raise Unreviewed("parameter type")
    return scalar(one(value, "static"))


def binding(code, bindings):
    key = field(code, "device")
    if key not in bindings:
        raise Unreviewed("unbound source target")
    return bindings[key]


def code_shape(code):
    container(code, {"id", "device", "type", "enabled", "display", "creator",
                     "creatorstate", "cmdcond", "subitems", "expression"})
    tags = [n.tag for n in code]
    if len(tags) != len(set(tags)):
        raise Unreviewed("duplicate code field")
    for n in code:
        if n.tag not in {"cmdcond", "subitems", "expression"}:
            scalar(n)
    if field(code, "enabled") not in {"True", "False"}:
        raise Unreviewed("enabled declaration")
    container(one(code, "subitems"), {"codeitem"})
    return field(code, "type")


def condition(code, bindings, depth):
    if depth > 64:
        raise Unreviewed("condition depth")
    code_shape(code)
    payloads = one(code, "cmdcond")
    container(payloads, {"deviceconditional"})
    p = one(payloads, "deviceconditional")
    if p.get("owneridtype") == "variable":
        container(p, {"param"}, ("owneridtype", "owneriditem", "name"))
        variable = ["variable", p.get("owneriditem")]
        if not variable[1]:
            raise Unreviewed("variable identity")
        rhs = operand(p)
        operation = p.get("name")
        if operation == "==":
            value = ["equal", variable, rhs]
        elif operation == "!=":
            value = ["not", ["equal", variable, rhs]]
        elif operation == "<":
            value = ["less", variable, rhs]
        else:
            raise Unreviewed("variable condition")
    else:
        container(p, {"name", "conditional", "params"}, ("owneridtype", "owneriditem"))
        b = binding(code, bindings)
        name = field(p, "name")
        if name in {"LOAD_ON", "LOAD_OFF"} and b["kind"] in {"switch", "dimmer"} and len(p) == 1:
            value = ["equal", ["on", b["device"]], ["bool", name == "LOAD_ON"]]
        elif name == "IS_LEVEL" and b["kind"] == "dimmer" and len(p) == 3 and field(p, "conditional") == "GREATER_THAN":
            threshold = integer(parameter(p, "SETTING"))
            if not 0 <= threshold <= 100:
                raise Unreviewed("level threshold")
            value = ["less", ["integer", threshold], ["level", b["device"]]]
        else:
            raise Unreviewed("device condition " + name)
    if code.find("expression") is not None:
        ex = one(code, "expression")
        container(ex, {"codeitem"})
        if len(ex) != 2:
            raise Unreviewed("compound arity")
        op, right = ex
        if code_shape(op) != "6" or code_shape(right) != "2":
            raise Unreviewed("compound operand kinds")
        if any(field(v, "enabled") != "True" or len(one(v, "subitems")) for v in ex):
            raise Unreviewed("disabled or executable expression operand")
        container(one(op, "cmdcond"), set())
        if op.find("expression") is not None:
            raise Unreviewed("compound operator has expression")
        connective = {"AND": "all", "OR": "any"}.get(field(op, "display"))
        if connective is None:
            raise Unreviewed("unknown connective")
        value = [connective, [value, condition(right, bindings, depth + 1)]]
    return value


def action(code, bindings):
    payload = one(code, "cmdcond")
    container(payload, {"devicecommand"})
    p = one(payload, "devicecommand")
    if p.get("owneridtype") == "variable":
        container(p, {"param"}, ("owneridtype", "owneriditem", "name"))
        key = p.get("owneriditem")
        if not key:
            raise Unreviewed("variable identity")
        value = operand(p)
        operation = p.get("name")
        if operation in {"+", "-"}:
            value = [{"+": "add", "-": "subtract"}[operation], ["variable", key], value]
        elif operation != "=":
            raise Unreviewed("variable action")
        return ["set", key, value]
    container(p, {"command", "params"}, ("owneridtype", "owneriditem"))
    command = field(p, "command")
    if command in {"ACTIVATE_SCENE", "DEACTIVATE_SCENE"}:
        key = parameter(p, "SCENE_ID")
        if p.get("owneridtype") != "agent" or p.get("owneriditem") != key:
            raise Unreviewed("scene reference identity")
        return ["scene", key, command == "ACTIVATE_SCENE"]
    b = binding(code, bindings)
    target = b["device"]
    if command in {"Turn On", "Turn Off"} and b["kind"] == "keypad":
        key = integer(parameter(p, "LED Number", ("INTEGER", "STRING")))
        if not 1 <= key <= 6:
            raise Unreviewed("LED selector")
        return ["command", f"{target}:key:{key}", "led", command == "Turn On"]
    if command == "SET_LEVEL" and b["kind"] == "dimmer":
        level = integer(parameter(p, "LEVEL"))
        if not 0 <= level <= 100:
            raise Unreviewed("level range")
        return ["command", target, "level", level, "Immediate"]
    container(one(p, "params"), set())
    if command == "OFF" and b["kind"] == "dimmer":
        return ["command", target, "level", 0, "Immediate"]
    if command in {"ON", "OFF"} and b["kind"] == "switch":
        return ["command", target, "switch", command == "ON"]
    if command == "TOGGLE" and b["kind"] == "switch":
        return ["if", ["equal", ["on", target], ["bool", True]],
                [["command", target, "switch", False]], [["command", target, "switch", True]]]
    raise Unreviewed("device action " + command)


def sequence(parent, bindings, depth=0):
    if depth > 64:
        raise Unreviewed("action depth")
    container(parent, {"codeitem"})
    out = []
    may_else = False
    for code in parent:
        kind = code_shape(code)
        if field(code, "enabled") == "False":
            # A disabled conditional is still an Else pairing boundary. An active
            # Else may not accidentally attach to an earlier active conditional.
            if kind != "5":
                may_else = False
            continue
        sub = one(code, "subitems")
        if kind == "5":
            container(one(code, "cmdcond"), set())
            if len(sub) or code.find("expression") is not None:
                raise Unreviewed("comment with executable children")
            continue
        if kind == "4":
            container(one(code, "cmdcond"), set())
            if not may_else or not out or out[-1][0] != "if" or out[-1][3]:
                raise Unreviewed("orphan or duplicate Else")
            out[-1][3] = sequence(sub, bindings, depth + 1)
            may_else = False
            continue
        may_else = kind == "2"
        if kind == "2":
            out.append(["if", condition(code, bindings, 0), sequence(sub, bindings, depth + 1), []])
        elif kind == "1" and field(code, "id") == "0" and field(code, "device") == "0":
            container(one(code, "cmdcond"), set())
            if code.find("expression") is not None:
                raise Unreviewed("sequence root with expression")
            out.extend(sequence(sub, bindings, depth + 1))
        elif kind == "1":
            if len(sub) or code.find("expression") is not None:
                raise Unreviewed("action with executable children")
            out.append(action(code, bindings))
        else:
            raise Unreviewed("unmodeled node kind")
    return out


def compare(xml, bindings, product):
    if len(xml) > 32 * 1024 * 1024 or b"<!DOCTYPE" in xml.upper():
        raise Unreviewed("source size or DTD")
    root = ET.fromstring(xml)
    if product.get("source_xml_sha256") != hashlib.sha256(xml).hexdigest():
        raise Unreviewed("production source fingerprint mismatch")
    manager = one(root, "event_mgr")
    container(manager, {"event"})
    observed = product["programs"]
    by_key = {v["key"]: v for v in observed}
    if len(by_key) != len(observed):
        raise Unreviewed("duplicate product event")
    if any(type(v.get("compiled")) is not bool or v["compiled"] and not isinstance(v.get("ast"), list) for v in observed):
        raise Unreviewed("invalid product observation")
    records = []
    for event in manager:
        container(event, {"deviceid", "eventid", "codeitem"})
        key = f"event:{field(event, 'deviceid')}:{field(event, 'eventid')}"
        if key not in by_key:
            raise Unreviewed("product source coverage")
        body = ET.Element("subitems")
        body.extend(event.findall("codeitem"))
        actual = by_key[key]
        try:
            expected = sequence(body, bindings)
        except Unreviewed as error:
            records.append({"key": key, "status": "independent_source_unreviewed", "reason": str(error),
                            "product_compiled": actual["compiled"], "product": actual})
            continue
        equal = actual["compiled"] and expected == actual["ast"]
        records.append({"key": key, "status": "ordered_ast_equivalent" if equal else "mismatch",
                        "expected": expected, "product": actual})
    if len(records) != len(by_key) or len({v['key'] for v in records}) != len(records):
        raise Unreviewed("event coverage mismatch")
    summary = {"programs": len(records), "ordered_ast_equivalent": sum(v["status"] == "ordered_ast_equivalent" for v in records),
               "mismatch": sum(v["status"] == "mismatch" for v in records),
               "independent_source_unreviewed": sum(v["status"] == "independent_source_unreviewed" for v in records),
               "product_compiled_in_unreviewed_subset": sum(v.get("product_compiled", False) for v in records),
               "automatic_execution": False, "full_semantic_approval": False}
    return {"summary": summary, "programs": records,
            "source_xml_sha256": hashlib.sha256(xml).hexdigest(), "runtime_definitions": []}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("source_xml", type=Path)
    p.add_argument("bindings", type=Path)
    p.add_argument("production_ast", type=Path)
    p.add_argument("--private-output", required=True, type=Path)
    args = p.parse_args()
    def bounded(path):
        with path.open("rb") as stream:
            raw = stream.read(32 * 1024 * 1024 + 1)
        if len(raw) > 32 * 1024 * 1024:
            raise Unreviewed("input exceeds 32MiB")
        return raw
    raw_bindings = bounded(args.bindings)
    product = json.loads(bounded(args.production_ast))
    if product.get("bindings_sha256") != hashlib.sha256(raw_bindings).hexdigest():
        raise Unreviewed("production binding fingerprint mismatch")
    result = compare(bounded(args.source_xml), json.loads(raw_bindings), product)
    with os.fdopen(os.open(args.private_output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
        json.dump(result, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    print(json.dumps(result["summary"], sort_keys=True))
    if result["summary"]["mismatch"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

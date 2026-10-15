#!/usr/bin/env python3
"""Offline, explicit metadata-driven schema-2 Keypad candidate. Never publishes."""
import argparse
import copy
import json
import os
from pathlib import Path


def candidate(document, reviews):
    result = copy.deepcopy(document)
    if result.get("schema_version") != 2 or result.get("keypads"):
        raise ValueError("requires unmigrated schema 2 input")
    modules = {m["id"]: m for g in result["gateways"] for m in g["modules"]}
    panels, moved, seen_modules, seen_ids = [], set(), set(), set()
    all_ids = {d["id"] for d in result["devices"]}
    for review in reviews:
        if set(review) - {"id", "name", "module_id", "room", "review_evidence"}:
            raise ValueError("unknown review field; key actions are not inferred")
        module_id = review["module_id"]
        if not review.get("review_evidence", "").strip():
            raise ValueError("explicit review evidence required")
        if module_id in seen_modules or review["id"] in all_ids | seen_ids:
            raise ValueError("duplicate module or panel identity")
        if modules.get(module_id, {}).get("profile") != "ts6_86_rs485_v2_6_5_6key":
            raise ValueError("requires known six-key TS6 module")
        children = [d for d in result["devices"] if d.get("module_id") == module_id]
        if not children or len(children) > 6 or len({d["channel"] for d in children}) != len(children):
            raise ValueError("invalid child coverage")
        keys = []
        for child in sorted(children, key=lambda d: d["channel"]):
            if set(child) - {"id", "name", "room", "module_id", "channel", "enabled"}:
                raise ValueError("unsupported legacy fields; manual review required")
            if child.get("room") != review.get("room") or child["channel"] not in range(1, 7):
                raise ValueError("migration cannot change room or assume key layout")
            keys.append({"number": child["channel"], "label": str(child["channel"]),
                         "led_endpoint": {"id": child["id"], "name": child["name"],
                                          "enabled": child.get("enabled", False)}})
            moved.add(child["id"])
        panel = {k: review[k] for k in ("id", "name", "module_id", "room") if k in review}
        panel.update(enabled=any(d.get("enabled", False) for d in children), keys=keys)
        panels.append(panel)
        seen_modules.add(module_id)
        seen_ids.add(review["id"])
    if not panels:
        raise ValueError("no explicitly reviewed panels")
    result["devices"] = [d for d in result["devices"] if d["id"] not in moved]
    result["keypads"] = panels
    # Revision stays unchanged: this is an offline representation candidate,
    # never a publishable server draft or automatic CAS operation.
    return result


def strict_load(path):
    def pairs(values):
        result = {}
        for key, value in values:
            if key in result:
                raise ValueError("duplicate JSON field")
            result[key] = value
        return result
    if path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError("input exceeds configuration bound")
    return json.loads(path.read_text(), object_pairs_hook=pairs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("configuration", type=Path)
    parser.add_argument("review", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    result = candidate(strict_load(args.configuration), strict_load(args.review))
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump(result, stream, ensure_ascii=False, indent=2)
        stream.write("\n")


if __name__ == "__main__":
    main()

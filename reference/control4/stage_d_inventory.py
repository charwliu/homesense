"""Offline Stage D disposition inventory. Does not enable any imported rule."""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
source = json.loads((ROOT / 'reference/control4/Charlies250801.parsed.json').read_text())
scenes = []
for scene in source['lighting_scenes']:
    reviewed = scene['scene_id'] in {'6', '8', '10'}
    scenes.append({'id': scene['scene_id'], 'name': scene['name'],
                   'status': 'manual_field_verified' if reviewed else 'needs_review',
                   'automatic_enabled': False,
                   'reason': 'study manual activation and deactivation verified' if reviewed else
                   'timing, physical binding and deactivation semantics require review'})
programs = []
for event in source['programmed_events']:
    reviewed = event['device_id'] == '63' and event['event_id'] in {'2', '6', '10'}
    programs.append({'device_id': event['device_id'], 'event_id': event['event_id'],
                     'name': event['event_name'], 'room': event.get('room'),
                     'status': 'study_manual_runtime' if reviewed else 'disabled_pending',
                     'automatic_enabled': False,
                     'reason': 'reviewed study release branch; no automatic subscription' if reviewed else
                     'AST semantic translation and replay not complete'})
assert len(scenes) == 68 and len(programs) == 215
assert len({s['id'] for s in scenes}) == 68
assert len({(p['device_id'], p['event_id']) for p in programs}) == 215
report = {'source_sha256': source['sha256'], 'stage_d_complete': False,
          'counts': {'scenes': len(scenes), 'programs': len(programs),
                     'manual_verified_scenes': 3, 'manual_runtime_programs': 3},
          'scenes': scenes, 'programs': programs}
output = ROOT / 'docs/evidence/stage-d-study/project-disposition.json'

# Enumerate every original XML node, including type-6 expression containers
# omitted by the historical Python parsed export. Values and licenses are not emitted.
import zipfile
import xml.etree.ElementTree as ET
import collections
import hashlib
project = Path('/Volumes/m4work/aiml/Control4/Projects/Charlies250801.c4p')
assert hashlib.sha256(project.read_bytes()).hexdigest() == source['sha256']
with zipfile.ZipFile(project) as archive:
    root = ET.fromstring(archive.read('project.xml'))
node_total = 0
by_event = {}
for event in root.findall('.//event'):
    key = (event.findtext('deviceid'), event.findtext('eventid'))
    nodes = event.findall('.//codeitem')
    if not nodes:
        continue
    node_total += len(nodes)
    commands = collections.Counter()
    conditions = collections.Counter()
    types = collections.Counter()
    for node in nodes:
        types[node.findtext('type')] += 1
        payload = node.find('cmdcond')
        if payload is None:
            continue
        for command in payload.findall('.//devicecommand'):
            name = command.get('name') or command.findtext('command')
            if name:
                commands[name] += 1
        for condition in payload.findall('.//deviceconditional'):
            name = condition.get('name') or condition.findtext('name')
            if name:
                conditions[name] += 1
    assert key not in by_event
    by_event[key] = {'source_nodes': len(nodes), 'node_types': dict(types),
                     'commands': dict(commands), 'conditions': dict(conditions)}
assert node_total == 3138 and len(by_event) == 215
for program in report['programs']:
    program['source_inventory'] = by_event[(program['device_id'], program['event_id'])]
report['counts']['source_nodes'] = node_total
report['inventory_source'] = 'Original project.xml; all codeitem descendants including expression containers'
output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n')
print('Original nodes accounted for:', node_total)

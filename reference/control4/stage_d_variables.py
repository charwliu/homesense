#!/usr/bin/env python3
"""Read-only variable use evidence; proposals never enable runtime variables."""
import hashlib
import json
from pathlib import Path
import sys
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[2]
source = Path(sys.argv[1]) if len(sys.argv) > 1 else Path('/Volumes/m4work/aiml/Control4/Projects/Charlies250801.c4p')
parsed = json.loads((ROOT / 'reference/control4/Charlies250801.parsed.json').read_text())
digest = hashlib.sha256(source.read_bytes()).hexdigest()
if digest != parsed['sha256']:
    raise SystemExit('source fingerprint mismatch')
with zipfile.ZipFile(source) as archive:
    root = ET.fromstring(archive.read('project.xml'))
parents = {child: parent for parent in root.iter() for child in parent}
variables = {}
for original in parsed['variables']:
    attr = original['attributes']
    key = attr['variableid']
    if key in variables:
        raise SystemExit('duplicate variable')
    variables[key] = {'id': key, 'name': attr['name'], 'source_type': attr['type'],
                      'saved_value_for_reference_only': original['value'],
                      'initialize_from_saved_value': False, 'enabled': False,
                      'reads': [], 'writes': [], 'context_commands': set()}
events = [event for event in root.iter('event') if event.find('deviceid') is not None and event.find('eventid') is not None]
if len(events) != 215 or len(variables) != 25:
    raise SystemExit('project inventory mismatch')
for event in events:
    owner = f"device:{event.findtext('deviceid')}/event:{event.findtext('eventid')}"
    commands = {node.findtext('command') for node in event.iter('devicecommand') if node.findtext('command')}
    for node in event.iter():
        if node.tag not in ('devicecommand', 'deviceconditional') or node.get('owneridtype') != 'variable':
            continue
        key = node.get('owneriditem')
        if key not in variables:
            raise SystemExit(f'undefined variable {key}')
        code = node
        while code.tag != 'codeitem':
            code = parents[code]
        param = node.find('param')
        record = {'source': f"{owner}/code:{code.findtext('id')}", 'operator': node.get('name'),
                  'operand': None if param is None else param.text,
                  'operand_type': None if param is None else param.get('type')}
        variables[key]['writes' if node.tag == 'devicecommand' else 'reads'].append(record)
        variables[key]['context_commands'].update(commands)
for key, variable in variables.items():
    commands = variable['context_commands']
    scene = bool(commands & {'ACTIVATE_SCENE', 'DEACTIVATE_SCENE'})
    motor = bool(commands & {'SET_LEVEL_TARGET', 'STOP'})
    # This is evidence from containing event programs, not a semantic proof.
    lighting = bool(commands & {'ON', 'OFF', 'SET_LEVEL', 'TOGGLE'})
    variable['proposed_lifetime'] = 'temporary' if motor and not (scene or lighting) else 'persistent' if (scene or lighting) and not motor else 'needs_review'
    variable['encoding'] = 'arithmetic_components' if any(w['operator'] in ('+', '-') for w in variable['writes']) else 'enumerated_integer'
    variable['assignment_values'] = sorted({int(w['operand']) for w in variable['writes'] if w['operator'] == '='})
    variable['arithmetic_operands'] = sorted({int(w['operand']) for w in variable['writes'] if w['operator'] in ('+', '-')})
    variable['feedback_cache_confirmed'] = False
    variable['basis'] = 'scene/light actions imply logical mode candidate; motor-only actions imply transient bookkeeping candidate; no physical feedback classification inferred'
    variable['review_status'] = 'study_logic_field_validated' if key == '411' else 'needs_review'
    variable['context_commands'] = sorted(commands)
    variable['read_count'] = len(variable['reads'])
    variable['write_count'] = len(variable['writes'])
output = ROOT / 'docs/evidence/stage-d-study/variable-dispositions.json'
output.write_text(json.dumps({'source_sha256': digest, 'variables': list(variables.values()),
    'policy': 'Only logical modes may persist; motion bookkeeping needs review and must not be treated as physical feedback. Source saved values are not runtime initialization.'}, ensure_ascii=False, indent=2) + '\n')
print(f'{len(variables)} variables; {sum(v["read_count"] for v in variables.values())} reads; {sum(v["write_count"] for v in variables.values())} writes')

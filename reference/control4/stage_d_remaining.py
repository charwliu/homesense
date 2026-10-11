#!/usr/bin/env python3
"""Extract remaining source semantics without enabling or rewriting programs."""
import hashlib
import json
from pathlib import Path
import sys
import xml.etree.ElementTree as ET
import zipfile

ROOT = Path(__file__).resolve().parents[2]
source = Path(sys.argv[1]) if len(sys.argv) > 1 else Path('/Volumes/m4work/aiml/Control4/Projects/Charlies250801.c4p')
report = json.loads((ROOT / 'docs/evidence/stage-d-study/semantic-dispositions.json').read_text())
if hashlib.sha256(source.read_bytes()).hexdigest() != report['source']['sha256']:
    raise SystemExit('source fingerprint mismatch')
with zipfile.ZipFile(source) as archive:
    xml = archive.read('project.xml')
if hashlib.sha256(xml).hexdigest() != report['source']['project_xml_sha256']:
    raise SystemExit('project XML fingerprint mismatch')
root = ET.fromstring(xml)
parents = {child: parent for parent in root.iter() for child in parent}
blocked = {p['owner']: p['blocker'] for p in report['programs'] if p['blocker']}
programs = []
for event in root.iter('event'):
    owner = f"event:{event.findtext('deviceid')}:{event.findtext('eventid')}"
    if owner not in blocked:
        continue
    evidence = []
    for node in event.iter():
        command = node.findtext('command')
        condition = node.findtext('name') if node.tag == 'deviceconditional' else None
        if command != 'RAMP_TO_LEVEL' and condition not in ('IF_MOVING', 'IF_STOPPED'):
            continue
        ancestor = node
        while ancestor.tag != 'codeitem':
            ancestor = parents[ancestor]
        evidence.append({'code_id': ancestor.findtext('id'), 'operation': command or condition,
                         'source_xml': ET.tostring(node, encoding='unicode').strip()})
    if not evidence:
        raise SystemExit(f'blocked program has no extracted evidence: {owner}')
    programs.append({'owner': owner, 'first_compiler_blocker': blocked[owner],
                     'enabled': False, 'evidence': evidence})
if len(programs) != len(blocked):
    raise SystemExit('blocked program coverage mismatch')
output = {'source': report['source'], 'automatic_execution': False,
          'policy': 'Motor predicates require verified physical motion evidence; target commands do not establish motion state. Timed ramps require exact duration semantics, not a boolean ramp flag.',
          'programs': programs}
(ROOT / 'docs/evidence/stage-d-study/remaining-semantics.json').write_text(json.dumps(output, ensure_ascii=False, indent=2) + '\n')
print(f'{len(programs)} blocked programs; {sum(len(p["evidence"]) for p in programs)} semantic sites')

import collections
import hashlib
import io
import json
import re
import sqlite3
import tempfile
import zipfile
from pathlib import Path
from xml.etree import ElementTree as ET

SOURCE = Path('/Volumes/m4work/aiml/Control4/Projects/Charlies250801.c4p')
OUT = Path(__file__).parent
SENSITIVE = re.compile(r'activation|password|passwd|secret|token|license|licensed|credential|api.?key', re.I)


def xml_data(node):
    return {'tag': node.tag, 'attributes': dict(node.attrib),
            'text': (node.text or '').strip(),
            'children': [xml_data(c) for c in node]}


with zipfile.ZipFile(SOURCE) as archive:
    root = ET.fromstring(archive.read('project.xml'))
    driver_metadata = {}
    for name in archive.namelist():
        if not name.startswith('drivers/') or name.endswith('/'):
            continue
        data = archive.read(name)
        encrypted = False
        if name.endswith('.c4z'):
            with zipfile.ZipFile(io.BytesIO(data)) as driver:
                encrypted = any(n.endswith('.encrypted') for n in driver.namelist())
                data = driver.read('driver.xml')
        doc = ET.fromstring(data)
        driver_metadata[Path(name).name] = {
            'name': doc.findtext('name'), 'manufacturer': doc.findtext('manufacturer'),
            'model': doc.findtext('model'), 'version': doc.findtext('version'),
            'lua_encrypted': encrypted,
            'events': {v.findtext('id'): v.findtext('name') for v in doc.findall('.//events/event')},
        }

    nodes = root.findall('.//systemitems//item')
    node_index = {n.findtext('id'): n for n in nodes}
    items = []

    def walk(node, parents=(), floor=None, room=None):
        name, kind, ident = node.findtext('name'), node.findtext('type'), node.findtext('id')
        path = parents + (name,)
        if kind == '4': floor = name
        if kind == '8': room = name
        driver = node.findtext('itemdata/config_data_file')
        properties = {}
        raw_state = node.findtext('state')
        if raw_state:
            try:
                state = ET.fromstring(raw_state)
                for prop in state.findall('.//properties/property'):
                    key = prop.findtext('name') or ''
                    properties[key] = '[REDACTED]' if SENSITIVE.search(key) else prop.findtext('value')
            except ET.ParseError:
                pass
        items.append({'id': ident, 'name': name, 'type_code': kind,
                      'path': list(path), 'floor': floor, 'room': room,
                      'driver': driver, 'properties': properties})
        for child in node.findall('subitems/item'):
            walk(child, path, floor, room)

    for node in root.findall('systemitems/item'): walk(node)
    assert len(items) == len(nodes) == len({n['id'] for n in items})
    index = {n['id']: n for n in items}

    scenes = []
    schedules = []
    for node in nodes:
        raw = node.findtext('state')
        if raw:
            try:
                state = ET.fromstring(raw)
                for scene in state.findall('.//AdvScene'):
                    members = []
                    for member in scene.findall('all_members/AdvSceneMember'):
                        target = member.findtext('device_id')
                        members.append({'device_id': target, 'device_name': index.get(target, {}).get('name'),
                                        'track_level': member.findtext('track_lvl'),
                                        'elements': [{c.tag: c.text for c in element}
                                                     for element in member.findall('all_elements/element')]})
                    scenes.append({'name': scene.findtext('name'), 'scene_id': scene.findtext('scene_id'),
                                   'members': members})
            except ET.ParseError:
                pass
        for value in node.findall('state_values/value'):
            if value.get('name') == 'entry_data' and value.text:
                for entry in ET.fromstring(value.text):
                    schedules.append({'device_id': node.findtext('id'), 'event_id': entry.findtext('eventid'),
                                      'description': entry.findtext('description'),
                                      'enabled': entry.findtext('enabled'), 'data': xml_data(entry)})
    schedule_names = {(s['device_id'], s['event_id']): s['description'] for s in schedules}

    def parse_code(node):
        target = node.findtext('device')
        name = index.get(target, {}).get('name', {'100001': '项目变量', '100000': '编程注释'}.get(target, target))
        display = node.findtext('display') or ''
        return {'id': node.findtext('id'), 'type_code': node.findtext('type'),
                'device_id': target, 'device_name': name,
                'display': display, 'resolved_display': display.replace('NAME', name or ''),
                'enabled': node.findtext('enabled') == 'True',
                'command_or_condition': xml_data(node.find('cmdcond')) if node.find('cmdcond') is not None else None,
                'children': [parse_code(c) for c in node.findall('subitems/codeitem')]}

    events = []
    for event in root.findall('event_mgr/event'):
        device_id, event_id = event.findtext('deviceid'), event.findtext('eventid')
        item = index.get(device_id, {})
        label = driver_metadata.get(item.get('driver'), {}).get('events', {}).get(event_id)
        label = schedule_names.get((device_id, event_id), label)
        events.append({'device_id': device_id, 'device_name': item.get('name'),
                       'room': item.get('room'), 'event_id': event_id,
                       'event_name': label or f'事件 ID {event_id}',
                       'program': [parse_code(n) for n in event.findall('codeitem')]})

    connections = []
    for binding in root.findall('bindings/boundbinding'):
        provider = binding.findtext('deviceid')
        for consumer in binding.findall('boundconsumers/bound'):
            target = consumer.findtext('deviceid')
            connections.append({'provider_id': provider, 'provider_name': index.get(provider, {}).get('name'),
                                'provider_binding_id': binding.findtext('bindingid'),
                                'consumer_id': target, 'consumer_name': index.get(target, {}).get('name'),
                                'consumer_binding_id': consumer.findtext('bindingid'),
                                'name': consumer.findtext('name'),
                                'classes': [n.text for n in consumer.findall('boundclasses/boundclass')]})
    networks = []
    for binding in root.findall('networkbindings/networkbinding'):
        data = {n.tag: n.text for n in binding}
        data['device_name'] = index.get(data.get('deviceid'), {}).get('name')
        networks.append(data)

    databases = {}
    for filename in ['mm.db', 'identity.db']:
        with tempfile.TemporaryDirectory() as directory:
            dbpath = Path(directory) / filename
            dbpath.write_bytes(archive.read(filename))
            with sqlite3.connect(f'file:{dbpath}?mode=ro', uri=True) as db:
                names = [v[0] for v in db.execute("SELECT name FROM sqlite_master WHERE type='table'")]
                databases[filename] = {name: db.execute('SELECT COUNT(*) FROM "' + name.replace('"', '""') + '"').fetchone()[0]
                                       for name in names}

    summary = {'project_name': nodes[0].findtext('name'),
               'project_version': dict(nodes[0].attrib),
               'timezone': nodes[0].findtext('itemdata/timezone'),
               'archive_snapshot': '2025-07-28 12:53:34',
               'items': len(items), 'floors': sum(n['type_code']=='4' for n in items),
               'rooms': sum(n['type_code']=='8' for n in items),
               'lights': sum(n['driver']=='light_v2.c4i' for n in items),
               'switch_drivers': sum(n['driver']=='GJ-TECH.CN_Switch_V2.c4z' for n in items),
               'dimmer_drivers': sum(n['driver']=='GJ-TECH.CN_Dimmer_V2.c4z' for n in items),
               'blinds': sum(n['driver']=='blind.c4i' for n in items),
               'keypads': sum(n['driver']=='GJ-TECH.CN_Keypad.c4z' for n in items),
               'lighting_scenes': len(scenes), 'programmed_events': len(events),
               'commands': len(root.findall('.//event_mgr//devicecommand')),
               'conditions': len(root.findall('.//event_mgr//deviceconditional')),
               'connections': len(connections), 'bundled_drivers': len(driver_metadata)}
    result = {'source': str(SOURCE), 'sha256': hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
              'summary': summary, 'items': items, 'driver_metadata': driver_metadata,
              'lighting_scenes': scenes, 'programmed_events': events,
              'connections': connections, 'network_bindings': networks,
              'variables': [{'attributes': dict(n.attrib), 'value': n.text} for n in root.findall('variables/variable')],
              'schedules': schedules, 'database_row_counts': databases,
              'notes': ['所有在线状态和变量值均为备份时快照，不代表当前状态。',
                        '驱动授权密钥已脱敏；未导出 identity.db 的个人记录。',
                        '未解密驱动 Lua；房间、代理对象和驱动对象分别计数，不能将所有 item 当作物理设备。']}
    (OUT/'Charlies250801.parsed.json').write_text(json.dumps(result, ensure_ascii=False, indent=2))

lines = ["Control4 项目解析：Charlie's", '来源：'+str(SOURCE), '备份快照：2025-07-28 12:53:34；Composer/Director 3.2.3',
         '注意：下列配置和状态来自备份，未连接当前 Control4 主机。', '', '概况：'+json.dumps(summary, ensure_ascii=False),
         '', '房间与设备']
for floor in [n['name'] for n in items if n['type_code']=='4']:
    lines.append('\n'+floor)
    for room in [n for n in items if n['type_code']=='8' and n['floor']==floor]:
        lines.append('  '+room['name']+' [ID '+room['id']+']')
        for item in items:
            if item['room']==room['name'] and item['floor']==floor and item['type_code'] in ['6','7']:
                lines.append('    '+item['id']+' '+item['name']+' ('+(item['driver'] or '')+')')
lines.extend(['', '灯光场景'])
for scene in scenes:
    lines.append(scene['name']+' [ID '+str(scene['scene_id'])+']')
    for member in scene['members']:
        elements=', '.join('level='+str(v.get('level'))+' delay='+str(v.get('delay'))+' rate='+str(v.get('rate')) for v in member['elements'])
        lines.append('  '+str(member['device_name'])+' [设备 '+str(member['device_id'])+'] '+elements)
lines.extend(['', '事件编程'])

def render(program, indent='  '):
    for instruction in program:
        if instruction['resolved_display']:
            lines.append(indent+instruction['resolved_display']+' [设备 '+str(instruction['device_id'])+']')
        render(instruction['children'], indent+'  ')

for event in events:
    lines.append(str(event['device_name'])+' [设备 '+str(event['device_id'])+'] '+event['event_name']+' [事件 '+str(event['event_id'])+']')
    render(event['program'])
lines.extend(['', '网络绑定（备份时）'])
for n in networks: lines.append(str(n.get('device_name'))+'：'+str(n.get('addr'))+'；状态 '+str(n.get('status')))
lines.extend(['', '调度任务'])
for s in schedules: lines.append(str(s['description'])+'；启用 '+str(s['enabled']))
(OUT/'Charlies250801.report.txt').write_text('\n'.join(lines)+'\n')
print(json.dumps(summary, ensure_ascii=False))
print('Floors:', [n['name'] for n in items if n['type_code']=='4'])
print('Output:', OUT/'Charlies250801.report.txt', OUT/'Charlies250801.parsed.json')

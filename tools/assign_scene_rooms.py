#!/usr/bin/env python3
"""Build an offline, evidence-backed scene placement inventory and candidate.

No connections, publication, commands or credential reads. Input documents are
immutable. Generated household data belongs in a private output directory.
"""
import argparse
import collections
import copy
import hashlib
import json
import re
from pathlib import Path


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def walk(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk(child)


def index(rows):
    result = {row['id']: row for row in rows}
    if len(result) != len(rows):
        raise ValueError('duplicate stable ID')
    return result


class Inventory:
    def __init__(self, project, devices, automation):
        self.project, self.devices, self.automation = project, devices, automation
        self.items = index(project['items'])
        self.rooms = index(devices['rooms'])
        self.endpoints = index(devices['devices'])
        self.scenes = index([s for k in ('immediate_scenes', 'timed_scenes') for s in automation.get(k, [])])
        self.metadata = index(automation.get('scene_metadata', []))
        self.source_scenes = {s['scene_id']: s for s in project['lighting_scenes']}
        # Existing imported stable room IDs, plus the four historic named IDs.
        aliases = {'43': 'artstudio', '44': 'leisure', '47': 'study', '408': 'sunroom'}
        self.source_rooms = {}
        for item in project['items']:
            if item['type_code'] == '8':
                rid = aliases.get(item['id'], 'control4-room-' + item['id'])
                if rid in self.rooms:
                    self.source_rooms[(item.get('floor'), item['name'])] = rid
        self.source_calls = collections.defaultdict(list)
        for event in project.get('programmed_events', []):
            for node in walk(event.get('program', [])):
                if 'command_or_condition' not in node or node.get('enabled') is False:
                    continue
                for param in walk(node['command_or_condition']):
                    if param.get('tag') != 'param':
                        continue
                    children = param.get('children', [])
                    if not any(c.get('tag') == 'name' and c.get('text') == 'SCENE_ID' for c in children):
                        continue
                    for value in walk(children):
                        if value.get('tag') == 'static' and value.get('text') in self.source_scenes:
                            self.source_calls[value['text']].append({
                                'device': event['device_id'], 'event': event['event_id'],
                                'name': event.get('device_name'), 'room': event.get('room'),
                                'floor': self.items.get(event['device_id'], {}).get('floor'),
                                'node': node.get('id')})

    def source_room(self, item):
        return self.source_rooms.get((item.get('floor'), item.get('room')))

    def load(self, ident):
        device = self.endpoints.get(ident)
        if device is None:
            return {'id': ident, 'room': None, 'issue': '配置中缺少负载'}
        match = re.fullmatch(r'control4:(\d+)', ident)
        original = self.items.get(match[1]) if match else None
        current, source = device.get('room'), self.source_room(original) if original else None
        issue = None
        if current and current not in self.rooms:
            issue = '当前负载房间不存在'
        elif current and source and current != source:
            issue = '当前负载归属与原工程冲突'
        room = None if issue else current or source
        if not room:
            issue = issue or '原工程与当前配置均无法确认负载房间'
        return {'id': ident, 'name': device.get('name', ident), 'room': room,
                'configured_room': current, 'source_room': source,
                'source_item': original['id'] if original else None, 'issue': issue}

    def targets(self, sid, path=()):
        if sid in path:
            raise ValueError('cyclic scene reference: ' + sid)
        if sid not in self.scenes:
            raise ValueError('unresolved scene reference: ' + sid)
        outputs, refs = set(), set()
        scene = self.scenes[sid]
        for key in ('activate', 'deactivate', 'steps'):
            for node in walk(scene.get(key, [])):
                command = node.get('command')
                if isinstance(node.get('device'), str) and isinstance(command, dict):
                    # Conditions and LED feedback do not determine load location.
                    if command.get('kind') not in ('panel_led', 'panel_led_off'):
                        outputs.add(node['device'])
                if node.get('kind') == 'motor' and isinstance(node.get('device'), str):
                    outputs.add(node['device'])
                if node.get('kind') == 'scene':
                    ref = node['id']
                    more, nested = self.targets(ref, path + (sid,))
                    outputs.update(more)
                    refs.update(nested | {ref})
        for link in self.automation.get('scene_deactivations', []):
            if link['scene'] == sid and link.get('enabled', False):
                more, nested = self.targets(link['target'], path + (sid,))
                outputs.update(more)
                refs.update(nested | {link['target']})
        return outputs, refs

    def runtime_rows(self):
        internal = {p['selection_scene'] for p in self.automation.get('scene_presentations', [])}
        rows = []
        for sid, scene in self.scenes.items():
            targets, refs = self.targets(sid)
            loads = [self.load(x) for x in sorted(targets)]
            rooms = sorted({x['room'] for x in loads if x['room']})
            meta = self.metadata.get(sid, {})
            panels = []
            for panel in self.devices.get('keypads', []):
                for key in panel['keys']:
                    if key.get('action', {}).get('scene') == sid:
                        panels.append({'id': panel['id'], 'room': panel.get('room'),
                                       'key': key['number'], 'label': key.get('label')})
            source_ids = set(re.findall(r'SCENE_ID=(\d+)', scene.get('review_evidence', '')))
            source_ids.update(s['scene_id'] for s in self.project['lighting_scenes'] if s['name'] == sid)
            source_evidence = [{'id': s, 'name': self.source_scenes[s]['name'],
                                'panel_programs': self.source_calls[s]}
                               for s in sorted(source_ids) if s in self.source_scenes]
            issues = sorted({x['issue'] for x in loads if x['issue']})
            if len(rooms) == 1 and loads and not issues:
                scope, room, reason = 'room', rooms[0], '所有实际控制负载均指向同一房间；已排除面板 LED 与变量'
            else:
                scope, room = 'unassigned', None
                reason = '控制多个房间，需确认主要入口或公共区域归属' if len(rooms) > 1 else '缺少完整且一致的负载房间证据'
            # A whole-home marker must already have explicit reviewed metadata.
            if meta.get('scope') == 'household' and not meta.get('room'):
                scope, room, reason = 'household', None, '已有明确全屋归属；保留，不由跨房间负载自动推断'
            rows.append({'id': sid, 'name': meta.get('name', sid), 'scope': scope, 'room': room,
                         'previous_room': meta.get('room'), 'previous_scope': meta.get('scope'),
                         'enabled': scene.get('enabled', True), 'internal_profile': sid in internal,
                         'reason': reason, 'issues': issues, 'load_rooms': rooms, 'loads': loads,
                         'panels': panels, 'source_scenes': source_evidence, 'nested_scenes': sorted(refs)})
        # A declared public/profile pair moves together, or stays unassigned.
        by_id = {x['id']: x for x in rows}
        for pair in self.automation.get('scene_presentations', []):
            public, profile = by_id[pair['scene']], by_id[pair['selection_scene']]
            if (public['scope'], public['room']) != (profile['scope'], profile['room']):
                for row in (public, profile):
                    row.update(scope='unassigned', room=None, reason='显式公开入口与内部配方的负载归属不一致，需共同核对')
        return rows

    def original_rows(self):
        rows = []
        for scene in self.project['lighting_scenes']:
            items = [self.items.get(m['device_id'], {}) for m in scene['members']]
            rooms = sorted({self.source_room(x) for x in items if self.source_room(x)})
            complete = all(self.source_room(x) for x in items)
            # Coverage of the entire original lighting catalog, not its name.
            all_lights = {x['id'] for x in self.project['items'] if x.get('driver') == 'light_v2.c4i'}
            targets = {m['device_id'] for m in scene['members']}
            household = bool(all_lights) and targets == all_lights
            scope = 'household' if household else 'room' if len(rooms) == 1 and complete else 'unassigned'
            rows.append({'id': scene['scene_id'], 'name': scene['name'], 'scope': scope,
                         'room': rooms[0] if scope == 'room' else None, 'load_rooms': rooms,
                         'members': sorted(targets), 'source_panel_programs': self.source_calls[scene['scene_id']],
                         'reason': '覆盖原工程全部灯光负载' if household else '原工程全部负载在同一房间' if scope == 'room'
                         else '跨房间或楼层区域场景，不能自动当作全屋',
                         'runtime_definition_present': scene['name'] in self.scenes})
        return rows


def candidate(automation, rows):
    result = copy.deepcopy(automation)
    metadata = index(result.get('scene_metadata', []))
    for row in rows:
        entry = metadata.setdefault(row['id'], {'id': row['id'], 'name': row['name']})
        entry['scope'] = row['scope']
        if row['room']:
            entry['room'] = row['room']
        else:
            entry.pop('room', None)
    result['scene_metadata'] = list(metadata.values())
    # Placement only: no scene is created, enabled or edited.
    assert {k: v for k, v in automation.items() if k != 'scene_metadata'} == {k: v for k, v in result.items() if k != 'scene_metadata'}
    return result


def markdown(report, rooms):
    def label(rid):
        r = rooms.get(rid, {})
        return ' / '.join(x for x in (r.get('floor'), r.get('name')) if x) or '—'
    def safe(value):
        return str(value).replace('|', r'\|').replace('\n', ' ')
    rows = report['runtime_scenes']
    lines = ['# 场景批量归属清单', '',
             '离线候选；尚未发布。输入哈希见 inventory.json。未读取现网活动版本。发布前须重新核对活动版本并基于最新配置合并归属，不能直接覆盖新版本。原工程楼层按稳定房间 ID 对照当前楼层，未按同名房间合并。', '',
             f"运行配置 revision {report['revision']}：{len(rows)} 个定义，{sum(not x['internal_profile'] for x in rows)} 个公开入口；原工程 {len(report['original_scenes'])} 个灯光场景。", '',
             '## 待确认', '', '| 场景 | 旧归属 | 实际负载位置 | 关联面板 / 键 | 问题 |', '|---|---|---|---|---|']
    for row in rows:
        if row['scope'] == 'unassigned':
            panels = '；'.join(f"{p['id']} / {p['key']} {p.get('label', '')} ({label(p.get('room'))})" for p in row['panels']) or '无直接绑定'
            lines.append('| ' + ' | '.join(map(safe, [row['id'], label(row['previous_room']), '；'.join(label(r) for r in row['load_rooms']), panels, row['reason']])) + ' |')
    lines += ['', '## 明确房间归属', '', '| 场景 ID | 显示名称 | 房间 | 原归属 | 类型 | 负载 ID | 关联面板 / 键 |', '|---|---|---|---|---|---|---|']
    for row in sorted(rows, key=lambda r: (r['room'] or '', r['id'])):
        if row['scope'] == 'room':
            lines.append('| ' + ' | '.join(map(safe, [row['id'], row['name'], label(row['room']), label(row['previous_room']), '内部配方' if row['internal_profile'] else '公开入口', '；'.join(x['id'] for x in row['loads']), '；'.join(f"{x['id']} / {x['key']} {x.get('label', '')}" for x in row['panels']) or '无直接绑定'])) + ' |')
    lines += ['', '## 原 Control4 场景', '',
              '全屋项目仅记录来源归属；不创建或启用当前配置中不存在的动作。F1 全开/全关保留区域场景问题，不能标为全屋。', '',
              '| 来源 ID | 场景 | 归属 | 原工程关联面板 / 事件 | 当前存在同名定义 |', '|---|---|---|---|---|']
    for row in report['original_scenes']:
        place = '全屋场景' if row['scope'] == 'household' else label(row['room']) if row['room'] else '待确认区域：' + '；'.join(label(x) for x in row['load_rooms'])
        calls = '；'.join(sorted({f"{x['device']} / {x['event']} {x.get('name') or ''}" for x in row['source_panel_programs']})) or '无直接程序引用'
        lines.append('| ' + ' | '.join(map(safe, [row['id'], row['name'], place, calls, '是' if row['runtime_definition_present'] else '否'])) + ' |')
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ('project', 'devices', 'automation', 'output'):
        parser.add_argument('--' + flag, required=True, type=Path)
    args = parser.parse_args()
    project, devices, automation = [json.loads(p.read_text()) for p in (args.project, args.devices, args.automation)]
    if devices['revision'] != automation['configuration_revision']:
        raise ValueError('device and automation revisions differ')
    archive = Path(project['source'])
    if digest(archive) != project['sha256']:
        raise ValueError('original Control4 archive hash differs from parsed evidence')
    inventory = Inventory(project, devices, automation)
    rows = inventory.runtime_rows()
    report = {'state': 'offline_placement_candidate_not_published', 'revision': devices['revision'],
              'inputs': {str(p): digest(p) for p in (args.project, args.devices, args.automation, archive)},
              'runtime_counts': dict(collections.Counter(x['scope'] for x in rows)),
              'public_counts': dict(collections.Counter(x['scope'] for x in rows if not x['internal_profile'])),
              'original_scenes': inventory.original_rows(), 'runtime_scenes': rows,
              'publication_or_device_commands': 0}
    proposed = candidate(automation, rows)
    next_revision = devices['revision'] + 1
    proposed['configuration_revision'] = next_revision
    proposed_devices = copy.deepcopy(devices)
    proposed_devices['revision'] = next_revision
    report['candidate_revision'] = next_revision
    report['room_changes'] = [dict(id=x['id'], before=x['previous_room'], after=x['room'], scope=x['scope'])
                              for x in rows if x['previous_room'] != x['room']]
    args.output.mkdir(parents=True, exist_ok=False)
    for name, value in [('inventory.json', report), ('automation.placement.json', proposed), ('devices.placement.json', proposed_devices)]:
        (args.output / name).write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    (args.output / '归属清单.md').write_text(markdown(report, inventory.rooms))
    print(json.dumps({'output': str(args.output), 'runtime': report['runtime_counts'], 'public': report['public_counts'],
                      'original': dict(collections.Counter(x['scope'] for x in report['original_scenes']))}, ensure_ascii=False))


if __name__ == '__main__':
    main()

import copy
import unittest

from assign_scene_rooms import Inventory, candidate


class ScenePlacementTests(unittest.TestCase):
    def setUp(self):
        self.project = {'items': [
            {'id': '41', 'name': '卧室', 'type_code': '8', 'floor': '第一层', 'room': '卧室'},
            {'id': '36', 'name': '卧室', 'type_code': '8', 'floor': '第二层', 'room': '卧室'},
            {'id': '111', 'name': '一层灯代理', 'driver': 'light_v2.c4i', 'type_code': '7', 'floor': '第一层', 'room': '卧室'},
            {'id': '112', 'name': '二层灯代理', 'driver': 'light_v2.c4i', 'type_code': '7', 'floor': '第二层', 'room': '卧室'},
            {'id': '211', 'name': '一层灯驱动', 'type_code': '6', 'floor': '第一层', 'room': '卧室'},
            {'id': '212', 'name': '二层灯驱动', 'type_code': '6', 'floor': '第二层', 'room': '卧室'},
        ], 'lighting_scenes': [
            {'scene_id': '1', 'name': '无全屋字样', 'members': [{'device_id': '111'}, {'device_id': '112'}]},
            {'scene_id': '2', 'name': '全屋名称但只控一灯', 'members': [{'device_id': '111'}]},
        ]}
        self.devices = {'rooms': [
            {'id': 'control4-room-41', 'name': '卧室', 'floor': '2层'},
            {'id': 'control4-room-36', 'name': '卧室', 'floor': '3层'},
        ], 'devices': [{'id': 'control4:211'}, {'id': 'control4:212'}], 'keypads': [
            {'id': 'remote-panel', 'room': 'control4-room-36', 'keys': [
                {'number': 1, 'label': '远程灯', 'action': {'scene': 'one'}}]}]}
        self.automation = {'configuration_revision': 51, 'rules': [], 'variables': [],
                           'immediate_scenes': [self.scene('one', 'control4:211')],
                           'timed_scenes': [], 'scene_metadata': [], 'scene_presentations': []}

    @staticmethod
    def scene(sid, target):
        return {'id': sid, 'enabled': True, 'activate': [
            {'action': {'kind': 'command', 'device': target, 'command': {'kind': 'switch', 'on': True}}},
            {'action': {'kind': 'command', 'device': 'other-room-panel-led', 'command': {'kind': 'panel_led', 'on': True}}},
        ]}

    def inventory(self):
        return Inventory(self.project, self.devices, self.automation)

    def test_source_floor_identity_beats_remote_panel_and_led_and_existing_wrong_room(self):
        self.automation['scene_metadata'] = [{'id': 'one', 'name': '远程', 'room': 'control4-room-36'}]
        row = self.inventory().runtime_rows()[0]
        self.assertEqual(row['room'], 'control4-room-41')
        self.assertEqual(row['panels'][0]['room'], 'control4-room-36')
        self.assertEqual([x['id'] for x in row['loads']], ['control4:211'])

    def test_both_conditional_branches_and_nested_scene_loads_remain_ambiguous(self):
        self.automation['immediate_scenes'].append(self.scene('two', 'control4:212'))
        self.automation['immediate_scenes'][0]['deactivate'] = [{'action': {
            'kind': 'if', 'then_actions': [{'action': {'kind': 'scene', 'id': 'two', 'activate': True}}],
            'else_actions': []}}]
        row = self.inventory().runtime_rows()[0]
        self.assertEqual(row['scope'], 'unassigned')
        self.assertEqual(row['nested_scenes'], ['two'])
        self.assertEqual(row['load_rooms'], ['control4-room-36', 'control4-room-41'])

    def test_missing_and_conflicting_load_evidence_never_falls_back_to_panel(self):
        self.devices['devices'][0]['room'] = 'control4-room-36'
        row = self.inventory().runtime_rows()[0]
        self.assertEqual(row['scope'], 'unassigned')
        self.assertIn('冲突', row['issues'][0])
        self.automation['immediate_scenes'][0] = self.scene('one', 'missing-load')
        self.assertEqual(self.inventory().runtime_rows()[0]['scope'], 'unassigned')

    def test_household_uses_all_original_loads_and_never_its_display_name(self):
        rows = self.inventory().original_rows()
        self.assertEqual(rows[0]['scope'], 'household')
        self.assertEqual(rows[1]['scope'], 'room')

    def test_candidate_preserves_all_actions_and_unknown_metadata_fields_and_pairs(self):
        self.automation['immediate_scenes'].append(self.scene('profile', 'control4:211'))
        self.automation['scene_presentations'] = [{'scene': 'one', 'selection_scene': 'profile'}]
        self.automation['scene_metadata'] = [{'id': 'one', 'name': '远程', 'future': {'keep': True}}]
        before = copy.deepcopy(self.automation)
        result = candidate(self.automation, self.inventory().runtime_rows())
        self.assertEqual(self.automation, before)
        self.assertEqual(result['immediate_scenes'], before['immediate_scenes'])
        self.assertEqual(result['scene_presentations'], before['scene_presentations'])
        self.assertEqual(result['scene_metadata'][0]['future'], {'keep': True})
        self.assertTrue(all(x['room'] == 'control4-room-41' for x in result['scene_metadata']))
        self.automation['immediate_scenes'][1] = self.scene('profile', 'control4:212')
        self.assertTrue(all(x['scope'] == 'unassigned' for x in self.inventory().runtime_rows()))

    def test_unknown_or_cyclic_references_stop_candidate_generation(self):
        self.automation['immediate_scenes'][0]['activate'] = [{'action': {'kind': 'scene', 'id': 'one', 'activate': True}}]
        with self.assertRaisesRegex(ValueError, 'cyclic'):
            self.inventory().runtime_rows()
        self.automation['immediate_scenes'][0]['activate'][0]['action']['id'] = 'missing'
        with self.assertRaisesRegex(ValueError, 'unresolved'):
            self.inventory().runtime_rows()


if __name__ == '__main__':
    unittest.main()

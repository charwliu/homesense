import copy
import unittest
from migrate_keypad_objects import candidate


class KeypadMigrationTests(unittest.TestCase):
    def fixture(self):
        document = {"schema_version": 2, "revision": 7,
                    "gateways": [{"id": "bus", "modules": [{"id": "module", "address": "41",
                                  "profile": "ts6_86_rs485_v2_6_5_6key", "led_mode": "manual"}]}],
                    "devices": [{"id": f"legacy:{i}", "name": f"Original LED {i}", "room": "room",
                                 "module_id": "module", "channel": i, "enabled": i != 4}
                                for i in [6, 1, 4]]}
        review = [{"id": "panel", "name": "Panel", "module_id": "module", "room": "room",
                   "review_evidence": "Synthetic review"}]
        return document, review

    def test_migration_preserves_alias_identity_scope_and_hardware_declarations(self):
        source, review = self.fixture()
        before = copy.deepcopy(source)
        result = candidate(source, review)
        self.assertEqual(source, before)
        self.assertEqual(result["gateways"], source["gateways"])
        self.assertEqual(result["revision"], source["revision"])
        self.assertEqual(result["devices"], [])
        keys = result["keypads"][0]["keys"]
        self.assertEqual([k["number"] for k in keys], [1, 4, 6])
        for key in keys:
            old = next(d for d in source["devices"] if d["channel"] == key["number"])
            self.assertEqual(key["led_endpoint"], {k: old[k] for k in ["id", "name", "enabled"]})
            self.assertNotIn("action", key)

    def test_no_guessing_modes_rooms_unknown_profiles_or_extra_legacy_fields(self):
        for kind in ["profile", "room", "extra", "duplicate"]:
            source, review = self.fixture()
            if kind == "profile": source["gateways"][0]["modules"][0]["profile"] = "unverified_keypad"
            if kind == "room": review[0]["room"] = "different"
            if kind == "extra": source["devices"][0]["unrecognized"] = True
            if kind == "duplicate": review.append(copy.deepcopy(review[0]))
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                candidate(source, review)


if __name__ == "__main__":
    unittest.main()

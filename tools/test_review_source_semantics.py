"""Synthetic dependency review regressions; no household data or network."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location("source_semantics", Path(__file__).with_name("review_source_semantics.py"))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def code(key, payload="", kind="1", children="", enabled="True", target="2"):
    return f"<codeitem><id>{key}</id><device>{target}</device><type>{kind}</type><enabled>{enabled}</enabled><display>PRIVATE-DISPLAY</display><cmdcond>{payload}</cmdcond><subitems>{children}</subitems></codeitem>"


def event(body, key="1", owner="1"):
    return f"<event><deviceid>{owner}</deviceid><eventid>{key}</eventid>{body}</event>"


def command(operation, params=""):
    return f"<devicecommand owneridtype='' owneriditem='-1'><command>{operation}</command><params>{params}</params></devicecommand>"


def param(name, value, kind="INTEGER"):
    return f"<param><name>{name}</name><value type='{kind}'><static>{value}</static></value></param>"


def project(events="", state="", calendar="", variables=""):
    return (f"<project><systemitems><item><id>1</id><name>PRIVATE-HOME-NAME</name><itemdata><config_data_file>blind.c4i</config_data_file></itemdata><state><![CDATA[{state}]]></state><state_values>{calendar}</state_values><subitems><item><id>2</id></item></subitems></item></systemitems><variables>{variables}</variables><event_mgr>{events}</event_mgr></project>").encode()


class ReviewTests(unittest.TestCase):
    def test_nested_blocker_keeps_later_motor_and_all_variable_dependencies(self):
        conditional = "<deviceconditional owneridtype='variable' owneriditem='9' name='=='><param name='value' type='int'>0</param></deviceconditional>"
        assignment = "<devicecommand owneridtype='variable' owneriditem='9' name='+'><param name='value' type='int'>2</param></devicecommand>"
        body = code("a", conditional, "2", code("b", command("STOP")) + code("c", assignment, enabled="False"))
        result = MODULE.review(project(event(body), variables="<variable variableid='9' type='int'>PRIVATE-SAVED-VALUE</variable>"))
        program = result["programs"][0]
        self.assertEqual([s["operation"] for s in program["operations"]], ["==", "STOP", "+"])
        variable = result["variables"][0]
        self.assertEqual(len(variable["reads"]), 1)
        self.assertEqual(len(variable["writes"]), 1)
        self.assertFalse(variable["writes"][0]["enabled"])
        self.assertTrue(variable["writes"][0]["implicit_read"])
        self.assertTrue(variable["integer_semantics_required"])
        self.assertFalse(variable["semantic_approved"])
        self.assertIn("lifetime", variable["review_gates"])
        self.assertFalse(variable["saved_value_imported"])
        exported = json.dumps(result)
        for secret in ["PRIVATE-HOME-NAME", "PRIVATE-DISPLAY", "PRIVATE-SAVED-VALUE"]:
            self.assertNotIn(secret, exported)

    def test_scene_static_dependency_preserves_rates_and_source_order_without_units(self):
        state = "<state><AdvScene><name>PRIVATE-SCENE-NAME</name><scene_id>7</scene_id><track_mode>2</track_mode><all_members><AdvSceneMember><device_id>2</device_id><all_elements><element><level>10</level><delay>0</delay><rate>750</rate></element><element><level>40</level><delay>5</delay><rate>1</rate></element></all_elements></AdvSceneMember></all_members></AdvScene></state>"
        result = MODULE.review(project(event(code("a", command("ACTIVATE_SCENE", param("SCENE_ID", "7")))), state))
        dependency = result["programs"][0]["dependencies"][0]
        self.assertTrue(dependency["resolved"])
        scene = result["scenes"][0]
        self.assertEqual([e["rate"] for e in scene["members"][0]["elements"]], ["750", "1"])
        self.assertEqual([e["ordinal"] for e in scene["members"][0]["elements"]], [0, 1])
        self.assertFalse(scene["semantic_approved"])
        self.assertIn("delay_and_rate_semantics", scene["review_gates"])
        self.assertEqual(len(scene["referenced_by"]), 1)
        self.assertNotIn("PRIVATE-SCENE-NAME", json.dumps(result))
        for malformed in [state.replace("<rate>750</rate>","<rate>Open</rate>"),state.replace("<level>10</level>","<level>101</level>")]:
            with self.assertRaisesRegex(ValueError,"source scene"):
                MODULE.review(project(state=malformed))

    def test_dynamic_scene_and_symbolic_motor_parameter_do_not_become_numeric(self):
        dynamic = "<param><name>SCENE_ID</name><value><variable>9</variable></value></param>"
        result = MODULE.review(project(event(code("a", command("ACTIVATE_SCENE", dynamic)) + code("b", command("SET_LEVEL_TARGET", param("LEVEL_TARGET_NAME", "Open"))))))
        self.assertFalse(result["programs"][0]["dependencies"][0]["resolved"])
        motor = result["programs"][0]["operations"][1]["parameters"][0]
        self.assertEqual(motor["declared_type"], "INTEGER")
        self.assertEqual(motor["static"], "Open")
        self.assertEqual(result["runtime_definitions"], [])

    def test_empty_program_does_not_approve_calendar_agent_or_create_task(self):
        empty = code("0", target="0")
        calendar = "<value name='entry_data'><![CDATA[<entries><entry><eventid>1</eventid><description>PRIVATE-UPDATE-NAME</description><enabled>True</enabled><when><hour>3</hour></when></entry></entries>]]></value>"
        result = MODULE.review(project(event(empty), calendar=calendar))
        self.assertTrue(result["programs"][0]["source_empty_noop"])
        self.assertTrue(result["programs"][0]["semantic_approved"])
        self.assertTrue(result["calendars"][0]["source_program_empty"])
        self.assertIn("agent_owned_behavior", result["calendars"][0]["review_gates"])
        self.assertFalse(result["calendars"][0]["runtime_task_generated"])
        self.assertNotIn("PRIVATE-UPDATE-NAME", json.dumps(result))
        nested = MODULE.review(project(event(code("0", children=code("x", enabled="False"), target="0"))))
        self.assertFalse(nested["programs"][0]["source_empty_noop"])

    def test_hash_bound_typed_and_prior_evidence_remains_limited(self):
        data = project(event(code("0", target="0")))
        fingerprint = hashlib.sha256(data).hexdigest()
        typed = {"project_xml_sha256": fingerprint, "review": {"automatic_execution": False, "syntax_only": True,
            "events": [{"owner": "event:1:1", "source_empty_noop": True, "standard": {"syntax_compiled": True}, "typed_sites": []}]}}
        prior = {"source_sha256": fingerprint, "scenes": [], "programs": [{"device_id": "1", "event_id": "1", "status": "study_manual_runtime"}]}
        result = MODULE.review(data, typed, prior)
        self.assertEqual(result["programs"][0]["prior_limited_status"], "study_manual_runtime")
        self.assertFalse(result["stage_d_complete"])
        typed["project_xml_sha256"] = "wrong"
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            MODULE.review(data, typed)
        prior["source_sha256"] = "wrong"
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            MODULE.review(data, prior=prior)

    def test_verified_led_source_subset_closes_only_its_conversion_subgate(self):
        body=code("0",children=code("1",command("Turn On",param("LED Number","2"))),target="0")
        data=project(event(body))
        typed={"project_xml_sha256":hashlib.sha256(data).hexdigest(),"review":{"automatic_execution":False,"syntax_only":True,"events":[{"owner":"event:1:1","source_empty_noop":False,"standard":{"syntax_compiled":True},"typed_sites":[]}]},"direct_led_request_proofs":[{"owner":"event:1:1","source_request_sequence":[{"device":"synthetic:key:2","on":True}],"ordered_ast_request_equivalent":True,"semantic_approved":False,"runtime_rule_enabled":False,"pending_gates":["driver LED behavior","trigger semantics"]}]}
        result=MODULE.review(data,typed)
        program=result["programs"][0]
        self.assertEqual(program["closed_source_subgate"],"straight_line_led_request_sequence")
        self.assertNotIn("branch_semantics",program["review_gates"])
        self.assertIn("trigger_semantics",program["review_gates"])
        self.assertFalse(program["semantic_approved"])
        self.assertEqual(result["summary"]["closed_source_led_request_sequences"],1)
        typed["direct_led_request_proofs"][0]["semantic_approved"]=True
        with self.assertRaisesRegex(ValueError,"limited source equivalence"):
            MODULE.review(data,typed)

    def test_embedded_agent_schedule_without_event_is_preserved_without_task(self):
        result=MODULE.review(project(state="<agent><backup_schedule><days>1</days><hour>3</hour></backup_schedule></agent>"))
        self.assertEqual(result["summary"]["embedded_agent_schedules"],1)
        self.assertEqual(result["summary"]["calendar_entries"],0)
        self.assertFalse(result["embedded_agent_schedules"][0]["runtime_task_generated"])

    def test_ramp_api_duration_unit_is_scoped_and_does_not_approve_execution(self):
        payload=command("RAMP_TO_LEVEL",param("TIME","1000")+param("LEVEL","0"))
        result=MODULE.review(project(event(code("a",payload))))
        site=result["programs"][0]["operations"][0]
        self.assertEqual(site["closed_parameter_subgate"]["duration_ms"],1000)
        self.assertEqual(site["closed_parameter_subgate"]["api_unit"],"milliseconds")
        self.assertFalse(site["closed_parameter_subgate"]["driver_execution_confirmed"])
        self.assertFalse(result["programs"][0]["semantic_approved"])
        self.assertEqual(result["runtime_definitions"],[])
        for changed in [payload.replace("INTEGER","STRING"),payload.replace("<static>1000</static>","<variable>1000</variable>"),payload.replace("<static>1000</static>","<static opaque='unreviewed'>1000</static>"),payload.replace("<static>1000</static>","<static>Open</static>")]:
            review=MODULE.review(project(event(code("a",changed))))
            self.assertEqual(review["summary"]["closed_ramp_time_api_units"],0)

    def test_toggle_edges_are_one_hop_and_unknown_sentinel_is_not_all_off(self):
        state="<state>"+"".join(f"<AdvScene><name>PRIVATE</name><scene_id>{key}</scene_id><toggle_id>{toggle}</toggle_id><all_members/></AdvScene>" for key,toggle in [(7,8),(8,7),(9,65535)])+"</state>"
        body=code("a",command("DEACTIVATE_SCENE",param("SCENE_ID","7")))+code("b",command("DEACTIVATE_SCENE",param("SCENE_ID","9")))
        result=MODULE.review(project(event(body),state))
        self.assertEqual(result["summary"]["resolved_source_toggle_destinations"],2)
        self.assertEqual(result["summary"]["unresolved_source_toggle_destinations"],1)
        self.assertEqual(result["summary"]["resolved_deactivate_calls"],1)
        self.assertEqual(result["summary"]["unresolved_deactivate_calls"],1)
        target=result["programs"][0]["dependencies"][0]["deactivation_target"]
        self.assertEqual(target["candidates"],["scene:1:8"])
        self.assertIn("not recursive",target["execution_scope"])
        self.assertFalse(result["scenes"][2]["toggle_dependency"]["unresolved_sentinel_semantics_known"])
        self.assertFalse(any(s["semantic_approved"] for s in result["scenes"]))

    def test_all_off_toggle_target_keeps_its_own_rate_not_the_source_rate(self):
        state="<state>"+"".join(f"<AdvScene><name>PRIVATE</name><scene_id>{key}</scene_id><toggle_id>{toggle}</toggle_id><all_members><AdvSceneMember><device_id>2</device_id><all_elements><element><level>{level}</level><delay>0</delay><rate>{rate}</rate></element></all_elements></AdvSceneMember></all_members></AdvScene>" for key,toggle,level,rate in [(7,8,100,750),(8,7,0,1000)])+"</state>"
        result=MODULE.review(project(event(code("a",command("DEACTIVATE_SCENE",param("SCENE_ID","7")))),state))
        difference=result["programs"][0]["dependencies"][0]["source_target_comparison"]
        self.assertTrue(difference["target_all_levels_zero"])
        self.assertTrue(difference["same_member_order"])
        self.assertTrue(difference["same_element_order_and_delay"])
        self.assertFalse(difference["same_rate_sequence"])
        self.assertTrue(difference["use_complete_target_definition"])
        self.assertFalse(difference["source_rate_unit_confirmed"])

    def test_binary_assignments_and_unresolved_variables_are_not_approved(self):
        assignment = "<devicecommand owneridtype='variable' owneriditem='9' name='='><param name='value' type='int'>1</param></devicecommand>"
        result = MODULE.review(project(event(code("a", assignment)), variables="<variable variableid='9' type='bool'>0</variable>"))
        self.assertFalse(result["variables"][0]["integer_semantics_required"])
        self.assertFalse(result["variables"][0]["semantic_approved"])
        self.assertFalse(result["variables"][0]["runtime_range_complete"])
        missing = MODULE.review(project(event(code("a", assignment))))
        self.assertEqual(missing["summary"]["unresolved_dependencies"], 1)

    def test_nominal_blind_event_identity_does_not_confirm_physical_state(self):
        for event_id,label in [("3","Stopped"),("5","Moving")]:
            result=MODULE.review(project(event(code("a",command("Turn Off",param("LED Number","1"))),key=event_id)))
            identity=result["programs"][0]["nominal_proxy_event"]
            self.assertEqual(identity["name"],label)
            self.assertFalse(identity["physical_observation_confirmed"])
            self.assertFalse(identity["migration_trigger_confirmed"])
            self.assertIn("trigger_semantics",result["programs"][0]["review_gates"])

    def test_cli_private_permissions_no_overwrite_and_archive_scope(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "source.c4p"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("project.xml", project())
                archive.writestr("identity.db", b"PRIVATE-IDENTITY")
                archive.writestr("drivers/locked.c4z", b"PRIVATE-DRIVER")
            output = Path(temporary) / "private"
            command_line = [sys.executable, str(Path(MODULE.__file__)), str(source), "--private-dir", str(output)]
            completed = subprocess.run(command_line, capture_output=True, text=True, check=True)
            self.assertEqual(json.loads(completed.stdout)["programs"], 0)
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
            self.assertEqual((output / "semantic-review.json").stat().st_mode & 0o777, 0o600)
            self.assertNotIn("PRIVATE-IDENTITY", (output / "semantic-review.json").read_text())
            before = (output / "semantic-review.json").read_bytes()
            self.assertNotEqual(subprocess.run(command_line, capture_output=True).returncode, 0)
            self.assertEqual((output / "semantic-review.json").read_bytes(), before)
        with self.assertRaisesRegex(ValueError, "DTD"):
            MODULE.review(b"<!DOCTYPE project><project/>")
        archive_bytes = io.BytesIO()
        with zipfile.ZipFile(archive_bytes, "w") as archive:
            archive.writestr("../project.xml", project())
        with self.assertRaisesRegex(ValueError, "root project"):
            MODULE.review(archive_bytes.getvalue())

    def test_plain_descriptor_capabilities_do_not_read_encrypted_lua_or_prove_runtime(self):
        package=io.BytesIO()
        with zipfile.ZipFile(package,"w") as zipped:
            zipped.writestr("driver.xml","<driver><capabilities><advanced_scene_support>True</advanced_scene_support><ramp_level>False</ramp_level></capabilities><config><property><name>Ramp Timer</name><type>LIST</type><default>400</default><items><item>400ms</item></items></property></config></driver>")
            zipped.writestr("driver.lua.encrypted",b"PRIVATE-ENCRYPTED-CODE")
        archive=io.BytesIO()
        with zipfile.ZipFile(archive,"w") as zipped:
            zipped.writestr("project.xml",project())
            zipped.writestr("drivers/protocol.c4z",package.getvalue())
            zipped.writestr("identity.db",b"PRIVATE-IDENTITY")
        metadata=MODULE.descriptor_metadata(archive.getvalue(),{"protocol.c4z"})
        self.assertTrue(metadata[0]["encrypted_payload_entries_present"])
        self.assertFalse(metadata[0]["lua_read_or_executed"])
        self.assertFalse(metadata[0]["runtime_conformance_verified"])
        self.assertFalse(metadata[0]["ramp_timer_properties"][0]["semantics_verified"])
        self.assertNotIn("PRIVATE-ENCRYPTED-CODE",json.dumps(metadata))
        self.assertNotIn("PRIVATE-IDENTITY",json.dumps(metadata))


if __name__ == "__main__":
    unittest.main()

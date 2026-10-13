import hashlib
import unittest
import xml.etree.ElementTree as ET

from compare_source_ast import Unreviewed, compare


def code(key, kind, payload="", children="", device="proxy", enabled="True", extra=""):
    return f'<codeitem><id>{key}</id><device>{device}</device><type>{kind}</type><enabled>{enabled}</enabled><display></display><cmdcond>{payload}</cmdcond><subitems>{children}</subitems>{extra}</codeitem>'


def variable(operation, value, condition=False):
    tag = "deviceconditional" if condition else "devicecommand"
    return f'<{tag} owneridtype="variable" owneriditem="mode" name="{operation}"><param name="value" type="int">{value}</param></{tag}>'


def command(on):
    return f'<devicecommand owneridtype="" owneriditem="-1"><command>{"ON" if on else "OFF"}</command><params/></devicecommand>'


def fixture(body, ast):
    xml = f'<project><event_mgr><event><deviceid>panel</deviceid><eventid>2</eventid>{body}</event></event_mgr></project>'.encode()
    product = {"source_xml_sha256": hashlib.sha256(xml).hexdigest(), "programs": [{"key": "event:panel:2", "compiled": True, "ast": ast}]}
    return xml, {"proxy": {"kind": "switch", "device": "synthetic:light"}}, product


class SourceAstTest(unittest.TestCase):
    def test_nested_else_and_arithmetic_keep_source_order_and_expression_ownership(self):
        write = code("3", "1", variable("+", 2))
        yes = code("2", "2", variable("==", 0, True), write)
        no = code("4", "4", children=code("5", "1", variable("-", 1)))
        body = code("1", "2", variable("!=", 1, True), yes + no)
        expected = [["if", ["not", ["equal", ["variable", "mode"], ["integer", 1]]],
                     [["if", ["equal", ["variable", "mode"], ["integer", 0]],
                       [["set", "mode", ["add", ["variable", "mode"], ["integer", 2]]]],
                       [["set", "mode", ["subtract", ["variable", "mode"], ["integer", 1]]]]]], []]]
        result = compare(*fixture(body, expected))
        self.assertEqual(result["summary"]["ordered_ast_equivalent"], 1)
        self.assertFalse(result["summary"]["full_semantic_approval"])

    def test_compound_predicate_preserves_connective_and_device_condition(self):
        for display, op in [("AND", "all"), ("OR", "any")]:
            connective = code("2", "6").replace("<display></display>", f"<display>{display}</display>")
            right = code("3", "2", variable("&lt;", 4, True))
            payload = '<deviceconditional owneridtype="" owneriditem="-1"><name>LOAD_OFF</name></deviceconditional>'
            body = code("1", "2", payload, code("4", "1", command(True)), extra=f"<expression>{connective}{right}</expression>")
            expected = [["if", [op, [["equal", ["on", "synthetic:light"], ["bool", False]], ["less", ["variable", "mode"], ["integer", 4]]]],
                         [["command", "synthetic:light", "switch", True]], []]]
            self.assertEqual(compare(*fixture(body, expected))["summary"]["mismatch"], 0)

    def test_reordered_actions_and_modified_condition_do_not_pass(self):
        body = code("1", "1", command(True)) + code("2", "1", command(False))
        incorrect = [["command", "synthetic:light", "switch", False], ["command", "synthetic:light", "switch", True]]
        self.assertEqual(compare(*fixture(body, incorrect))["summary"]["mismatch"], 1)
        body = code("1", "2", variable("!=", 1, True))
        incorrect = [["if", ["equal", ["variable", "mode"], ["integer", 1]], [], []]]
        self.assertEqual(compare(*fixture(body, incorrect))["summary"]["mismatch"], 1)

    def test_unknown_source_shape_does_not_inherit_product_success(self):
        for body in [code("1", "1", command(True), extra="<opaque/>"),
                     code("1", "1", command(True)).replace("<codeitem>", '<codeitem unknown="1">'),
                     code("1", "1", variable("=", 1)).replace('type="int"', 'type="int" future="1"')]:
            report = compare(*fixture(body, [["command", "synthetic:light", "switch", True]]))
            self.assertEqual(report["summary"]["ordered_ast_equivalent"], 0)
            self.assertEqual(report["summary"]["product_compiled_in_unreviewed_subset"], 1)

    def test_disabled_conditional_cannot_capture_else_for_previous_conditional(self):
        first = code("1", "2", variable("==", 0, True))
        disabled = code("2", "2", variable("==", 1, True), enabled="False")
        otherwise = code("3", "4", children=code("4", "1", command(True)))
        result = compare(*fixture(first + disabled + otherwise, []))
        self.assertEqual(result["summary"]["independent_source_unreviewed"], 1)

    def test_source_fingerprint_duplicate_missing_and_non_boolean_observations_rejected(self):
        xml, bindings, product = fixture(code("1", "1", command(True)), [])
        product["source_xml_sha256"] = "wrong"
        with self.assertRaises(Unreviewed):
            compare(xml, bindings, product)
        product["source_xml_sha256"] = hashlib.sha256(xml).hexdigest()
        product["programs"] *= 2
        with self.assertRaises(Unreviewed):
            compare(xml, bindings, product)
        product["programs"] = []
        with self.assertRaises(Unreviewed):
            compare(xml, bindings, product)
        product["programs"] = [{"key": "event:panel:2", "compiled": "true", "ast": []}]
        with self.assertRaises(Unreviewed):
            compare(xml, bindings, product)


if __name__ == "__main__":
    unittest.main()

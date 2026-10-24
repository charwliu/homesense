"""Local synthetic backup regressions; never deployed to either host."""
import contextlib
import datetime as dt
import importlib.util
import io
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("backup", Path(__file__).with_name("homesense_backup.py"))
backup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(backup)


class BackupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.state = self.root / "live"
        for name in backup.DATABASES:
            path = self.state / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with contextlib.closing(sqlite3.connect(path)) as db, db:
                if name == "automation-tcp.sqlite":
                    db.executescript(
                        "CREATE TABLE active_configuration(revision INTEGER);"
                        "CREATE TABLE configuration_revision(revision INTEGER,payload BLOB);"
                        "CREATE TABLE automation_revision(revision INTEGER,payload BLOB);"
                        "PRAGMA user_version=27;")
                    db.execute("INSERT INTO active_configuration VALUES(52)")
                    db.execute("INSERT INTO configuration_revision VALUES(52,?)",
                               (json.dumps({"schema_version": 1, "revision": 52, "devices": []}),))
                    db.execute("INSERT INTO automation_revision VALUES(52,?)",
                               (json.dumps({"schema_version": 1, "configuration_revision": 52}),))
                elif name.endswith("identity.sqlite"):
                    db.executescript(
                        "CREATE TABLE entries(key TEXT,value BLOB);"
                        "CREATE TABLE accessory_ids(device TEXT,aid INTEGER);"
                        "CREATE TABLE instance_ids(aid INTEGER,resource TEXT,iid INTEGER);")
                    db.execute("INSERT INTO entries VALUES('config',?)", (b'{"host":"192.168.3.3","pin":"synthetic"}',))
                    db.execute("INSERT INTO entries VALUES('pair:test',?)", (b"synthetic-only-pair",))
                else:
                    db.execute("CREATE TABLE payload(value TEXT)")
                    db.execute("INSERT INTO payload VALUES('synthetic permission')")
        self.static = {}
        for index, name in enumerate(backup.STATIC):
            path = self.root / "static" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(("synthetic-runtime-" + str(index)).encode())
            self.static[name] = path

    def snapshot(self, suffix="one"):
        path = self.root / ("snapshot-" + suffix)
        manifest = backup.build_snapshot(path, self.state, self.static,
                                         {"executable_sha256": "synthetic"})
        archive = self.root / (suffix + ".tar.gz")
        backup.pack_snapshot(path, archive)
        return path, manifest, archive

    def receive(self, archive):
        with open(archive, "rb") as stream:
            return backup.receive(stream, self.root / "receiver", lambda: None)

    def test_complete_roundtrip_preserves_state_and_permissions(self):
        path, manifest, archive = self.snapshot()
        receipt = self.receive(archive)
        latest = (self.root / "receiver/latest").resolve()
        self.assertEqual(receipt["snapshot_id"], manifest["snapshot_id"])
        self.assertEqual(receipt["database_checks"], 6)
        self.assertEqual(backup.protected(latest / "state", True),
                         backup.protected(self.state))
        self.assertEqual(set(manifest["files"]), backup.ALLOWED)
        self.assertEqual(json.loads((latest / "config/devices.json").read_text())["revision"], 52)

    def test_live_permission_change_rejects_snapshot(self):
        original = backup.shutil.copyfile
        changed = False
        def copy(*args, **kwargs):
            nonlocal changed
            result = original(*args, **kwargs)
            if not changed:
                changed = True
                with contextlib.closing(sqlite3.connect(self.state / "web-credentials.sqlite")) as db, db:
                    db.execute("UPDATE payload SET value='revoked during backup'")
            return result
        with mock.patch.object(backup.shutil, "copyfile", side_effect=copy):
            with self.assertRaisesRegex(backup.BackupError, "changed during backup"):
                backup.build_snapshot(self.root / "changed", self.state, self.static, {})

    def test_bad_hash_preserves_previous_latest(self):
        path, manifest, archive = self.snapshot()
        first = self.receive(archive)
        (path / "state/web-controller-id").write_bytes(b"tampered")
        bad = self.root / "bad.tar.gz"
        backup.pack_snapshot(path, bad)
        with self.assertRaisesRegex(backup.BackupError, "size differs|hash mismatch"):
            self.receive(bad)
        self.assertEqual((self.root / "receiver/latest").resolve().name, first["snapshot_id"])
        self.assertTrue(list((self.root / "receiver/failures").glob("*.json")))

    def test_tar_traversal_rejected_without_escape(self):
        path, manifest, archive = self.snapshot()
        payload = io.BytesIO()
        with tarfile.open(fileobj=payload, mode="w:gz") as tf:
            tf.add(path / "manifest.json", arcname="manifest.json")
            item = tarfile.TarInfo("../escaped")
            item.size = 3
            tf.addfile(item, io.BytesIO(b"bad"))
        payload.seek(0)
        with self.assertRaisesRegex(backup.BackupError, "unsafe"):
            backup.receive(payload, self.root / "receiver", lambda: None)
        self.assertFalse((self.root / "escaped").exists())

    def test_active_ngr_refuses_reception(self):
        _, _, archive = self.snapshot()
        def active():
            raise backup.BackupError("NGR HomeSense is not inactive")
        with open(archive, "rb") as stream:
            with self.assertRaisesRegex(backup.BackupError, "not inactive"):
                backup.receive(stream, self.root / "receiver", active)
        self.assertFalse((self.root / "receiver/latest").exists())

    def test_retention_keeps_24_hourly_and_seven_daily(self):
        now = dt.datetime(2026, 10, 24, 9, tzinfo=dt.timezone.utc)
        entries = [(str(i), now - dt.timedelta(hours=i)) for i in range(24 * 10)]
        keep = backup.retention_ids(entries)
        self.assertTrue({str(i) for i in range(24)} <= keep)
        self.assertLessEqual(len(keep), 31)
        daily = {}
        for name, when in entries:
            daily.setdefault(when.astimezone(backup.ZoneInfo("Asia/Shanghai")).date(), name)
        self.assertTrue(set(list(daily.values())[:7]) <= keep)
        self.assertNotIn("239", keep)

    def test_online_backup_includes_committed_wal(self):
        with contextlib.closing(sqlite3.connect(self.state / "automation-tcp.sqlite")) as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("UPDATE configuration_revision SET payload=?",
                       (json.dumps({"revision": 52, "stamp": "committed-in-WAL"}),))
            db.commit()
            self.assertTrue((self.state / "automation-tcp.sqlite-wal").exists())
            path, _, archive = self.snapshot()
            self.receive(archive)
            self.assertEqual(backup.bundle(path / "state", True)[2]["stamp"],
                             "committed-in-WAL")

    def test_cold_guard_rejects_enabled_or_primary_role(self):
        with mock.patch.object(backup, "service_state", return_value={
                "ActiveState": "inactive", "MainPID": "0", "UnitFileState": "enabled"}):
            with self.assertRaisesRegex(backup.BackupError, "not disabled"):
                backup.cold_check()
        with mock.patch.object(backup, "service_state", return_value={
                "ActiveState": "inactive", "MainPID": "0", "UnitFileState": "disabled"}):
            with mock.patch.object(Path, "exists", return_value=True):
                with self.assertRaisesRegex(backup.BackupError, "role gate is open"):
                    backup.cold_check()

    def test_restricted_key_cannot_run_arbitrary_command(self):
        env = dict(os.environ, SSH_ORIGINAL_COMMAND="uname -a")
        r = subprocess.run([sys.executable, str(Path(backup.__file__)), "receive"],
                           env=env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("permits only receive-v1", r.stderr)


if __name__ == "__main__":
    unittest.main()

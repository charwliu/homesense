#!/usr/bin/env python3
"""Operational online SQLite backup transport. Never starts/stops HomeSense."""
from __future__ import annotations
import contextlib, datetime as dt, fcntl, hashlib, json, os, pathlib, re
import shutil, sqlite3, subprocess, sys, tarfile, tempfile, uuid
from zoneinfo import ZoneInfo

VERSION = 1
DATABASES = (
    "automation-tcp.sqlite", "web-credentials.sqlite", "web-drafts.sqlite",
    "web-receipts.sqlite", "hap-bridges/study-e/identity/identity.sqlite",
    "hap-bridges/house-extension/identity/identity.sqlite",
)
STATIC = (
    "state/web-controller-id", "config/hap-plan.json",
    "tls/server-chain.pem", "tls/server-private.pem", "secrets/bootstrap-token",
)
ALLOWED = frozenset(["state/" + x for x in DATABASES] + list(STATIC) +
                    ["config/devices.json", "config/automation.json"])
SNAPSHOT_RE = re.compile(r"^\d{8}T\d{6}Z-[a-f0-9]{12}$")
MAX_FILE = 2 * 1024**3
MAX_TOTAL = 4 * 1024**3


class BackupError(RuntimeError):
    pass


def require(ok, message):
    if not ok:
        raise BackupError(message)


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True,
                      separators=(",", ":")).encode()


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for data in iter(lambda: f.read(1024 * 1024), b""):
            h.update(data)
    return h.hexdigest()


def write_json(path, value):
    path = pathlib.Path(path)
    tmp = path.with_name(path.name + ".new")
    with open(tmp, "w", encoding="utf8") as f:
        json.dump(value, f, ensure_ascii=False, indent=2)
        f.write("\n")
        f.flush()
        os.fsync(f.fileno())
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)


@contextlib.contextmanager
def database(path, immutable=False):
    uri = pathlib.Path(path).resolve().as_uri() + "?mode=ro"
    if immutable:
        uri += "&immutable=1"
    with contextlib.closing(sqlite3.connect(uri, uri=True, timeout=5)) as db:
        yield db


def bundle(state, immutable=False):
    with database(pathlib.Path(state) / DATABASES[0], immutable) as db:
        revision = db.execute("SELECT revision FROM active_configuration").fetchone()[0]
        row = db.execute(
            "SELECT c.payload,a.payload FROM configuration_revision c "
            "JOIN automation_revision a ON c.revision=a.revision "
            "WHERE c.revision=?", (revision,)).fetchone()
        require(row is not None, "active configuration bundle incomplete")
        return (revision, db.execute("PRAGMA user_version").fetchone()[0],
                json.loads(row[0]), json.loads(row[1]))


def protected(state, immutable=False):
    state = pathlib.Path(state)
    revision, schema, devices, automation = bundle(state, immutable)
    result = {"revision": revision, "schema": schema,
              "devices": hashlib.sha256(canonical(devices)).hexdigest(),
              "automation": hashlib.sha256(canonical(automation)).hexdigest()}
    with database(state / "web-credentials.sqlite", immutable) as db:
        result["credentials"] = hashlib.sha256(
            "\n".join(db.iterdump()).encode()).hexdigest()
    for bridge in ("study-e", "house-extension"):
        with database(state / "hap-bridges" / bridge / "identity/identity.sqlite",
                      immutable) as db:
            # Include host and port: periodic backups never migrate identities.
            entries = [(k, bytes(v).hex()) for k, v in db.execute(
                "SELECT key,value FROM entries ORDER BY key")]
            aids = db.execute("SELECT device,aid FROM accessory_ids ORDER BY device").fetchall()
            iids = db.execute(
                "SELECT aid,resource,iid FROM instance_ids ORDER BY aid,resource").fetchall()
            result[bridge] = hashlib.sha256(canonical([entries, aids, iids])).hexdigest()
    return result


def service_state():
    output = subprocess.check_output(
        ["systemctl", "show", "homesense-e2.service", "-p", "ActiveState",
         "-p", "MainPID", "-p", "UnitFileState"], text=True)
    return dict(line.split("=", 1) for line in output.splitlines() if "=" in line)


def cold_check():
    info = service_state()
    require(info.get("ActiveState") == "inactive" and info.get("MainPID") == "0",
            "NGR HomeSense is not inactive; refuse backup reception")
    require(info.get("UnitFileState") in ("disabled", "masked"),
            "NGR HomeSense is not disabled")
    require(not pathlib.Path("/var/lib/homesense-role/primary").exists(),
            "NGR primary role gate is open")


def live_inputs(root):
    root = pathlib.Path(root).resolve()
    info = service_state()
    require(info.get("ActiveState") == "active" and info.get("MainPID") not in ("0", None),
            "source HomeSense is not active")
    require(pathlib.Path("/var/lib/homesense-role/primary").exists(),
            "source primary role gate is closed")
    pid = info["MainPID"]
    proc = pathlib.Path("/proc") / pid
    args = proc.joinpath("cmdline").read_bytes().decode().rstrip("\0").split("\0")
    require(len(args) >= 6 and args[1] == "serve-automation-tcp",
            "unexpected production command")
    def inside(value):
        path = pathlib.Path(value).resolve()
        require(path.is_relative_to(root), "runtime input outside approved service root")
        return path
    state = inside(args[4])
    files = {"state/web-controller-id": state / "web-controller-id"}
    for flag, name in (
        ("--hap-plan", "config/hap-plan.json"),
        ("--web-cert", "tls/server-chain.pem"),
        ("--web-key", "tls/server-private.pem"),
        ("--web-token", "secrets/bootstrap-token"),
    ):
        require(args.count(flag) == 1, "missing or repeated runtime file flag")
        files[name] = inside(args[args.index(flag) + 1])
    require(files["tls/server-private.pem"].stat().st_mode & 0o077 == 0,
            "source TLS key permissions are not private")
    identity = {
        "pid": pid, "start_ticks": proc.joinpath("stat").read_text().rsplit(")", 1)[1].split()[19],
        "argv_sha256": hashlib.sha256(canonical(args)).hexdigest(),
        "executable_sha256": digest(proc / "exe"),
        "static_sha256": {name: digest(path) for name, path in files.items()},
    }
    return state, files, identity


def build_snapshot(destination, state, static, source):
    """Each DB uses SQLite online backup; critical cross-DB metadata is fenced."""
    destination = pathlib.Path(destination)
    destination.mkdir(mode=0o700)
    before = protected(state)
    for name in DATABASES:
        out = destination / "state" / name
        out.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        with database(pathlib.Path(state) / name) as src:
            with contextlib.closing(sqlite3.connect(out)) as dst:
                src.backup(dst, pages=256, sleep=0.05)
        os.chmod(out, 0o600)
        with database(out, True) as check:
            require(check.execute("PRAGMA quick_check").fetchall() == [("ok",)],
                    "snapshot SQLite integrity failure: " + name)
    for name, original in static.items():
        require(name in STATIC, "unapproved static file")
        out = destination / name
        out.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        shutil.copyfile(original, out)
        os.chmod(out, 0o600)
        require(digest(original) == digest(out), "static file changed during copy")
    revision, _, devices, automation = bundle(destination / "state", True)
    for name, value in (("devices", devices), ("automation", automation)):
        write_json(destination / "config" / (name + ".json"), value)
    snap = protected(destination / "state", True)
    after = protected(state)
    require(before == snap == after,
            "configuration, pairing or permissions changed during backup; keep failure")
    files = {}
    for name in sorted(ALLOWED):
        path = destination / name
        require(path.is_file() and not path.is_symlink(), "missing runtime file " + name)
        files[name] = {"size": path.stat().st_size, "sha256": digest(path)}
    require(all(row["size"] <= MAX_FILE for row in files.values()) and
            sum(row["size"] for row in files.values()) <= MAX_TOTAL,
            "backup exceeds safety size limit")
    now = dt.datetime.now(dt.timezone.utc)
    snapshot_id = now.strftime("%Y%m%dT%H%M%SZ-") + uuid.uuid4().hex[:12]
    manifest = {"version": VERSION, "snapshot_id": snapshot_id,
                "created_at": now.isoformat(), "revision": revision,
                "source": source, "protected": snap, "files": files}
    write_json(destination / "manifest.json", manifest)
    return manifest


def pack_snapshot(snapshot, archive):
    snapshot = pathlib.Path(snapshot)
    with tarfile.open(archive, "w:gz", compresslevel=3) as tf:
        tf.add(snapshot / "manifest.json", arcname="manifest.json", recursive=False)
        for name in sorted(ALLOWED):
            tf.add(snapshot / name, arcname=name, recursive=False)


def validate_manifest(manifest):
    require(manifest.get("version") == VERSION, "unsupported backup protocol")
    require(isinstance(manifest.get("snapshot_id"), str) and
            SNAPSHOT_RE.fullmatch(manifest["snapshot_id"]), "invalid snapshot id")
    date = dt.datetime.fromisoformat(manifest["created_at"])
    require(date.tzinfo is not None, "snapshot time must include timezone")
    files = manifest["files"]
    require(set(files) == ALLOWED, "runtime file allowlist mismatch")
    for name, row in files.items():
        require(type(row["size"]) is int and 0 <= row["size"] <= MAX_FILE,
                "invalid file size")
        require(isinstance(row["sha256"], str) and
                re.fullmatch("[0-9a-f]{64}", row["sha256"]), "invalid file hash")
    require(sum(row["size"] for row in files.values()) <= MAX_TOTAL,
            "total backup size limit exceeded")


def retention_ids(snapshots):
    ordered = sorted(snapshots, key=lambda x: x[1], reverse=True)
    keep = {name for name, _ in ordered[:24]}
    days = {}
    for name, when in ordered:
        days.setdefault(when.astimezone(ZoneInfo("Asia/Shanghai")).date(), name)
    keep.update(list(days.values())[:7])
    return keep


def receive(stream, root, check_cold=cold_check):
    """Receive exact runtime allowlist; no shell, restore or service-start path."""
    root = pathlib.Path(root)
    check_cold()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    with open(root / ".receive.lock", "a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        snapshots = root / "snapshots"
        snapshots.mkdir(mode=0o700, exist_ok=True)
        stage = pathlib.Path(tempfile.mkdtemp(prefix=".incoming-", dir=root))
        try:
            with tarfile.open(fileobj=stream, mode="r|gz") as tf:
                members = iter(tf)
                first = next(members)
                require(first.name == "manifest.json" and first.isfile() and
                        first.size <= 1024 * 1024, "manifest must be first regular file")
                manifest_bytes = tf.extractfile(first).read()
                manifest = json.loads(manifest_bytes)
                validate_manifest(manifest)
                require(shutil.disk_usage(root).free >
                        sum(row["size"] for row in manifest["files"].values()) + 256*1024**2,
                        "insufficient backup disk space")
                seen = set()
                for member in members:
                    name = member.name
                    require(name in ALLOWED and name not in seen and member.isfile(),
                            "unsafe, duplicate or non-regular tar member")
                    seen.add(name)
                    row = manifest["files"][name]
                    require(member.size == row["size"], "tar size differs from manifest")
                    out = stage / name
                    out.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                    with open(out, "xb") as f:
                        shutil.copyfileobj(tf.extractfile(member), f, 1024*1024)
                        f.flush()
                        os.fsync(f.fileno())
                    os.chmod(out, 0o600)
                    require(out.stat().st_size == row["size"] and digest(out) == row["sha256"],
                            "received file hash mismatch")
                require(seen == ALLOWED, "incomplete backup stream")
            for name in DATABASES:
                with database(stage / "state" / name, True) as db:
                    require(db.execute("PRAGMA quick_check").fetchall() == [("ok",)],
                            "received SQLite integrity failure")
            require(protected(stage / "state", True) == manifest["protected"],
                    "received protected metadata mismatch")
            _, _, devices, automation = bundle(stage / "state", True)
            for name, value in (("devices", devices), ("automation", automation)):
                require(json.loads((stage / "config" / (name + ".json")).read_text()) == value,
                        "exported configuration differs from backed-up DB")
            check_cold()
            write_json(stage / "manifest.json", manifest)
            final = snapshots / manifest["snapshot_id"]
            require(not final.exists(), "snapshot id already exists")
            os.rename(stage, final)
            stage = None
            latest_tmp = root / (".latest-" + uuid.uuid4().hex)
            latest_tmp.symlink_to("snapshots/" + manifest["snapshot_id"])
            os.replace(latest_tmp, root / "latest")
            rows = []
            for p in snapshots.iterdir():
                if not p.is_dir() or p.is_symlink() or not SNAPSHOT_RE.fullmatch(p.name):
                    continue
                saved = json.loads((p / "manifest.json").read_text())
                rows.append((p.name, dt.datetime.fromisoformat(saved["created_at"])))
            keep = retention_ids(rows)
            removed = []
            for name, _ in rows:
                if name not in keep:
                    shutil.rmtree(snapshots / name)
                    removed.append(name)
            receipt = {"version": VERSION, "snapshot_id": manifest["snapshot_id"],
                       "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
                       "revision": manifest["revision"], "database_checks": len(DATABASES),
                       "received_at": dt.datetime.now(dt.timezone.utc).isoformat(),
                       "retained": len(rows) - len(removed), "removed": removed,
                       "NGR_service": "inactive, disabled, role gate closed"}
            write_json(root / "last-success.json", receipt)
            return receipt
        except BaseException as e:
            failures = root / "failures"
            failures.mkdir(mode=0o700, exist_ok=True)
            write_json(failures / (dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
                                   + "-" + uuid.uuid4().hex[:8] + ".json"),
                       {"error": type(e).__name__ + ": " + str(e)})
            raise
        finally:
            if stage is not None:
                shutil.rmtree(stage)


def send(config):
    work = pathlib.Path(config["work"])
    work.mkdir(parents=True, exist_ok=True, mode=0o700)
    with open(work / ".send.lock", "a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        stage = pathlib.Path(tempfile.mkdtemp(prefix=".work-", dir=work))
        try:
            state, static, identity = live_inputs(config["service_root"])
            manifest = build_snapshot(stage / "snapshot", state, static, identity)
            require(live_inputs(config["service_root"])[2] == identity,
                    "runtime executable or static inputs changed during backup")
            archive = stage / "snapshot.tar.gz"
            pack_snapshot(stage / "snapshot", archive)
            manifest_sha = digest(stage / "snapshot/manifest.json")
            # All transport arguments are root-owned configuration, not shell input.
            with open(archive, "rb") as stream:
                result = subprocess.run(config["ssh_argv"], stdin=stream,
                                        capture_output=True, timeout=300)
            require(result.returncode == 0,
                    "backup SSH receiver failed: " + result.stderr.decode(errors="replace")[-2000:])
            receipt = json.loads(result.stdout)
            require(receipt["snapshot_id"] == manifest["snapshot_id"] and
                    receipt["manifest_sha256"] == manifest_sha and
                    receipt["database_checks"] == len(DATABASES),
                    "receiver receipt differs from sent snapshot")
            receipt["archive_bytes"] = archive.stat().st_size
            receipt["archive_sha256"] = digest(archive)
            write_json(work / "last-success.json", receipt)
            print(json.dumps(receipt))
        except BaseException as e:
            failures = work / "failures"
            failures.mkdir(mode=0o700, exist_ok=True)
            write_json(failures / (dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
                                   + "-" + uuid.uuid4().hex[:8] + ".json"),
                       {"error": type(e).__name__ + ": " + str(e)})
            raise
        finally:
            shutil.rmtree(stage)


def main():
    os.umask(0o077)
    require(len(sys.argv) == 2 and sys.argv[1] in ("send", "receive"), "invalid operation")
    if sys.argv[1] == "receive":
        require(os.environ.get("SSH_ORIGINAL_COMMAND") == "receive-v1",
                "restricted backup key permits only receive-v1")
        config = json.loads(pathlib.Path("/etc/homesense-backup-receiver.json").read_text())
        print(json.dumps(receive(sys.stdin.buffer, config["root"])))
    else:
        config = json.loads(pathlib.Path("/etc/homesense-backup.json").read_text())
        send(config)


if __name__ == "__main__":
    try:
        main()
    except (Exception, KeyboardInterrupt) as error:
        print("HomeSense backup failed: " + type(error).__name__ + ": " + str(error),
              file=sys.stderr)
        sys.exit(1)

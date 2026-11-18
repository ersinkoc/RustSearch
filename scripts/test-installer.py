"""Exercise the real PowerShell installer in isolated paths, without Windows integration."""

import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unittest
import zipfile

ROOT = Path(__file__).resolve().parent.parent
SHELL = os.environ.get("RUSTSEARCH_TEST_POWERSHELL", "powershell.exe")


def digest(data):
    return hashlib.sha256(data).hexdigest()


class InstallerTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".temp_files"
        scratch.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="installer-", dir=scratch)
        self.root = Path(self.temp.name).resolve()
        self.app = self.root / "Programs with spaces" / "RustSearch"
        self.data = self.root / "Saved data"
        self.data.mkdir()
        self.profile = {
            "background": False, "paused": True, "auto_index": False,
            "start_at_login": False, "sizes": False, "mode": "Accessible",
            "roots": [], "catalog": None,
        }
        self.profile_path = self.data / "background.json"
        self.profile_path.write_text(json.dumps(self.profile), encoding="utf-8")
        (self.data / "preserve.rsidx").write_bytes(b"owned test data: never remove")
        self.archive = self.package()

    def tearDown(self):
        endpoint = self.data / "endpoint.json"
        if endpoint.exists():
            try:
                self.stop_worker()
            except (OSError, EOFError):
                pass
        self.assertTrue(self.root.is_relative_to((ROOT / ".temp_files").resolve()))
        self.temp.cleanup()

    def package(self, *, build=2, extra=None, corrupt_internal=False, broken_worker=False):
        files = {name: (ROOT / name).read_bytes() for name in ("install.ps1", "uninstall.ps1")}
        files.update({name: (ROOT / "target" / "release" / name).read_bytes()
                      for name in ("rustsearch.exe", "rustsearch-desktop.exe")})
        if broken_worker:
            files["rustsearch-desktop.exe"] = b"not an executable"
        files["build-info.json"] = json.dumps({
            "version": "0.0.1", "build": build, "target": "x86_64-pc-windows-msvc", "commit": "a" * 40,
        }).encode()
        files["SHA256SUMS.txt"] = "".join(f"{digest(value)}  {name}\n" for name, value in files.items()).encode()
        if corrupt_internal:
            files["rustsearch.exe"] += b"tampering"
        if extra:
            files.update(extra)
        folder = self.root / f"package-{time.time_ns()}"
        folder.mkdir()
        archive = folder / f"rustsearch-0.0.1-build.{build}-windows-x64.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zipped:
            for name, value in files.items():
                zipped.writestr(f"rustsearch-0.0.1-build.{build}-windows-x64/{name}", value)
        (folder / "SHA256SUMS.txt").write_text(f"{digest(archive.read_bytes())}  {archive.name}\n")
        return archive

    def run_installer(self, *options, archive=None, data=None, success=True):
        result = subprocess.run([
            SHELL, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
            "-File", str(ROOT / "install.ps1"), "-InstallDirectory", str(self.app),
            "-DataDirectory", str(data or self.data), "-NoIntegration", "-NoLaunch",
            "-Archive", str(archive or self.archive), *options,
        ], capture_output=True, text=True, timeout=100)
        output = result.stdout + result.stderr
        if success:
            self.assertEqual(result.returncode, 0, output)
        else:
            self.assertNotEqual(result.returncode, 0, output)
        return output

    def stop_worker(self):
        info = json.loads((self.data / "endpoint.json").read_bytes())
        with socket.create_connection(("127.0.0.1", info["port"]), timeout=5) as connection:
            def send(value):
                encoded = json.dumps(value).encode()
                connection.sendall(struct.pack("<I", len(encoded)) + encoded)

            def read(count):
                result = b""
                while len(result) < count:
                    chunk = connection.recv(count - len(result))
                    if not chunk:
                        raise EOFError()
                    result += chunk
                return result

            def receive():
                return json.loads(read(struct.unpack("<I", read(4))[0]))

            send({"token": info["token"]})
            self.assertIs(receive(), True)
            send("Stop")
            self.assertFalse(receive()["Runtime"]["running"])
        deadline = time.monotonic() + 15
        while (self.data / "endpoint.json").exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertFalse((self.data / "endpoint.json").exists())

    def test_install_upgrade_uninstall_preserves_data(self):
        before = self.profile_path.read_bytes()
        self.run_installer()
        self.assertEqual(json.loads((self.app / "build-info.json").read_bytes())["build"], 2)
        self.run_installer(archive=self.package(build=3))
        self.assertEqual(json.loads((self.app / "build-info.json").read_bytes())["build"], 3)
        self.assertEqual(self.profile_path.read_bytes(), before)
        result = subprocess.run([
            SHELL, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
            "-File", str(self.app / "uninstall.ps1"),
        ], capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.app.exists())
        self.assertEqual(self.profile_path.read_bytes(), before)
        self.assertEqual((self.data / "preserve.rsidx").read_bytes(), b"owned test data: never remove")

    def test_rejects_downgrade_without_changing_install(self):
        self.run_installer(archive=self.package(build=3))
        output = self.run_installer(success=False)
        self.assertIn("downgrade", output)
        self.assertEqual(json.loads((self.app / "build-info.json").read_bytes())["build"], 3)

    def test_rejects_corrupt_archive_before_stopping_worker(self):
        self.archive.write_bytes(self.archive.read_bytes() + b"tampering")
        self.assertIn("SHA-256 mismatch", self.run_installer(success=False))
        self.assertFalse(self.app.exists())

    def test_rejects_internal_hash_mismatch(self):
        self.assertIn("SHA-256 mismatch", self.run_installer(archive=self.package(corrupt_internal=True), success=False))
        self.assertFalse(self.app.exists())

    def test_rejects_zip_traversal(self):
        archive = self.package(extra={"../../escape.txt": b"bad"})
        self.assertIn("Unsafe ZIP entry", self.run_installer(archive=archive, success=False))
        self.assertFalse((self.root / "escape.txt").exists())
        self.assertFalse(self.app.exists())

    def test_refuses_unowned_target(self):
        self.app.mkdir(parents=True)
        sentinel = self.app / "user-file.txt"
        sentinel.write_text("preserve")
        self.run_installer(success=False)
        self.assertEqual(sentinel.read_text(), "preserve")

    def test_rejects_data_inside_install_directory(self):
        output = self.run_installer(data=self.app / "indexes", success=False)
        self.assertIn("separate", output)
        self.assertFalse(self.app.exists())

    def test_worker_migrates_and_update_failure_rolls_back(self):
        self.profile["background"] = True
        self.profile_path.write_text(json.dumps(self.profile))
        self.run_installer()
        process = subprocess.Popen([str(self.app / "rustsearch-desktop.exe"), "--worker", "--data-dir", str(self.data)])
        try:
            deadline = time.monotonic() + 10
            while not (self.data / "endpoint.json").exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue((self.data / "endpoint.json").exists())
            self.run_installer(archive=self.package(build=3))
            process.wait(timeout=5)
            self.assertEqual(json.loads((self.app / "build-info.json").read_bytes())["build"], 3)
            output = self.run_installer(archive=self.package(build=4, broken_worker=True), success=False)
            self.assertTrue("valid" in output.lower() or "process" in output.lower(), output)
            self.assertEqual(json.loads((self.app / "build-info.json").read_bytes())["build"], 3)
            self.assertTrue((self.data / "endpoint.json").exists())
            self.stop_worker()
        finally:
            process.wait(timeout=10)

    def test_stop_drains_queued_progress_before_acknowledgement(self):
        self.profile["background"] = True
        self.profile_path.write_text(json.dumps(self.profile))
        self.run_installer()
        process = subprocess.Popen([str(self.app / "rustsearch-desktop.exe"), "--worker", "--data-dir", str(self.data)])
        endpoint = self.data / "endpoint.json"
        deadline = time.monotonic() + 10
        while not endpoint.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        original = json.loads(endpoint.read_bytes())
        failures = []
        with socket.create_server(("127.0.0.1", 0)) as listener:
            listener.settimeout(15)

            def queued_progress_proxy():
                try:
                    with listener.accept()[0] as installer, socket.create_connection(("127.0.0.1", original["port"]), timeout=5) as worker:
                        installer.settimeout(15)

                        def frame(connection):
                            def read(count):
                                data = b""
                                while len(data) < count:
                                    part = connection.recv(count - len(data))
                                    if not part:
                                        raise EOFError()
                                    data += part
                                return data
                            header = read(4)
                            return header + read(struct.unpack("<I", header)[0])

                        worker.sendall(frame(installer))
                        installer.sendall(frame(worker))
                        stop_request = frame(installer)
                        self.assertEqual(json.loads(stop_request[4:]), "Stop")
                        progress = json.dumps({"Progress": {"stage": "Monitoring changes"}}).encode()
                        installer.sendall(struct.pack("<I", len(progress)) + progress)
                        worker.sendall(stop_request)
                        while True:
                            reply = frame(worker)
                            installer.sendall(reply)
                            if json.loads(reply[4:]).get("Runtime", {}).get("running") is False:
                                break
                except Exception as error:
                    failures.append(error)

            thread = threading.Thread(target=queued_progress_proxy)
            thread.start()
            endpoint.write_text(json.dumps({**original, "port": listener.getsockname()[1]}))
            try:
                self.run_installer(archive=self.package(build=3))
                process.wait(timeout=10)
            finally:
                thread.join(timeout=20)
            self.assertFalse(thread.is_alive())
            self.assertEqual(failures, [])
        self.stop_worker()


if __name__ == "__main__":
    unittest.main(verbosity=2)

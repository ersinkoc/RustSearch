"""Package already-built Windows binaries and their third-party notices (Python 3.11+)."""

import argparse
import base64
import concurrent.futures
import functools
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tomllib
import urllib.parse
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parent.parent
LICENSE_NAME = re.compile(r"^(licen[sc]e|copying|copyright|notice|unlicense|patents)([._-].*)?$", re.I)


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True, encoding="utf-8").strip()


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "RustSearch-release", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read()


@functools.lru_cache(maxsize=None)
def upstream_license(repository, revision):
    parsed = urllib.parse.urlparse(repository)
    parts = parsed.path.strip("/").removesuffix(".git").split("/")
    if parsed.hostname == "github.com" and len(parts) >= 2:
        repository = "/".join(parts[:2])
        url = f"https://api.github.com/repos/{repository}/license?ref={urllib.parse.quote(revision)}"
        result = json.loads(fetch(url))
        return base64.b64decode(result["content"]), result["html_url"]
    if parsed.hostname == "gitlab.redox-os.org":
        url = f"https://gitlab.redox-os.org/{'/'.join(parts[:2])}/-/raw/{revision}/LICENSE"
        return fetch(url), url
    raise ValueError(f"No pinned license source for {repository}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New output directory; existing packages are never overwritten")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    if command("git", "status", "--porcelain", "--untracked-files=normal"):
        raise SystemExit("Commit the reviewed sources before packaging release provenance.")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    name = f"rustsearch-{version}-windows-x64"
    stage = output / name
    stage.mkdir()
    for filename in ("rustsearch.exe", "rustsearch-desktop.exe"):
        source = ROOT / "target" / "release" / filename
        with source.open("rb") as file:
            file.seek(0x3C)
            offset = struct.unpack("<I", file.read(4))[0]
            file.seek(offset)
            if file.read(6) != b"PE\0\0\x64\x86":
                raise ValueError(f"Expected a Windows AMD64 executable: {filename}")
        shutil.copy2(source, stage / filename)
    if command(str(stage / "rustsearch.exe"), "--version") != f"rustsearch {version}":
        raise ValueError("CLI version does not match the workspace")
    shutil.copy2(ROOT / "docs" / "PORTABLE.md", stage / "README.md")
    shutil.copy2(ROOT / "docs" / "releases" / f"v{version}.md", stage / "RELEASE_NOTES.md")
    metadata = json.loads(command("cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"))
    external = sorted((p for p in metadata["packages"] if p["id"] not in metadata["workspace_members"]), key=lambda p: (p["name"], p["version"]))

    def notices(package):
        directory = Path(package["manifest_path"]).parent
        destination = stage / "licenses" / f"{package['name']}-{package['version']}"
        destination.mkdir(parents=True)
        files = [p for p in directory.iterdir() if p.is_file() and LICENSE_NAME.match(p.name)]
        if package["license_file"]:
            files.append(directory / package["license_file"])
        for folder in (directory / "LICENSES", directory / "licenses"):
            if folder.is_dir():
                files.extend(p for p in folder.rglob("*") if p.is_file() and not p.is_symlink())
        for file in set(files):
            relative = file.relative_to(directory)
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(file, target)
        source_url = package["repository"] or package["homepage"] or f"https://crates.io/crates/{package['name']}/{package['version']}"
        if not files and package["license"] != "CC0-1.0":
            vcs = json.loads((directory / ".cargo_vcs_info.json").read_text())
            content, source_url = upstream_license(source_url, vcs["git"]["sha1"])
            (destination / "LICENSE-upstream.txt").write_bytes(content)
        return {"name": package["name"], "version": package["version"], "license": package["license"], "authors": package["authors"], "source": source_url}

    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        inventory = list(pool.map(notices, external))
    lucide, lucide_url = upstream_license("https://github.com/lucide-icons/lucide", "main")
    (stage / "licenses" / "Lucide-LICENSE.txt").write_bytes(lucide)
    lines = ["# Third-party notices", "", "Conservative Cargo.lock inventory for the Windows workspace, including build and test tooling. License files are under licenses/. Lucide icon notices are included separately for the embedded GPUI Component icons.", "", f"Lucide license source: {lucide_url}", ""]
    for p in inventory:
        lines.extend([f"## {p['name']} {p['version']}", "", f"Declared license: {p['license']}", "", f"Source: {p['source']}", "", "Authors: " + ("; ".join(p["authors"]) or "See the upstream license/source notices."), ""])
    (stage / "THIRD_PARTY_NOTICES.md").write_text("\n".join(lines), encoding="utf-8")
    (stage / "dependency-inventory.json").write_text(json.dumps(inventory, indent=2), encoding="utf-8")
    info = {"version": version, "commit": command("git", "rev-parse", "HEAD"), "target": "x86_64-pc-windows-msvc", "rustc": command("rustc", "--version"), "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest()}
    (stage / "build-info.json").write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")
    hashes = [f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.relative_to(stage).as_posix()}" for p in sorted(stage.rglob("*")) if p.is_file()]
    (stage / "SHA256SUMS.txt").write_text("\n".join(hashes) + "\n", encoding="utf-8")
    archive = output / f"{name}.zip"
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zip_file:
        for file in sorted(stage.rglob("*")):
            if file.is_file():
                zip_file.write(file, file.relative_to(output))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / "SHA256SUMS.txt").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    print(json.dumps({"archive": str(archive), "sha256": digest, "commit": info["commit"], "dependency_notices": len(inventory)}, indent=2))


if __name__ == "__main__":
    main()

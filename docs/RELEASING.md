# Windows portable release

Keep the workspace, executable resource, CLI version, release notes, and portable guide on the same version. Version 0.0.1 is a Windows x64 alpha. A release does not establish Everything parity, full-machine coverage, signing, or installer acceptance.

## Validate and build

Use Windows x64 with the MSVC Rust toolchain and C++ build tools. Stop any indexer running from `target/release` before replacing that executable; retain its profile and restart it after the build.

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo test --workspace --release --locked
cargo build --workspace --release --bins --examples --locked
```

Run the GUI with isolated data and fixture source directories that are **outside** the data directory. Verify native window controls, search, background updates after close, pause/resume, and reconnect. Record exact acceptance and known limits in `docs/DESKTOP_RESULTS.md`. Confirm the GUI file/product version and English resource metadata.

Review and commit the final source and documentation. Packaging requires a clean working tree so `build-info.json` identifies a reviewable source commit. The packaging script requires Python 3.11+, an authenticated GitHub CLI, and network access to retrieve omitted upstream license texts from pinned Cargo source revisions; no fetched code is executed. If a historical revision has no standalone license file, or an old crate has no VCS revision metadata, a current upstream notice is included and explicitly labeled as a fallback. Cargo's conservative notice inventory includes build and test dependencies as well as runtime dependencies.

```powershell
python scripts/package-release.py --output .temp_files/release-0.0.1/package
```

Use a new output directory for each attempt. Existing artifacts are never overwritten or deleted. The ZIP contains the desktop/CLI executables, portable/release notes, dependency attributions and license files, build provenance, and hashes for each file. The adjacent `SHA256SUMS.txt` covers the ZIP. Do not include snapshots, catalogs, worker credentials, user settings, or raw disk metadata.

Extract the ZIP to a new folder, verify every file checksum and provenance commit, and run the extracted CLI on a generated folder. Compare both extracted executable hashes to the validated binaries. This byte-identity check connects GUI acceptance to the distributed executable. Inspect PE dependencies and document external runtime prerequisites. An existing-machine smoke check is separate from clean-machine acceptance.

## Publish

Push the validated source commit, then create and push an annotated `v0.0.1` tag pointing to that exact commit. Never retarget a published release tag. Use the authenticated GitHub CLI to upload the ZIP and checksum with the committed English release notes:

```powershell
gh release create v0.0.1 .temp_files/release-0.0.1/package/rustsearch-0.0.1-windows-x64.zip .temp_files/release-0.0.1/package/SHA256SUMS.txt --repo ersinkoc/RustSearch --verify-tag --prerelease --title "RustSearch 0.0.1 - Windows x64 alpha" --notes-file docs/releases/v0.0.1.md
```

Verify the remote tag's peeled commit, release publication state, and uploaded asset checksums. Download the published ZIP and compare it to the local archive. Report local validation, publication, and any hosted-CI result separately. The current manual release process does not claim a hosted CI build or Authenticode signature.

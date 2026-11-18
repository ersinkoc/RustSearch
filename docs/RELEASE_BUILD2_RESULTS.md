# Windows 0.0.1 build 2 acceptance

This report concerns distribution **v0.0.1-build.2**, with product version
**0.0.1**, from source commit `edd1f9a6880d1febbe88fd122c3768d7437869e5`.
The original public `v0.0.1` tag still resolves to
`cefdc1cac7acfba0e9643a39d366b84de5ba9f30`; its archive was not replaced.

## Local release gate

Verified on Windows 11 x64 on 18 November 2026:

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --release --locked -- -D warnings`: passed.
- `cargo test --workspace --release --locked`: 87 tests passed, none failed/ignored.
- `cargo build --workspace --bins --release --locked`: GUI and CLI passed.
- `python scripts/test-installer.py`: 10 scenarios passed with Windows PowerShell
  5.1, and the same 10 passed with PowerShell 7.
- Python script compilation and `git diff --check`: passed.

The lockfile and production Rust behavior were unchanged during the installer
work. A CI follow-up canonicalizes synthetic live-test fixtures to match the
engine watch roots when TEMP is an alias; all assertions remain intact. A local
TEMP-junction reproduction failed before that correction, and all 38 desktop
tests passed with the same alias afterward. The final installer corrections
were verified in both shells, including a relative checksum path when
PowerShell location differs from the process working directory. A deterministic proxy inserts
an unrelated progress frame before a real worker's stop acknowledgement: the
previous installer failed on a missing `Runtime` property; the final installer
drains the queued frame and completes the update. Other scenarios cover
install/upgrade, the installed uninstaller, saved-data retention, downgrade
rejection, external/internal checksum failures, ZIP traversal, unsafe data
placement, unowned directories, worker migration and activation rollback.

## Downloaded package

The GitHub-uploaded assets were downloaded back and checked independently:

- ZIP: `rustsearch-0.0.1-build.2-windows-x64.zip`, 8,734,873 bytes.
- SHA-256: `338546c8808a8972d43f1259c15f38968b10600b49aee4f07f060e5bbe17952c`.
- External release hashes, all **989** internal file hashes, build commit,
  target/version/build and Cargo.lock hash match.
- The package includes **551** dependency inventory entries and retained notices.
- The external installer/uninstaller match their copies inside the ZIP.
- The downloaded CLI reports `rustsearch 0.0.1`, indexes an isolated directory,
  and finds a Unicode fixture through the persisted cache.

Checksums were also compared with GitHub's uploaded asset digests. They identify
the downloaded bytes; they are not code signing or build attestations.

## Actual installation and saved profile

The downloaded installer migrated the existing portable worker to
`%LOCALAPPDATA%\Programs\RustSearch`, keeping its data in
`%LOCALAPPDATA%\RustSearch`. No previous portable program directory was removed.

Before/after SHA-256 checks show the existing profile, catalog and all six
snapshot/search-cache files are byte-for-byte unchanged, including after the
live-update smoke. Settings retain C:/D:/E:, accessible indexing, size collection
disabled, background indexing enabled, and sign-in startup enabled.

Process inspection shows one worker from the permanent installation and none
from the former Downloads folder. Start menu and desktop shortcuts point to
the new GUI and embed its executable icon. The per-user Installed apps entry
reports version 0.0.1 and points to the installed uninstaller. The HKCU Run entry
points to the new background executable with the retained data directory.
No reboot/sign-in test was performed after this migration.

The installed worker loaded the existing three-source catalog with **22,235,751**
records at the observed publication. This count changes as disk contents change.
Controlled files on all three drives passed:

- Combined search: three matches; each drive filter: one matching source.
- Live file creation on C:, D:, and E:.
- Unicode file renames preserving per-source file identities and removing old names.
- Live deletion on all three drives.

All owned fixture files/directories were removed. These checks passed while the
accessible backend checked offline changes in the background (`recovering=true`).
This is saved-catalog reuse plus synchronization, not proof of protected-path
coverage or completion of the offline-change traversal. This release does not
remove that traversal after worker downtime.

## Publication

Published [0.0.1 build 2](https://github.com/ersinkoc/RustSearch/releases/tag/v0.0.1-build.2)
on 18 November 2026 at 14:47:08 UTC and marked it as the latest release. It is
an early desktop alpha despite the regular GitHub release designation.

The [exact-tag Windows CI run](https://github.com/ersinkoc/RustSearch/actions/runs/44324920465)
passed formatting, strict Clippy, all 87 Rust tests, GUI/CLI release builds,
10 installer scenarios in each PowerShell shell, packaging, and artifact upload.
The [same-commit main run](https://github.com/ersinkoc/RustSearch/actions/runs/44324918735)
also passed. The initial CI fixture failure was reproduced and corrected; no
assertion was removed and no test was skipped to obtain these results.

An anonymous HTTPS download from `releases/latest/download/install.ps1` matches
the published installer hash. Running that script with its default online
release selection installed version 0.0.1 build 2 into an isolated profile;
the installed uninstaller then removed the program and retained its profile and
saved-data sentinel. No GitHub credentials were supplied to this online flow.

The real installation was updated to the final published package, and its worker
passed the three-drive live-update checks again. All **989 installed package
files** match the published per-file manifest. Build provenance and the ownership
marker both record the exact release commit. The installed desktop executable
SHA-256 is `c83c2bc8c149225f29be084b76080b83c7ac9d569b4ffc217b5dab595fc12f8e`.
Saved profile/catalog/base/cache hashes remain unchanged after the final update
and live checks. The GUI was left closed and one background worker remained.

The original release now links to build 2 and warns about checkpoint-format
compatibility; its tag and archive are unchanged. No code-signing certificate
or build attestation was added.

## Limits

The application/scripts remain unsigned, and no automatic updater is included.
Direct3D 11 and the Microsoft Visual C++ x64 runtime are required; missing-runtime
handling was inspected, not exercised by removing this machine's runtime.
Clean-machine GUI use, Windows 10, screen readers, mixed DPI and long-duration
unattended acceptance remain open. The known grid advisory through GPUI remains
disclosed in the release notes. No new idle/resource measurement is claimed by
this installer acceptance run.

Private package/proof artifacts are retained under the ignored `.temp_files/`
directory. Automatic approval review rejected removal of the empty owned
`ci-temp-alias` junction and its empty `ci-temp-physical` target with "blocked by
policy". Both are inside `.temp_files/` and remain; no unsafe cleanup fallback
was attempted. They do not affect the installed application.

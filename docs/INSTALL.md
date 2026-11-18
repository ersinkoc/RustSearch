# Install RustSearch on Windows

The application version remains **0.0.1**. Distribution **build 2** includes the
current runtime fixes and installer; the original `v0.0.1` archive is retained.
Use Windows x64 and 64-bit Windows PowerShell 5.1 or PowerShell 7. Rust and
administrator privileges are not needed for the per-user installation.

## Online installation

Download and run the installer from the [latest release](https://github.com/ersinkoc/RustSearch/releases/latest):

```powershell
Invoke-WebRequest https://github.com/ersinkoc/RustSearch/releases/latest/download/install.ps1 -OutFile install.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

The installer downloads the release ZIP and verifies its SHA-256 checksum and
the package's per-file checksums before stopping an existing worker. Checksum
files identify bytes downloaded over HTTPS; the scripts and executables are
unsigned. You may inspect `install.ps1` before running it and compare its hash
with the release's `SHA256SUMS.txt`. No execution policy is changed permanently.

Install with sign-in startup and a desktop shortcut:

```powershell
.\install.ps1 -StartWithWindows -DesktopShortcut
```

Installation defaults to `%LOCALAPPDATA%\Programs\RustSearch`. The Start menu
shortcut and Windows **Installed apps** entry are created for the current user.
The GUI opens after installation. `-NoLaunch` keeps the window closed; an
existing/enabled background worker is still restarted. Fresh installation starts
automatic accessible-file indexing when the application launches. Protected
folders remain outside normal-user coverage.

The desktop requires Direct3D 11 and the Microsoft Visual C++ x64 runtime.
The installer stops with the [official runtime download](https://aka.ms/vc14/vc_redist.x64.exe)
if `VCRUNTIME140.dll` is missing. It does not silently elevate or install a
system runtime. See [Microsoft's runtime requirements](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist).
The runtime's own installation may require administrator approval.

## Update and portable migration

Run the installer again to update. To select this exact distribution:

```powershell
.\install.ps1 -Release v0.0.1-build.2
```

Settings, selected drives, saved catalogs, snapshots and caches stay in
`%LOCALAPPDATA%\RustSearch`. Existing sign-in startup is redirected to the
permanent installation. The selected profile's worker is stopped through
authenticated IPC and restarted from the installed executable. The installer
does not force-kill other profiles or remove a previous portable folder.
Close any other RustSearch instances using the same executables before updating.

An update stages and validates the package before switching directories. A
failed activation restores the previous installed directory and integration
settings, and restarts the previous worker when available. Older build numbers
are rejected. Existing data is not rolled back or reindexed by the installer.
After worker downtime, the accessible backend still checks offline changes in
the background; **Syncing** can remain visible on large disks.

Both executables must be updated together. The original released binaries cannot
read newer `RSDEL002` checkpointed delta logs. Do not downgrade those binaries
over current saved data.

## Offline installation and custom paths

Download the build 2 ZIP, `install.ps1`, and `SHA256SUMS.txt` from the same release:

```powershell
.\install.ps1 -Archive .\rustsearch-0.0.1-build.2-windows-x64.zip -NoLaunch
```

`-ChecksumFile PATH` selects a manifest elsewhere. `-InstallDirectory PATH` and
`-DataDirectory PATH` accept separate absolute local directories; use the same
paths on subsequent updates. Junctions/symlinks in these paths are rejected.
An existing target must carry this installer's ownership marker. `-NoIntegration`
creates an isolated installation without registry changes or shortcuts; reuse
that option when updating it. No user or machine PATH entry is changed; the CLI
is `rustsearch.exe` in the installation directory.

## Uninstall

Use Windows **Installed apps > RustSearch > Uninstall**, or run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\RustSearch\uninstall.ps1"
```

The uninstaller stops the selected profile's worker, removes installer-owned
program files, matching shortcuts and startup registration, and the Installed
apps entry. **Saved indexes and settings are retained.** Reinstallation can
reuse them. No machine-wide service or automatic updater is installed.

## Release verification

Maintainers run the workspace format, strict Clippy, release tests and binary
build, plus `python scripts/test-installer.py` under PowerShell 5.1 and 7.
The installer suite uses isolated data and installation directories, including
paths with spaces. It exercises upgrades, uninstall/data retention, downgrade
rejection, corrupt downloads, internal checksum failures, ZIP traversal, unowned
directories, real worker migration, and failed-activation rollback.

Package only committed sources with
`python scripts/package-release.py --build 2 --output .temp_files/release-build2`.
Verify published downloads, their source/tag identity, per-file checksums and a
downloaded CLI/worker smoke before announcing a release. Windows CI runs the
same Rust and installer checks; it does not automatically publish assets.

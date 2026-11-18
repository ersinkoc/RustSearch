# RustSearch 0.0.1 — Windows portable alpha

The original public v0.0.1 ZIP predates the overflow-recovery fix in `9bb440b`. Updating the source checkout does not update an extracted portable executable or an already-running worker. Use a current source build for the subsequent saved-catalog and recovery fixes; extracting the original ZIP again restores the old executable. See the [runtime follow-up](RUNTIME_INSTALLATION_RESULTS.md) and [original recovery results](OVERFLOW_RECOVERY_RESULTS.md).

Extract the entire ZIP to a permanent folder, then open `rustsearch-desktop.exe`. No Rust toolchain or administrator installation is needed. The desktop starts accessible-file indexing for ready local fixed drives under your Windows account. Initial scans of large drives can take substantial time; protected folders may be skipped. Use **This computer** to change scope and **Settings** to pause, stop, or configure background indexing.

Current source builds can checkpoint update logs without replacing snapshots or search caches. Checkpointed logs require the updated GUI and CLI; the old released executables cannot read them. Back up saved data before switching back to an older build.

The package is built for Windows x64 and tested on Windows 11. The GUI needs a working Direct3D 11 graphics driver. It imports `VCRUNTIME140.dll`; if Windows reports that DLL missing, install Microsoft's [current x64 Visual C++ v14 Redistributable](https://aka.ms/vc14/vc_redist.x64.exe). See the [official runtime requirements and downloads](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist). Runtime DLLs are not bundled.

Closing the search window keeps the per-user worker running by default. Reopening reconnects to that worker. **Start with Windows** is optional and initially off; it starts only the background worker at sign-in. Settings and indexes live in `%LOCALAPPDATA%\RustSearch` unless `--data-dir PATH` is supplied. A machine-wide Windows service is not installed.

Keep the extracted folder in place while sign-in startup is enabled. Before moving, replacing, or removing the program, turn off **Start with Windows**, stop the indexer in Settings, and close the window. After moving it, launch from the new location and enable startup again if desired. Removing the program folder does not delete saved indexes/settings.

Closing the GUI alone does not stop a background worker. To identify the executable and data directory actually running, use PowerShell:

```powershell
Get-CimInstance Win32_Process -Filter "Name='rustsearch-desktop.exe'" |
    Select-Object ProcessId, ExecutablePath, CommandLine
```

Each data directory has its own worker lock. A development launch with `--data-dir` and a normal portable launch can therefore index the same drives independently. Keep the intended installation/data directory in the Windows startup entry and stop unused workers before comparing behavior or resource usage. Preserve settings and indexes when replacing binaries. Current source builds reuse a valid saved catalog immediately after loading it and check offline changes in background batches. The interface shows **Syncing** during this catch-up. Missing or unreadable saved sources require a fresh scan.

`rustsearch.exe` provides the CLI. Run `rustsearch.exe --help` for commands. Native NTFS enumeration requires the appropriate volume access, usually an elevated process; normal accessible-file indexing does not. Explicit `rustsearch-desktop.exe --index PATH` views are read-only.

This is an unsigned alpha portable build. No installer, automatic updater, tray icon, global activation shortcut, disk-space treemap, folder totals, or content search is included. Current source builds reconcile accessible indexes into deltas after worker downtime or notification loss while preserving their snapshots and search caches. Heavy filesystem churn can keep synchronization active. Filesystem access and missed changes remain visible as partial coverage, progress, or errors. Everything feature parity and speed superiority are not claimed.

The pinned GUI dependency has an [open upstream grid advisory](https://github.com/becheran/grid/security/advisories/GHSA-38c5-483c-4qqp); see the repository's [verification notes](https://github.com/ersinkoc/RustSearch/blob/v0.0.1/docs/DESKTOP_RESULTS.md). Windows 10, clean-machine installation, screen readers, mixed DPI, and long-duration resource behavior have not been accepted by this release.

`build-info.json` records the source commit and toolchain. `SHA256SUMS.txt` inside the ZIP covers its files; the separate release checksum covers the ZIP. These checksums identify bytes and do not constitute code signing. Dependency attributions and license texts are in `THIRD_PARTY_NOTICES.md` and `licenses/`.

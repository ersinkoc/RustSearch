# RustSearch 0.0.1 — Windows portable alpha

Extract the entire ZIP to a permanent folder, then open `rustsearch-desktop.exe`. No Rust toolchain or administrator installation is needed. The desktop starts accessible-file indexing for ready local fixed drives under your Windows account. Initial scans of large drives can take substantial time; protected folders may be skipped. Use **This computer** to change scope and **Settings** to pause, stop, or configure background indexing.

The package is built for Windows x64 and tested on Windows 11. The GUI needs a working Direct3D 11 graphics driver. It imports `VCRUNTIME140.dll`; if Windows reports that DLL missing, install Microsoft's [current x64 Visual C++ v14 Redistributable](https://aka.ms/vc14/vc_redist.x64.exe). See the [official runtime requirements and downloads](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist). Runtime DLLs are not bundled.

Closing the search window keeps the per-user worker running by default. Reopening reconnects to that worker. **Start with Windows** is optional and initially off; it starts only the background worker at sign-in. Settings and indexes live in `%LOCALAPPDATA%\RustSearch` unless `--data-dir PATH` is supplied. A machine-wide Windows service is not installed.

Keep the extracted folder in place while sign-in startup is enabled. Before moving, replacing, or removing the program, turn off **Start with Windows**, stop the indexer in Settings, and close the window. After moving it, launch from the new location and enable startup again if desired. Removing the program folder does not delete saved indexes/settings.

`rustsearch.exe` provides the CLI. Run `rustsearch.exe --help` for commands. Native NTFS enumeration requires the appropriate volume access, usually an elevated process; normal accessible-file indexing does not. Explicit `rustsearch-desktop.exe --index PATH` views are read-only.

This is an unsigned alpha portable build. No installer, automatic updater, tray icon, global activation shortcut, disk-space treemap, folder totals, or content search is included. Accessible indexes rescan after worker downtime or notification loss. Heavy filesystem churn can trigger expensive reconciliation. Filesystem access and missed changes remain visible as partial coverage, progress, or errors. Everything feature parity and speed superiority are not claimed.

The pinned GUI dependency has an [open upstream grid advisory](https://github.com/becheran/grid/security/advisories/GHSA-38c5-483c-4qqp); see the repository's [verification notes](https://github.com/ersinkoc/RustSearch/blob/v0.0.1/docs/DESKTOP_RESULTS.md). Windows 10, clean-machine installation, screen readers, mixed DPI, and long-duration resource behavior have not been accepted by this release.

`build-info.json` records the source commit and toolchain. `SHA256SUMS.txt` inside the ZIP covers its files; the separate release checksum covers the ZIP. These checksums identify bytes and do not constitute code signing. Dependency attributions and license texts are in `THIRD_PARTY_NOTICES.md` and `licenses/`.

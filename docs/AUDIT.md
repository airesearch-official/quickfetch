# QuickFetch repository audit

Reviewed November 15–22, 2026. Scope covers source code, download lifecycle, local persistence, dependency setup, frontend behavior, build tooling, documentation, and Windows packaging.

## Changes made following the review

| Area | Finding | Change |
| --- | --- | --- |
| Tool installation | Download size alone could not establish integrity. | Verify SHA-256 against the manifest from the same official yt-dlp release tag before replacement. Bound network reads and retry failed transfers. |
| Tool discovery | Windows `where` also searches the working directory. | Resolve absolute PATH entries and check executable versions. |
| YouTube setup | Missing JavaScript runtimes were not shown in Settings. | Detect Deno 2.3 or newer and provide a setup-help link. |
| Restart recovery | Queue state disappeared when the process closed. | Save queue metadata atomically and restore unfinished entries as Ready to resume. Retain the original folder for started jobs. |
| Process lifecycle | Closing the app could leave downloader or media processes running. | Put Windows download/media processes in kill-on-close job objects. |
| Progress traffic | Each output line sent the whole queue and logs to the webview. | Send a per-job event for recognized progress/status changes and fetch diagnostic text on demand. |
| File discovery | Show opened the current folder even if an older job used a different one. | Reveal the completed job's recorded file and report moved/deleted files. |
| Config saves | A partially written settings file could lose preferences. | Write a temporary file and atomically replace the previous file. Serialize changes under the settings lock. |
| Tool-update concurrency | New jobs could start during binary replacement. | Hold queue scheduling while the managed-tool update runs, then resume it after success or error. |
| Webview | Content Security Policy was disabled. | Restrict scripts/assets to the app and allow Tauri IPC explicitly. Inline styles support progress-bar widths. |
| Release checks | Installer verification depended on manual steps. | Add a Windows workflow for tests, installer packaging, installation, and launch. |
| Dependency advisories | The initial scans flagged vulnerable npm and Rust dependencies. | Update the affected development packages and Rust lockfile entries, including the TLS and QUIC stacks. |

## Verification

- TypeScript, ESLint, build, and five frontend tests passed. Rust formatting, Clippy with warnings denied, and 18 local Rust tests passed, including four process and media integration tests.
- The fifth integration test passed separately against an official upstream yt-dlp download. It verified the checksum and ran the installed binary's version check in a temporary directory.
- `npm audit` reported zero known vulnerabilities. RustSec reported zero vulnerability advisories after the lockfile update. Informational warnings remain for unmaintained transitive crates and a `glib` issue outside the Windows dependency tree. Registry checks for yanked packages timed out, so the final Rust scan used `--no-yanked`.
- The v0.1.1 Windows installer installed, launched the app, and uninstalled successfully in a local smoke test. The installed app retained its existing download history. The real app also completed a generated 1080p WebM download, fetched Details on demand, selected the output in File Explorer, and refreshed its tool status through Check setup.
- The installer is 1,931,651 bytes. SHA-256 is `dd6ed9cb950f8f13640383ac33bf7e98a3aadfea7716325e7377e14849572c27` for the locally tested build. GitHub Actions run results are tracked separately from the local smoke test.

The opt-in tests exercise actual yt-dlp processes against two generated 1080p VP9/Opus DASH fixtures on a local HTTP server. Other cases exercise H.264/AAC merge failure and MKV recovery, valid output despite a nonzero downloader exit, rate-limit errors with no output, and cookie-file arguments containing spaces. The upstream installer test downloads, verifies, and checks the version of a release executable in a temporary directory.

Frontend tests exercise incoming per-job IPC updates and the Reveal file, Details, and Resume actions. These checks complement the Windows application screenshots and local runtime checks.

## Privacy and trust

Source inspection found no analytics or advertising integration. QuickFetch contacts the media sites selected by the user. Managed-tool setup fetches GitHub release metadata, checksums, and a binary over HTTPS. Help actions open official documentation.

The local queue contains URLs, titles, and output paths. This can include sensitive signed URLs. Cookie contents remain in the selected file and are passed to yt-dlp. Diagnostic output is session-local and excluded from queue serialization and normal progress messages. Users should review logs before attaching them to public issues.

## Limits and follow-up work

- This is a repository and runtime review, not an independent penetration test or certification.
- Public Windows packages are unsigned. Checksums do not replace publisher code signing.
- Site authentication and rate limiting can change. Local media fixtures do not prove every YouTube URL or account works.
- FFprobe checks streams and duration. It does not decode every frame or prove a transfer contains the entire expected video.
- Windows job supervision attaches immediately after spawning the process. There is a short interval before assignment. Further hardening could assign the job at process creation.
- Multiple simultaneously running QuickFetch instances share per-user queue files. A dedicated single-instance policy remains a follow-up.
- Queue metadata is saved at lifecycle transitions. A forced stop can restore a job as interrupted even when a file completed just before exit. Resume rechecks the existing output through yt-dlp.
- Queue entries are deduplicated by the submitted URL string. Different URLs for the same video can still create duplicate work.
- New download presets, playlist selection, and browser-cookie extraction remain feature proposals. The current validator intentionally expects video with audio.

Use the live [Windows workflow](https://github.com/airesearch-official/quickfetch/actions/workflows/windows.yml) and [published release assets](https://github.com/airesearch-official/quickfetch/releases) to check the state of a particular revision or package.

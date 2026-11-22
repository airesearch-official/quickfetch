# Changelog

## 0.1.1 — 2026-11-22

### Added

- Saved queue history and manual resume for interrupted downloads. Started jobs keep their original output folder.
- Reveal file for completed outputs, including recovered MKV files.
- Check setup, Deno detection, and links to dependency installation guides.
- Windows process-tree supervision for downloader and media processes.
- Windows CI checks with installer packaging and an install-and-launch smoke test.
- Real application screenshots, setup guidance, and an audit report.

### Improved

- Managed yt-dlp downloads are verified against the SHA-256 manifest from the same release tag.
- Tool discovery searches absolute PATH entries and validates detected executables.
- Progress events carry one job at a time. Diagnostics are fetched when Details is opened.
- Settings and queue saves replace files atomically. Tool updates hold the queue until replacement finishes.
- Setup actions report progress and errors inside Settings.
- The webview has a restricted content security policy.

[Release and downloads](https://github.com/airesearch-official/quickfetch/releases/tag/v0.1.1)

## 0.1.0 — 2026-10-18

First public Windows x64 release.

- Concurrent download queue with live progress, cancellation, and retry.
- WebM video and audio selection up to 1080p, with MKV merge recovery.
- FFprobe output validation and actionable rate-limit messages.
- Folder preferences, cookies.txt support, and yt-dlp setup.
- Windows installer, portable ZIP, and package checksums.

[Release and downloads](https://github.com/airesearch-official/quickfetch/releases/tag/v0.1.0)

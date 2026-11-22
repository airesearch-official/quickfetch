# Hardening work

This file records the v0.1.0 work. For the current repository review and subsequent improvements, see [docs/AUDIT.md](docs/AUDIT.md) and [CHANGELOG.md](CHANGELOG.md).

## Completed changes

- Baseline repository initialized at `18e6794` with secret, cookie, download and build exclusions.
- Optional Netscape cookies file chooser, persistence, removal, and yt-dlp argument integration.
- Actionable HTTP 429 and bot-verification messages.
- Explicit progress enablement and isolated yt-dlp configuration, one video per queue item.
- Child output drains before terminal status assignment; terminal states resist late progress.
- Retry waits for the previous process to stop.
- JSON telemetry preserves percentage, speed, ETA, byte totals and estimated totals; UI renders byte sizes and one decimal percentage.
- FFmpeg/FFprobe paths are canonicalized and executables checked with `-version`.
- Output completion checks video/audio streams and positive duration with FFprobe after pipes drain, regardless of yt-dlp exit code.
- Recovery captures yt-dlp's pre-postprocessing stream paths and copies streams into a unique MKV if final output validation fails. Source files are preserved, and cancellation is supported.
- Default format selection prefers WebM video/audio up to 1080p before the general format fallback.
- Full queue integration test uses two local HTTP DASH fixtures, actual yt-dlp processes and FFmpeg merges.
- React component test verifies concurrent rows update from IPC events; initial snapshot subscription race fixed.
- Real yt-dlp emits unquoted `NA` for unavailable template fields. Added JSON `null` placeholder and floating-point byte-estimate support after the full queue test exposed this.
- Windows unit-test executable now embeds Common Controls v6 manifest; app retains Tauri's existing manifest.

## Verification results — 2026-10-18

- `npm run typecheck`, `npm run lint`, `npm test` (5 tests), and `npm run build`: passed.
- `cargo fmt --check` and `cargo test` (10 regular tests): passed.
- `cargo test --lib -- --ignored --test-threads=1`: all four process/media integration tests passed.
- Real H.264/AAC streams fail WebM merging and recover into validated MKV; originals remain intact.
- Full worker test confirms valid output plus exit code 1 completes, missing output fails with 429 guidance, and failed merges recover. Cookie paths containing spaces are passed correctly, using an empty test cookie jar.
- Two generated 1920×1080 VP9/Opus DASH fixtures download through real yt-dlp from local HTTP. The production scheduler was observed with two jobs downloading/merging simultaneously. Both merged WebM outputs passed FFprobe validation.
- The full worker test compares parsed byte counts, total sizes, speed and ETA to actual yt-dlp output. React IPC-event tests verify displayed metrics and independent updates of concurrent rows.

## Verification scope

Media tests use generated local HTTP fixtures for repeatability. Site-specific YouTube throttling and acceptance of a user's authenticated cookies are not guaranteed. Native window interactions and real-account cookie exports are not automated. Frontend rendering is tested in jsdom with IPC events; backend telemetry is checked against actual yt-dlp process output.

Byte totals describe the current stream and reset between video and audio. Failed recovery can leave partial output beside preserved source streams for diagnosis. Recovery does not overwrite existing files.

# QuickFetch

QuickFetch is a lightweight Windows desktop frontend for `yt-dlp`. Paste one or more video URLs, click **Add to downloads**, and the app runs up to three downloads concurrently by default.

## Behavior

- Prefers WebM video/audio up to 1080p: `bv*[ext=webm][height<=1080]+ba[ext=webm]/bv*[height<=1080][protocol^=https]+ba/b`.
- Attempts WebM merging, then copies downloaded streams into a uniquely named MKV if the original output is invalid. Recovery preserves source streams and supports cancellation.
- Checks final output with FFprobe for video, audio and positive duration. A valid output is completed even if yt-dlp reports a later error; warnings alone do not fail downloads.
- Reports percentage, downloaded/total bytes, speed and ETA through Tauri IPC. Sizes are per stream; they reset when yt-dlp switches from video to audio. Estimated totals are marked `~`.
- Spawns `yt-dlp` with an argument array and hides child console windows.
- Detects an app-managed `yt-dlp` first, then PATH. Resolves FFmpeg executable links and checks FFprobe beside FFmpeg or on PATH. Both are required.
- Stores the download folder and concurrency preference in the per-user app config directory.
- Downloads an app-managed `yt-dlp.exe` only from the official GitHub release URL when requested.
- Settings accept a Netscape-format `cookies.txt` export. Only its path is stored; cookie contents stay in the selected file and are passed to yt-dlp. Keep this file private. Authentication can help with bot checks but does not guarantee relief from rate limits.
- HTTP 429 and bot-check errors offer actionable guidance. Reduce concurrency and wait before retrying a rate-limited site.

## Development

```powershell
npm install
npm run tauri dev
```

Quality checks:

```powershell
npm run typecheck
npm run lint
npm test
cargo test --manifest-path src-tauri/Cargo.toml
# Real process/media integration tests (requires yt-dlp, FFmpeg, FFprobe,
# Python and rustc on PATH). Uses generated local media, no account required.
cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --test-threads=1
```

Build Windows installers:

```powershell
npm run tauri -- build
```

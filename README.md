# QuickFetch

QuickFetch is a lightweight Windows desktop frontend for `yt-dlp`. Paste one or more video URLs, click **Add to downloads**, and the app runs up to three downloads concurrently by default.

## Behavior

- Uses `bv*[height<=1080][protocol^=https]+ba/b` and merges to WebM.
- Spawns `yt-dlp` with an argument array and hides child console windows.
- Detects an app-managed `yt-dlp` first, then PATH; FFmpeg follows the same priority.
- Stores the download folder and concurrency preference in the per-user app config directory.
- Downloads an app-managed `yt-dlp.exe` only from the official GitHub release URL when requested.

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
```

Build Windows installers:

```powershell
npm run tauri build
```

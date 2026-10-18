import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Check, ChevronDown, ChevronUp, Clipboard, Download, ExternalLink, FolderOpen,
  LoaderCircle, RotateCcw, Settings as SettingsIcon, Trash2, X,
} from "lucide-react";
import type { AppSnapshot, DownloadJob, EnqueueResult, Settings } from "./types";
import { parseUrlLines } from "./url-utils";
import { progressText } from "./progress";

const emptySnapshot: AppSnapshot = {
  settings: { download_dir: "", concurrency: 3, open_folder_when_done: false, cookies_file: null },
  jobs: [],
  dependencies: { ytdlp_path: null, ytdlp_version: null, ytdlp_managed: false, ffmpeg_path: null, ffmpeg_version: null, ffprobe_path: null, ffprobe_version: null },
};

function statusLabel(status: DownloadJob["status"]) {
  return status.charAt(0).toUpperCase() + status.slice(1);
}

function App() {
  const [snapshot, setSnapshot] = useState<AppSnapshot>(emptySnapshot);
  const [input, setInput] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    let receivedEvent = false;
    listen<AppSnapshot>("snapshot", (event) => {
      receivedEvent = true;
      if (!disposed) setSnapshot(event.payload);
    }).then(async (fn) => {
      if (disposed) { fn(); return; }
      unlisten = fn;
      const initial = await invoke<AppSnapshot>("get_snapshot");
      if (!disposed && !receivedEvent) setSnapshot(initial);
    }).catch((error) => { if (!disposed) setNotice(String(error)); });
    return () => { disposed = true; unlisten?.(); };
  }, []);

  const counts = useMemo(() => snapshot.jobs.reduce((acc, job) => {
    acc[job.status] = (acc[job.status] ?? 0) + 1;
    return acc;
  }, {} as Record<string, number>), [snapshot.jobs]);

  async function paste() {
    try {
      const text = await navigator.clipboard.readText();
      setInput((current) => current ? `${current.trimEnd()}\n${text}` : text);
    } catch { setNotice("Clipboard access was unavailable. You can still press Ctrl+V in the box."); }
  }

  async function enqueue() {
    const parsed = parseUrlLines(input);
    if (!parsed.urls.length) {
      setNotice(parsed.rejected.length ? "Enter a valid http or https video URL." : "Paste at least one video URL first.");
      return;
    }
    setBusy(true);
    try {
      const result = await invoke<EnqueueResult>("enqueue_urls", { urls: parsed.urls });
      setInput("");
      const messages = [];
      if (result.duplicates.length) messages.push(`${result.duplicates.length} duplicate${result.duplicates.length === 1 ? " was" : "s were"} skipped`);
      if (parsed.rejected.length || result.rejected.length) messages.push(`${parsed.rejected.length + result.rejected.length} invalid entr${parsed.rejected.length + result.rejected.length === 1 ? "y" : "ies"} skipped`);
      setNotice(messages.length ? messages.join(" • ") : null);
    } catch (error) { setNotice(String(error)); }
    finally { setBusy(false); }
  }

  async function updateSettings(next: Partial<Settings>) {
    try { await invoke("update_settings", { settings: { ...snapshot.settings, ...next } }); }
    catch (error) { setNotice(String(error)); }
  }

  async function run(command: string, args: Record<string, unknown> = {}) {
    try { await invoke(command, args); }
    catch (error) { setNotice(String(error)); }
  }

  const hasFinished = snapshot.jobs.some((job) => ["completed", "failed", "cancelled"].includes(job.status));

  return (
    <main className="app-shell">
      <header>
        <div className="brand"><div className="brand-mark"><Download size={17} /></div><span>QuickFetch</span></div>
        <button className="icon-button" aria-label="Settings" onClick={() => setSettingsOpen(true)}><SettingsIcon size={19} /></button>
      </header>

      <section className="composer panel">
        <div className="section-heading">
          <div><h1>Download a video</h1><p>Paste one or more links, one per line.</p></div>
          <button className="paste-button" onClick={paste}><Clipboard size={16} /> Paste</button>
        </div>
        <textarea
          value={input}
          onChange={(event) => setInput(event.target.value)}
          onKeyDown={(event) => { if ((event.ctrlKey || event.metaKey) && event.key === "Enter") void enqueue(); }}
          placeholder={"https://youtube.com/watch?v=…\nhttps://youtu.be/…"}
          aria-label="Video URLs"
          spellCheck={false}
        />
        <div className="destination">
          <div><span>Downloads to</span><strong title={snapshot.settings.download_dir}>{snapshot.settings.download_dir || "Downloads"}</strong></div>
          <button className="text-button" onClick={() => run("choose_download_folder")}>Change folder</button>
        </div>
        <button className="download-button" disabled={busy || !input.trim()} onClick={enqueue}>
          {busy ? <LoaderCircle className="spin" size={18} /> : <Download size={18} />} Add to downloads
        </button>
        {notice && <div className="notice"><span>{notice}</span><button aria-label="Dismiss" onClick={() => setNotice(null)}><X size={14} /></button></div>}
      </section>

      <section className="downloads-section">
        <div className="downloads-header">
          <div><h2>Downloads</h2><p>{summaryText(counts)}</p></div>
          <div className="header-actions">
            <button className="text-button" onClick={() => run("open_download_folder")}><FolderOpen size={15} /> Open folder</button>
            {hasFinished && <button className="text-button" onClick={() => run("clear_finished")}><Trash2 size={15} /> Clear finished</button>}
          </div>
        </div>
        <div className="job-list">
          {snapshot.jobs.length === 0 ? <EmptyState /> : snapshot.jobs.map((job) => <JobRow key={job.id} job={job} run={run} />)}
        </div>
      </section>

      {settingsOpen && <SettingsPanel snapshot={snapshot} close={() => setSettingsOpen(false)} updateSettings={updateSettings} run={run} setNotice={setNotice} />}
    </main>
  );
}

function summaryText(counts: Record<string, number>) {
  const parts: string[] = [];
  const active = (counts.starting ?? 0) + (counts.downloading ?? 0) + (counts.merging ?? 0);
  if (active) parts.push(`${active} active`);
  if (counts.waiting) parts.push(`${counts.waiting} waiting`);
  if (counts.completed) parts.push(`${counts.completed} completed`);
  return parts.length ? parts.join(" • ") : "Your queue is ready";
}

function EmptyState() {
  return <div className="empty-state"><div><Download size={22} /></div><h3>No downloads yet</h3><p>Your videos will appear here with live progress.</p></div>;
}

function JobRow({ job, run }: { job: DownloadJob; run: (command: string, args?: Record<string, unknown>) => Promise<void> }) {
  const [details, setDetails] = useState(false);
  const active = ["starting", "downloading", "merging"].includes(job.status);
  const subtitle = job.status === "downloading"
    ? progressText(job)
    : statusLabel(job.status);
  return <article className={`job job-${job.status}`}>
    <div className="job-top">
      <div className="job-copy">
        <h3 title={job.title ?? job.url}>{job.title ?? (job.status === "waiting" ? "Waiting for download" : "Preparing video…")}</h3>
        <p title={job.filename ?? job.url}>{job.error ?? job.filename ?? subtitle}</p>
      </div>
      <span className="status-pill">{job.status === "completed" && <Check size={13} />}{active && <LoaderCircle className="spin" size={13} />}{statusLabel(job.status)}</span>
    </div>
    {(active || job.progress != null) && <div className="progress-track" role="progressbar" aria-label="Download progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={job.progress ?? undefined}><div style={{ width: `${job.progress ?? (job.status === "merging" ? 100 : 3)}%` }} /></div>}
    <div className="job-footer">
      <span>{subtitle}</span>
      <div className="job-actions">
        {active && <button onClick={() => run("cancel_job", { id: job.id })}><X size={14} /> Cancel</button>}
        {job.status === "waiting" && <button onClick={() => run("cancel_job", { id: job.id })}><X size={14} /> Remove</button>}
        {(job.status === "failed" || job.status === "cancelled") && <button onClick={() => run("retry_job", { id: job.id })}><RotateCcw size={14} /> Retry</button>}
        {job.status === "completed" && <button onClick={() => run("open_download_folder")}><FolderOpen size={14} /> Show</button>}
        {["completed", "failed", "cancelled"].includes(job.status) && <button className="icon-only" aria-label="Remove from list" onClick={() => run("remove_job", { id: job.id })}><Trash2 size={14} /></button>}
        {job.details && <button onClick={() => setDetails(!details)}>{details ? <ChevronUp size={14} /> : <ChevronDown size={14} />} Details</button>}
      </div>
    </div>
    {details && <pre>{job.details}</pre>}
  </article>;
}

function SettingsPanel({ snapshot, close, updateSettings, run, setNotice }: {
  snapshot: AppSnapshot; close: () => void; updateSettings: (next: Partial<Settings>) => Promise<void>;
  run: (command: string, args?: Record<string, unknown>) => Promise<void>; setNotice: (value: string | null) => void;
}) {
  const [updating, setUpdating] = useState(false);
  async function updateYtdlp() {
    setUpdating(true);
    try { const message = await invoke<string>("update_ytdlp"); setNotice(message); }
    catch (error) { setNotice(String(error)); }
    finally { setUpdating(false); }
  }
  return <div className="modal-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget) close(); }}>
    <aside className="settings-panel">
      <div className="settings-title"><div><h2>Settings</h2><p>Just the essentials.</p></div><button className="icon-button" onClick={close}><X size={19} /></button></div>
      <div className="settings-group">
        <h3>Downloads</h3>
        <label><span><strong>Download folder</strong><small title={snapshot.settings.download_dir}>{snapshot.settings.download_dir}</small></span><button onClick={() => run("choose_download_folder")}>Change</button></label>
        <label><span><strong>Concurrent downloads</strong><small>How many videos run at once</small></span>
          <select value={snapshot.settings.concurrency} onChange={(event) => updateSettings({ concurrency: Number(event.target.value) })}>{[1,2,3,4,5].map((n) => <option key={n}>{n}</option>)}</select>
        </label>
        <label><span><strong>Open folder when queue finishes</strong><small>Only after all active downloads finish</small></span><input type="checkbox" checked={snapshot.settings.open_folder_when_done} onChange={(event) => updateSettings({ open_folder_when_done: event.target.checked })} /></label>
      </div>
      <div className="settings-group">
        <h3>Downloader</h3>
        <label><span><strong>Authentication cookies</strong><small>{snapshot.settings.cookies_file ?? "No cookies selected"}</small></span><button onClick={() => run("choose_cookies_file")}>Choose cookies.txt</button></label>
        <p>Use a Netscape-format cookies export from your signed-in browser. Cookies grant access to your account; keep the file private. Authentication may help with bot checks, but does not guarantee relief from rate limits.</p>
        {snapshot.settings.cookies_file && <button onClick={() => updateSettings({ cookies_file: null })}>Stop using cookies</button>}
        <DependencyRow name="yt-dlp" available={!!snapshot.dependencies.ytdlp_path} detail={snapshot.dependencies.ytdlp_version ? `${snapshot.dependencies.ytdlp_version}${snapshot.dependencies.ytdlp_managed ? " • managed by QuickFetch" : " • system"}` : "Not found"} />
        <DependencyRow name="FFmpeg" available={!!snapshot.dependencies.ffmpeg_path} detail={snapshot.dependencies.ffmpeg_version ?? "Not found — required to merge audio and video"} />
        <DependencyRow name="FFprobe" available={!!snapshot.dependencies.ffprobe_path} detail={snapshot.dependencies.ffprobe_version ?? "Not found — required to validate output media"} />
        <div className="settings-buttons">
          <button onClick={updateYtdlp} disabled={updating}>{updating && <LoaderCircle className="spin" size={14} />}{snapshot.dependencies.ytdlp_path ? "Check yt-dlp update" : "Install yt-dlp"}</button>
          {!snapshot.dependencies.ffmpeg_path && <button onClick={() => run("open_ffmpeg_help")}><ExternalLink size={14} /> Get FFmpeg</button>}
        </div>
      </div>
      <footer>QuickFetch 0.1.0 · Local, private, and focused.</footer>
    </aside>
  </div>;
}

function DependencyRow({ name, available, detail }: { name: string; available: boolean; detail: string }) {
  return <div className="dependency-row"><div className={available ? "dependency-ok" : "dependency-missing"}>{available ? <Check size={14} /> : <X size={14} />}</div><span><strong>{name}</strong><small>{detail}</small></span></div>;
}

export default App;

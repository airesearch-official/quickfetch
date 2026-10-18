use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::Arc,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FORMAT_PRESET: &str = "bv*[height<=1080][protocol^=https]+ba/b";

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    download_dir: String,
    concurrency: usize,
    open_folder_when_done: bool,
}

#[derive(Clone, Serialize, Default)]
struct DependencyInfo {
    ytdlp_path: Option<String>,
    ytdlp_version: Option<String>,
    ytdlp_managed: bool,
    ffmpeg_path: Option<String>,
    ffmpeg_version: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum JobStatus {
    Waiting,
    Starting,
    Downloading,
    Merging,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Serialize, Deserialize)]
struct DownloadJob {
    id: String,
    url: String,
    title: Option<String>,
    status: JobStatus,
    progress: Option<f64>,
    speed: Option<String>,
    eta: Option<String>,
    filename: Option<String>,
    error: Option<String>,
    details: String,
}

#[derive(Clone, Serialize)]
struct AppSnapshot {
    settings: Settings,
    jobs: Vec<DownloadJob>,
    dependencies: DependencyInfo,
}

#[derive(Serialize)]
struct EnqueueResult {
    added: usize,
    duplicates: Vec<String>,
    rejected: Vec<String>,
}

struct Inner {
    settings: Settings,
    jobs: Vec<DownloadJob>,
    queue: VecDeque<String>,
    active: usize,
    cancellations: HashMap<String, CancellationToken>,
    dependencies: DependencyInfo,
}

#[derive(Clone)]
struct Shared {
    inner: Arc<Mutex<Inner>>,
    config_dir: PathBuf,
    data_dir: PathBuf,
}

fn default_settings() -> Settings {
    Settings {
        download_dir: dirs::download_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .to_string_lossy()
            .to_string(),
        concurrency: 3,
        open_folder_when_done: false,
    }
}

fn load_settings(config_dir: &Path) -> Settings {
    let path = config_dir.join("settings.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
        .map(|mut value| {
            value.concurrency = value.concurrency.clamp(1, 5);
            value
        })
        .unwrap_or_else(default_settings)
}

fn persist_settings(shared: &Shared, settings: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(&shared.config_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(shared.config_dir.join("settings.json"), json).map_err(|e| e.to_string())
}

fn log_line(shared: &Shared, message: &str) {
    let _ = std::fs::create_dir_all(&shared.data_dir);
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(shared.data_dir.join("quickfetch.log"))
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let _ = writeln!(file, "[{timestamp}] {message}");
    }
}

#[cfg(windows)]
fn hidden_std(command: &mut StdCommand) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hidden_std(_: &mut StdCommand) {}

#[cfg(windows)]
fn hidden_tokio(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hidden_tokio(_: &mut Command) {}

fn find_on_path(executable: &str) -> Option<PathBuf> {
    let mut command = if cfg!(windows) {
        let mut value = StdCommand::new("where.exe");
        value.arg(executable);
        value
    } else {
        let mut value = StdCommand::new("which");
        value.arg(executable);
        value
    };
    hidden_std(&mut command);
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.lines().next().map(str::trim).map(PathBuf::from))
}

fn command_version(path: &Path, arg: &str) -> Option<String> {
    let mut command = StdCommand::new(path);
    command
        .arg(arg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hidden_std(&mut command);
    command
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|out| {
            let text = if out.stdout.is_empty() {
                out.stderr
            } else {
                out.stdout
            };
            String::from_utf8(text)
                .ok()?
                .lines()
                .next()
                .map(|s| s.trim().to_string())
        })
}

fn detect_dependencies(data_dir: &Path) -> DependencyInfo {
    let managed_ytdlp = data_dir.join("bin").join(if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    });
    let ytdlp_path = managed_ytdlp
        .exists()
        .then_some(managed_ytdlp.clone())
        .or_else(|| find_on_path("yt-dlp"));
    let managed_ffmpeg = data_dir.join("bin").join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    let ffmpeg_path = managed_ffmpeg
        .exists()
        .then_some(managed_ffmpeg)
        .or_else(|| find_on_path("ffmpeg"));
    DependencyInfo {
        ytdlp_version: ytdlp_path
            .as_deref()
            .and_then(|path| command_version(path, "--version")),
        ytdlp_managed: ytdlp_path.as_ref() == Some(&managed_ytdlp),
        ytdlp_path: ytdlp_path.map(|p| p.to_string_lossy().to_string()),
        ffmpeg_version: ffmpeg_path
            .as_deref()
            .and_then(|path| command_version(path, "-version")),
        ffmpeg_path: ffmpeg_path.map(|p| p.to_string_lossy().to_string()),
    }
}

async fn snapshot(shared: &Shared) -> AppSnapshot {
    let inner = shared.inner.lock().await;
    AppSnapshot {
        settings: inner.settings.clone(),
        jobs: inner.jobs.clone(),
        dependencies: inner.dependencies.clone(),
    }
}

async fn emit_snapshot(shared: &Shared, app: &AppHandle) {
    let _ = app.emit("snapshot", snapshot(shared).await);
}

#[tauri::command]
async fn get_snapshot(state: State<'_, Shared>) -> Result<AppSnapshot, String> {
    Ok(snapshot(&state).await)
}

#[tauri::command]
async fn enqueue_urls(
    urls: Vec<String>,
    state: State<'_, Shared>,
    app: AppHandle,
) -> Result<EnqueueResult, String> {
    let shared = state.inner().clone();
    let mut duplicates = Vec::new();
    let mut rejected = Vec::new();
    let mut added = 0;
    {
        let mut inner = shared.inner.lock().await;
        for input in urls {
            let value = input.trim().to_string();
            if Url::parse(&value)
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .is_none()
            {
                rejected.push(value);
                continue;
            }
            if inner.jobs.iter().any(|job| job.url == value) {
                duplicates.push(value);
                continue;
            }
            let id = uuid::Uuid::new_v4().to_string();
            inner.queue.push_back(id.clone());
            inner.jobs.insert(
                0,
                DownloadJob {
                    id,
                    url: value,
                    title: None,
                    status: JobStatus::Waiting,
                    progress: None,
                    speed: None,
                    eta: None,
                    filename: None,
                    error: None,
                    details: String::new(),
                },
            );
            added += 1;
        }
    }
    emit_snapshot(&shared, &app).await;
    pump(shared, app);
    Ok(EnqueueResult {
        added,
        duplicates,
        rejected,
    })
}

#[tauri::command]
async fn update_settings(
    settings: Settings,
    state: State<'_, Shared>,
    app: AppHandle,
) -> Result<(), String> {
    let shared = state.inner().clone();
    let mut safe = settings;
    safe.concurrency = safe.concurrency.clamp(1, 5);
    if safe.download_dir.trim().is_empty() {
        return Err("Choose a valid download folder.".into());
    }
    persist_settings(&shared, &safe)?;
    shared.inner.lock().await.settings = safe;
    emit_snapshot(&shared, &app).await;
    pump(shared, app);
    Ok(())
}

#[tauri::command]
async fn choose_download_folder(state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let shared = state.inner().clone();
    let current = shared.inner.lock().await.settings.download_dir.clone();
    let selection = app
        .dialog()
        .file()
        .set_directory(current)
        .blocking_pick_folder();
    if let Some(path) = selection.and_then(|value| value.into_path().ok()) {
        let mut settings = shared.inner.lock().await.settings.clone();
        settings.download_dir = path.to_string_lossy().to_string();
        persist_settings(&shared, &settings)?;
        shared.inner.lock().await.settings = settings;
        emit_snapshot(&shared, &app).await;
    }
    Ok(())
}

#[tauri::command]
async fn open_download_folder(state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let path = state.inner.lock().await.settings.download_dir.clone();
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_ffmpeg_help(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url("https://ffmpeg.org/download.html", None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cancel_job(id: String, state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let shared = state.inner().clone();
    let token = {
        let mut inner = shared.inner.lock().await;
        cancel_in_inner(&mut inner, &id)?
    };
    if let Some(token) = token {
        token.cancel();
    }
    emit_snapshot(&shared, &app).await;
    Ok(())
}

#[tauri::command]
async fn retry_job(id: String, state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let shared = state.inner().clone();
    {
        let mut inner = shared.inner.lock().await;
        let job = inner
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or("Download not found")?;
        if !matches!(job.status, JobStatus::Failed | JobStatus::Cancelled) {
            return Err("Only failed or cancelled downloads can be retried.".into());
        }
        job.status = JobStatus::Waiting;
        job.progress = None;
        job.speed = None;
        job.eta = None;
        job.error = None;
        job.details.clear();
        inner.queue.push_back(id);
    }
    emit_snapshot(&shared, &app).await;
    pump(shared, app);
    Ok(())
}

#[tauri::command]
async fn remove_job(id: String, state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let shared = state.inner().clone();
    {
        let mut inner = shared.inner.lock().await;
        if inner.jobs.iter().any(|j| {
            j.id == id
                && matches!(
                    j.status,
                    JobStatus::Starting
                        | JobStatus::Downloading
                        | JobStatus::Merging
                        | JobStatus::Waiting
                )
        }) {
            return Err("Cancel this download before removing it.".into());
        }
        inner.jobs.retain(|job| job.id != id);
    }
    emit_snapshot(&shared, &app).await;
    Ok(())
}

#[tauri::command]
async fn clear_finished(state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let shared = state.inner().clone();
    shared.inner.lock().await.jobs.retain(|job| {
        !matches!(
            job.status,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
    });
    emit_snapshot(&shared, &app).await;
    Ok(())
}

#[tauri::command]
async fn update_ytdlp(state: State<'_, Shared>, app: AppHandle) -> Result<String, String> {
    let shared = state.inner().clone();
    let dependency = shared.inner.lock().await.dependencies.clone();
    let message = if let Some(path) = dependency.ytdlp_path {
        let mut command = Command::new(&path);
        command
            .arg("--update-to")
            .arg("stable")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hidden_tokio(&mut command);
        let output = command
            .output()
            .await
            .map_err(|e| format!("Could not start yt-dlp update: {e}"))?;
        let text = String::from_utf8_lossy(if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        })
        .trim()
        .to_string();
        if !output.status.success() {
            return Err(if text.is_empty() {
                "yt-dlp update failed.".into()
            } else {
                text
            });
        }
        if text.is_empty() {
            "yt-dlp is up to date.".into()
        } else {
            text
        }
    } else {
        install_managed_ytdlp(&shared).await?;
        "yt-dlp was installed and is ready.".into()
    };
    let dependencies = detect_dependencies(&shared.data_dir);
    shared.inner.lock().await.dependencies = dependencies;
    emit_snapshot(&shared, &app).await;
    Ok(message)
}

async fn install_managed_ytdlp(shared: &Shared) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Automatic yt-dlp setup is currently available in the Windows build.".into());
    }
    let bin_dir = shared.data_dir.join("bin");
    tokio::fs::create_dir_all(&bin_dir)
        .await
        .map_err(|e| e.to_string())?;
    let response =
        reqwest::get("https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe")
            .await
            .map_err(|e| format!("Could not reach the official yt-dlp release: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "The official yt-dlp download returned HTTP {}.",
            response.status()
        ));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() < 1_000_000 {
        return Err("The yt-dlp download did not look like a valid executable.".into());
    }
    let temporary = bin_dir.join("yt-dlp.download");
    let destination = bin_dir.join("yt-dlp.exe");
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|e| e.to_string())?;
    if destination.exists() {
        tokio::fs::remove_file(&destination)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::rename(temporary, destination)
        .await
        .map_err(|e| e.to_string())
}

fn take_next(inner: &mut Inner) -> Option<(String, CancellationToken)> {
    if inner.active >= inner.settings.concurrency {
        return None;
    }
    inner.queue.pop_front().map(|id| {
        inner.active += 1;
        let token = CancellationToken::new();
        inner.cancellations.insert(id.clone(), token.clone());
        if let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) {
            job.status = JobStatus::Starting;
        }
        (id, token)
    })
}

fn cancel_in_inner(inner: &mut Inner, id: &str) -> Result<Option<CancellationToken>, String> {
    let job = inner
        .jobs
        .iter_mut()
        .find(|job| job.id == id)
        .ok_or("Download not found")?;
    match job.status {
        JobStatus::Waiting => {
            job.status = JobStatus::Cancelled;
            inner.queue.retain(|queued| queued != id);
            Ok(None)
        }
        JobStatus::Starting | JobStatus::Downloading | JobStatus::Merging => {
            job.status = JobStatus::Cancelled;
            Ok(inner.cancellations.get(id).cloned())
        }
        _ => Ok(None),
    }
}

fn pump(shared: Shared, app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let next = {
                let mut inner = shared.inner.lock().await;
                take_next(&mut inner)
            };
            let Some((id, token)) = next else { break };
            emit_snapshot(&shared, &app).await;
            let job_shared = shared.clone();
            let job_app = app.clone();
            tauri::async_runtime::spawn(async move {
                run_job(id, token, job_shared, job_app).await;
            });
        }
    });
}

async fn run_job(id: String, cancellation: CancellationToken, shared: Shared, app: AppHandle) {
    let job_data = {
        let inner = shared.inner.lock().await;
        inner.jobs.iter().find(|job| job.id == id).map(|job| {
            (
                job.url.clone(),
                inner.settings.download_dir.clone(),
                inner.dependencies.clone(),
            )
        })
    };
    let Some((url, folder, dependencies)) = job_data else {
        return finish_slot(id, shared, app).await;
    };
    let Some(ytdlp) = dependencies.ytdlp_path else {
        fail_job(
            &id,
            "yt-dlp was not found. Open Settings to install it.",
            &shared,
            &app,
        )
        .await;
        return finish_slot(id, shared, app).await;
    };
    if dependencies.ffmpeg_path.is_none() {
        fail_job(
            &id,
            "FFmpeg was not found. Install FFmpeg before downloading merged video and audio.",
            &shared,
            &app,
        )
        .await;
        return finish_slot(id, shared, app).await;
    }

    log_line(&shared, &format!("starting job {id}; args: -f <preset> --merge-output-format webm -P <download-dir> <redacted-url>"));
    let mut command = Command::new(ytdlp);
    command
        .arg("-f").arg(FORMAT_PRESET)
        .arg("--merge-output-format").arg("webm")
        .arg("--newline")
        .arg("--progress-template").arg("download:__QF_PROGRESS__|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s|%(info.title)s|%(info.filename)s")
        .arg("--print").arg("before_dl:__QF_META__|%(title)s|%(filename)s")
        .arg("--print").arg("after_move:__QF_DONE__|%(filepath)s")
        .arg("-P").arg(folder)
        .arg("--no-colors");
    if let Some(ffmpeg) = dependencies
        .ffmpeg_path
        .and_then(|p| PathBuf::from(p).parent().map(Path::to_path_buf))
    {
        command.arg("--ffmpeg-location").arg(ffmpeg);
    }
    command
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hidden_tokio(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            fail_job(
                &id,
                &format!("Could not start yt-dlp: {error}"),
                &shared,
                &app,
            )
            .await;
            return finish_slot(id, shared, app).await;
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let output_shared = shared.clone();
    let output_app = app.clone();
    let output_id = id.clone();
    let stdout_task = tauri::async_runtime::spawn(async move {
        if let Some(stream) = stdout {
            read_output(stream, output_id, output_shared, output_app).await;
        }
    });
    let error_shared = shared.clone();
    let error_app = app.clone();
    let error_id = id.clone();
    let stderr_task = tauri::async_runtime::spawn(async move {
        if let Some(stream) = stderr {
            read_output(stream, error_id, error_shared, error_app).await;
        }
    });

    let process_id = child.id();
    let cancelled = tokio::select! {
        result = child.wait() => {
            match result {
                Ok(status) if status.success() => false,
                Ok(status) => {
                    let already_cancelled = shared.inner.lock().await.jobs.iter().find(|j| j.id == id).map(|j| j.status == JobStatus::Cancelled).unwrap_or(false);
                    if !already_cancelled { fail_job(&id, &format!("Download failed (yt-dlp exited with {status})."), &shared, &app).await; }
                    already_cancelled
                }
                Err(error) => { fail_job(&id, &format!("Could not wait for yt-dlp: {error}"), &shared, &app).await; false }
            }
        },
        _ = cancellation.cancelled() => {
            terminate_process_tree(process_id).await;
            let _ = child.start_kill();
            let _ = child.wait().await;
            true
        }
    };
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    if cancelled {
        log_line(&shared, &format!("cancelled job {id}"));
    } else {
        let mut inner = shared.inner.lock().await;
        if let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) {
            if !matches!(job.status, JobStatus::Failed | JobStatus::Cancelled) {
                job.status = JobStatus::Completed;
                job.progress = Some(100.0);
                log_line(&shared, &format!("completed job {id}"));
            }
        }
    }
    emit_snapshot(&shared, &app).await;
    finish_slot(id, shared, app).await;
}

async fn terminate_process_tree(process_id: Option<u32>) {
    #[cfg(windows)]
    if let Some(process_id) = process_id {
        let mut command = Command::new("taskkill.exe");
        command
            .arg("/PID")
            .arg(process_id.to_string())
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hidden_tokio(&mut command);
        let _ = command.status().await;
    }
    #[cfg(not(windows))]
    let _ = process_id;
}

async fn read_output<R: tokio::io::AsyncRead + Unpin>(
    stream: R,
    id: String,
    shared: Shared,
    app: AppHandle,
) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        apply_output_line(&id, &line, &shared).await;
        emit_snapshot(&shared, &app).await;
    }
}

async fn apply_output_line(id: &str, line: &str, shared: &Shared) {
    let mut inner = shared.inner.lock().await;
    let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) else {
        return;
    };
    if job.details.len() < 24_000 {
        job.details.push_str(line);
        job.details.push('\n');
    }
    if job.status == JobStatus::Cancelled {
        return;
    }
    if let Some(data) = line.strip_prefix("__QF_PROGRESS__|") {
        let parts: Vec<_> = data.splitn(6, '|').collect();
        job.status = JobStatus::Downloading;
        job.progress = parts.first().and_then(|value| parse_percent(value));
        job.speed = parts.get(1).and_then(|value| clean_field(value));
        job.eta = parts.get(2).and_then(|value| clean_field(value));
        if let Some(value) = parts.get(3).and_then(|value| clean_field(value)) {
            job.title = Some(value);
        }
        if let Some(value) = parts.get(4).and_then(|value| clean_field(value)) {
            job.filename = Some(value);
        }
    } else if let Some(data) = line.strip_prefix("__QF_META__|") {
        let mut parts = data.splitn(2, '|');
        job.title = parts.next().and_then(clean_field);
        job.filename = parts.next().and_then(clean_field);
        job.status = JobStatus::Downloading;
    } else if let Some(path) = line.strip_prefix("__QF_DONE__|") {
        job.filename = clean_field(path);
    } else if line.contains("[Merger]") || line.contains("Merging formats into") {
        job.status = JobStatus::Merging;
        job.progress = Some(100.0);
    } else if line.starts_with("ERROR:") {
        job.error = Some(human_error(line));
    }
}

fn parse_percent(value: &str) -> Option<f64> {
    value
        .trim()
        .trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
        .map(|v| v.clamp(0.0, 100.0))
}

fn clean_field(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "NA" && value != "Unknown").then(|| value.to_string())
}

fn human_error(line: &str) -> String {
    let lowered = line.to_ascii_lowercase();
    if lowered.contains("video unavailable") {
        "Video unavailable.".into()
    } else if lowered.contains("private video") {
        "This video is private.".into()
    } else if lowered.contains("unsupported url") {
        "This URL is not supported by yt-dlp.".into()
    } else if lowered.contains("ffmpeg") && lowered.contains("not found") {
        "FFmpeg was not found.".into()
    } else {
        line.trim_start_matches("ERROR:").trim().to_string()
    }
}

async fn fail_job(id: &str, message: &str, shared: &Shared, app: &AppHandle) {
    let mut inner = shared.inner.lock().await;
    if let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) {
        job.status = JobStatus::Failed;
        if job.error.is_none() {
            job.error = Some(message.to_string());
        }
    }
    log_line(shared, &format!("job {id} failed: {message}"));
    drop(inner);
    emit_snapshot(shared, app).await;
}

async fn finish_slot(id: String, shared: Shared, app: AppHandle) {
    let open_folder = {
        let mut inner = shared.inner.lock().await;
        inner.active = inner.active.saturating_sub(1);
        inner.cancellations.remove(&id);
        inner.settings.open_folder_when_done && inner.active == 0 && inner.queue.is_empty()
    };
    emit_snapshot(&shared, &app).await;
    pump(shared.clone(), app.clone());
    if open_folder {
        let path = shared.inner.lock().await.settings.download_dir.clone();
        let _ = app.opener().open_path(path, None::<&str>);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_values() {
        assert_eq!(parse_percent(" 72.4%"), Some(72.4));
        assert_eq!(parse_percent("NA"), None);
        assert_eq!(parse_percent("120%"), Some(100.0));
    }

    #[test]
    fn maps_common_errors() {
        assert_eq!(
            human_error("ERROR: Video unavailable"),
            "Video unavailable."
        );
        assert_eq!(
            human_error("ERROR: Unsupported URL: x"),
            "This URL is not supported by yt-dlp."
        );
    }

    fn test_inner(concurrency: usize, count: usize) -> Inner {
        let settings = Settings {
            download_dir: ".".into(),
            concurrency,
            open_folder_when_done: false,
        };
        let mut jobs = Vec::new();
        let mut queue = VecDeque::new();
        for index in 0..count {
            let id = index.to_string();
            queue.push_back(id.clone());
            jobs.push(DownloadJob {
                id,
                url: format!("https://example.com/{index}"),
                title: None,
                status: JobStatus::Waiting,
                progress: None,
                speed: None,
                eta: None,
                filename: None,
                error: None,
                details: String::new(),
            });
        }
        Inner {
            settings,
            jobs,
            queue,
            active: 0,
            cancellations: HashMap::new(),
            dependencies: DependencyInfo::default(),
        }
    }

    #[test]
    fn scheduler_respects_concurrency_and_advances() {
        let mut inner = test_inner(3, 5);
        assert!(take_next(&mut inner).is_some());
        assert!(take_next(&mut inner).is_some());
        assert!(take_next(&mut inner).is_some());
        assert!(take_next(&mut inner).is_none());
        assert_eq!(inner.active, 3);
        assert_eq!(inner.queue.len(), 2);
        inner.active -= 1;
        assert!(take_next(&mut inner).is_some());
        assert_eq!(inner.active, 3);
        assert_eq!(inner.queue.len(), 1);
    }

    #[test]
    fn cancellation_removes_waiting_and_signals_active_jobs() {
        let mut inner = test_inner(1, 2);
        let (active_id, active_token) = take_next(&mut inner).unwrap();
        let waiting_id = inner.queue.front().unwrap().clone();
        assert!(cancel_in_inner(&mut inner, &waiting_id).unwrap().is_none());
        assert!(inner.queue.is_empty());
        let returned = cancel_in_inner(&mut inner, &active_id).unwrap().unwrap();
        returned.cancel();
        assert!(active_token.is_cancelled());
    }

    #[test]
    fn settings_round_trip_through_json_file() {
        let directory =
            std::env::temp_dir().join(format!("quickfetch-settings-{}", uuid::Uuid::new_v4()));
        let shared = Shared {
            inner: Arc::new(Mutex::new(test_inner(3, 0))),
            config_dir: directory.clone(),
            data_dir: directory.join("data"),
        };
        let expected = Settings {
            download_dir: "D:\\Videos".into(),
            concurrency: 5,
            open_folder_when_done: true,
        };
        persist_settings(&shared, &expected).unwrap();
        let actual = load_settings(&directory);
        assert_eq!(actual.download_dir, expected.download_dir);
        assert_eq!(actual.concurrency, 5);
        assert!(actual.open_folder_when_done);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancellation_terminates_a_real_child_process() {
        let mut command = Command::new("ping.exe");
        command
            .arg("-t")
            .arg("127.0.0.1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hidden_tokio(&mut command);
        let mut child = command.spawn().unwrap();
        let process_id = child.id();
        assert!(process_id.is_some());
        terminate_process_tree(process_id).await;
        let status = child.wait().await.unwrap();
        assert!(!status.success());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let data_dir = app.path().app_local_data_dir()?;
            let settings = load_settings(&config_dir);
            let dependencies = detect_dependencies(&data_dir);
            let shared = Shared {
                inner: Arc::new(Mutex::new(Inner {
                    settings,
                    jobs: Vec::new(),
                    queue: VecDeque::new(),
                    active: 0,
                    cancellations: HashMap::new(),
                    dependencies: dependencies.clone(),
                })),
                config_dir,
                data_dir,
            };
            log_line(
                &shared,
                &format!(
                    "QuickFetch 0.1.0 started; yt-dlp={:?}; ffmpeg={:?}",
                    dependencies.ytdlp_version, dependencies.ffmpeg_version
                ),
            );
            app.manage(shared);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            enqueue_urls,
            update_settings,
            choose_download_folder,
            open_download_folder,
            open_ffmpeg_help,
            cancel_job,
            retry_job,
            remove_job,
            clear_finished,
            update_ytdlp,
        ])
        .run(tauri::generate_context!())
        .expect("error while running QuickFetch");
}

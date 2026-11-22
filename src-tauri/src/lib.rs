mod process_tree;

use process_tree::ProcessTree;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
const FORMAT_PRESET: &str =
    "bv*[ext=webm][height<=1080]+ba[ext=webm]/bv*[height<=1080][protocol^=https]+ba/b";
const PROGRESS_TEMPLATE: &str = "download:__QF_PROGRESS_JSON__{\"percent\":%(progress._percent_str)j,\"speed\":%(progress._speed_str)j,\"eta\":%(progress._eta_str)j,\"downloaded\":%(progress.downloaded_bytes)j,\"total\":%(progress.total_bytes)j,\"estimated_total\":%(progress.total_bytes_estimate)j}";

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    download_dir: String,
    concurrency: usize,
    open_folder_when_done: bool,
    #[serde(default)]
    cookies_file: Option<String>,
}

#[derive(Clone, Serialize, Default)]
struct DependencyInfo {
    ytdlp_path: Option<String>,
    ytdlp_version: Option<String>,
    ytdlp_managed: bool,
    ffmpeg_path: Option<String>,
    ffmpeg_version: Option<String>,
    ffprobe_path: Option<String>,
    ffprobe_version: Option<String>,
    deno_path: Option<String>,
    deno_version: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum JobStatus {
    Waiting,
    Interrupted,
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
    downloaded_bytes: Option<u64>,
    total_bytes: Option<u64>,
    total_is_estimate: bool,
    speed: Option<String>,
    eta: Option<String>,
    filename: Option<String>,
    #[serde(default)]
    download_dir: Option<String>,
    #[serde(skip)]
    merge_inputs: Vec<String>,
    error: Option<String>,
    #[serde(
        rename = "has_details",
        skip_deserializing,
        serialize_with = "serialize_log_presence"
    )]
    details: String,
}

fn serialize_log_presence<S: serde::Serializer>(
    details: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_bool(!details.is_empty())
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
    updating_tool: bool,
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
        cookies_file: None,
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
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    write_config_file(&shared.config_dir, "settings.json", &json)
}

fn load_queue(config_dir: &Path) -> Vec<DownloadJob> {
    let Ok(bytes) = std::fs::read(config_dir.join("queue.json")) else {
        return Vec::new();
    };
    let Ok(mut jobs) = serde_json::from_slice::<Vec<DownloadJob>>(&bytes) else {
        return Vec::new();
    };
    for job in &mut jobs {
        if matches!(
            job.status,
            JobStatus::Waiting | JobStatus::Starting | JobStatus::Downloading | JobStatus::Merging
        ) {
            job.status = JobStatus::Interrupted;
            job.progress = None;
            job.downloaded_bytes = None;
            job.total_bytes = None;
            job.total_is_estimate = false;
            job.speed = None;
            job.eta = None;
            job.error = None;
        }
    }
    jobs
}

fn persist_queue(config_dir: &Path, jobs: &[DownloadJob]) -> Result<(), String> {
    let bytes = serde_json::to_vec(jobs).map_err(|error| error.to_string())?;
    write_config_file(config_dir, "queue.json", &bytes)
}

fn write_config_file(config_dir: &Path, filename: &str, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(config_dir).map_err(|error| error.to_string())?;
    let staging = config_dir.join(format!("{filename}-{}.tmp", uuid::Uuid::new_v4()));
    let result = std::fs::write(&staging, bytes)
        .and_then(|()| std::fs::rename(&staging, config_dir.join(filename)));
    if let Err(error) = result {
        let _ = std::fs::remove_file(staging);
        return Err(error.to_string());
    }
    Ok(())
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
    let name = if cfg!(windows) {
        format!("{executable}.exe")
    } else {
        executable.to_owned()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&name))
        .find(|path| path.is_file())
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
    let ytdlp = Some(managed_ytdlp.clone())
        .into_iter()
        .chain(find_on_path("yt-dlp"))
        .filter(|path| path.is_file())
        .find_map(|path| command_version(&path, "--version").map(|version| (path, version)));
    let managed_ffmpeg = data_dir.join("bin").join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    let ffmpeg = Some(managed_ffmpeg)
        .into_iter()
        .chain(find_on_path("ffmpeg"))
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .find_map(|path| command_version(&path, "-version").map(|version| (path, version)));
    let ffprobe = ffmpeg
        .as_ref()
        .and_then(|(path, _)| path.parent())
        .map(|path| {
            path.join(if cfg!(windows) {
                "ffprobe.exe"
            } else {
                "ffprobe"
            })
        })
        .into_iter()
        .chain(find_on_path("ffprobe"))
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .find_map(|path| command_version(&path, "-version").map(|version| (path, version)));
    let deno = find_on_path("deno").and_then(|path| {
        let version = command_version(&path, "--version")?;
        let mut parts = version.split_whitespace().nth(1)?.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let minor = parts.next()?.parse::<u32>().ok()?;
        ((major, minor) >= (2, 3)).then_some((path, version))
    });
    DependencyInfo {
        ytdlp_managed: ytdlp
            .as_ref()
            .is_some_and(|(path, _)| path == &managed_ytdlp),
        ytdlp_path: ytdlp
            .as_ref()
            .map(|(path, _)| path.to_string_lossy().into_owned()),
        ytdlp_version: ytdlp.map(|(_, version)| version),
        ffmpeg_path: ffmpeg
            .as_ref()
            .map(|(path, _)| path.to_string_lossy().into_owned()),
        ffmpeg_version: ffmpeg.map(|(_, version)| version),
        ffprobe_path: ffprobe
            .as_ref()
            .map(|(path, _)| path.to_string_lossy().into_owned()),
        ffprobe_version: ffprobe.map(|(_, version)| version),
        deno_path: deno
            .as_ref()
            .map(|(path, _)| path.to_string_lossy().into_owned()),
        deno_version: deno.map(|(_, version)| version),
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

trait SnapshotSink {
    fn send(&self, snapshot: AppSnapshot);
}

impl SnapshotSink for AppHandle {
    fn send(&self, snapshot: AppSnapshot) {
        let _ = self.emit("snapshot", snapshot);
    }
}

impl SnapshotSink for Option<AppHandle> {
    fn send(&self, snapshot: AppSnapshot) {
        if let Some(app) = self {
            app.send(snapshot);
        }
    }
}

async fn emit_snapshot(shared: &Shared, app: &impl SnapshotSink) {
    let inner = shared.inner.lock().await;
    if let Err(error) = persist_queue(&shared.config_dir, &inner.jobs) {
        log_line(shared, &format!("Could not save download queue: {error}"));
    }
    app.send(AppSnapshot {
        settings: inner.settings.clone(),
        jobs: inner.jobs.clone(),
        dependencies: inner.dependencies.clone(),
    });
}

#[tauri::command]
async fn get_snapshot(state: State<'_, Shared>) -> Result<AppSnapshot, String> {
    Ok(snapshot(&state).await)
}

#[tauri::command]
async fn get_job_details(id: String, state: State<'_, Shared>) -> Result<String, String> {
    state
        .inner
        .lock()
        .await
        .jobs
        .iter()
        .find(|job| job.id == id)
        .map(|job| job.details.clone())
        .ok_or_else(|| "Download not found".into())
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
                    downloaded_bytes: None,
                    total_bytes: None,
                    total_is_estimate: false,
                    speed: None,
                    eta: None,
                    filename: None,
                    download_dir: None,
                    merge_inputs: Vec::new(),
                    error: None,
                    details: String::new(),
                },
            );
            added += 1;
        }
    }
    emit_snapshot(&shared, &app).await;
    pump(shared, Some(app));
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
    safe.cookies_file = safe.cookies_file.filter(|path| !path.trim().is_empty());
    if let Some(path) = &safe.cookies_file {
        if !Path::new(path).is_file() {
            return Err("The selected cookies file no longer exists. Choose it again.".into());
        }
    }
    {
        let mut inner = shared.inner.lock().await;
        persist_settings(&shared, &safe)?;
        inner.settings = safe;
    }
    emit_snapshot(&shared, &app).await;
    pump(shared, Some(app));
    Ok(())
}

#[tauri::command]
async fn choose_cookies_file(state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    let selection = app
        .dialog()
        .file()
        .add_filter("Netscape cookies", &["txt"])
        .blocking_pick_file();
    if let Some(path) = selection.and_then(|value| value.into_path().ok()) {
        let shared = state.inner().clone();
        let mut inner = shared.inner.lock().await;
        let mut settings = inner.settings.clone();
        settings.cookies_file = Some(path.to_string_lossy().into_owned());
        persist_settings(&shared, &settings)?;
        inner.settings = settings;
        drop(inner);
        emit_snapshot(&shared, &app).await;
    }
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
        let mut inner = shared.inner.lock().await;
        let mut settings = inner.settings.clone();
        settings.download_dir = path.to_string_lossy().to_string();
        persist_settings(&shared, &settings)?;
        inner.settings = settings;
        drop(inner);
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
async fn reveal_download(
    id: String,
    state: State<'_, Shared>,
    app: AppHandle,
) -> Result<(), String> {
    let path = state
        .inner
        .lock()
        .await
        .jobs
        .iter()
        .find(|job| job.id == id && job.status == JobStatus::Completed)
        .and_then(|job| job.filename.clone())
        .ok_or("No completed file is available for this download.")?;
    if !Path::new(&path).is_file() {
        return Err("The downloaded file has been moved or deleted.".into());
    }
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn open_ffmpeg_help(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url("https://ffmpeg.org/download.html", None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_deno_help(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(
            "https://docs.deno.com/runtime/getting_started/installation/",
            None::<&str>,
        )
        .map_err(|error| error.to_string())
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
        if inner.cancellations.contains_key(&id) {
            return Err("The previous download is still stopping. Retry in a moment.".into());
        }
        let job = inner
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or("Download not found")?;
        if !matches!(
            job.status,
            JobStatus::Failed | JobStatus::Cancelled | JobStatus::Interrupted
        ) {
            return Err(
                "Only failed, cancelled, or interrupted downloads can be restarted.".into(),
            );
        }
        job.status = JobStatus::Waiting;
        job.progress = None;
        job.downloaded_bytes = None;
        job.total_bytes = None;
        job.total_is_estimate = false;
        job.speed = None;
        job.eta = None;
        job.error = None;
        job.merge_inputs.clear();
        job.filename = None;
        job.details.clear();
        inner.queue.push_back(id);
    }
    emit_snapshot(&shared, &app).await;
    pump(shared, Some(app));
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
    static UPDATE_LOCK: Mutex<()> = Mutex::const_new(());
    let _update = UPDATE_LOCK
        .try_lock()
        .map_err(|_| "A yt-dlp update is already running.")?;
    {
        let mut inner = shared.inner.lock().await;
        if inner.active > 0 {
            return Err("Wait for active downloads to finish before updating yt-dlp.".into());
        }
        inner.updating_tool = true;
    }
    let result = async {
        let version = install_managed_ytdlp(&shared).await?;
        refresh_dependencies(&shared, &app).await?;
        Ok(format!("Managed yt-dlp {version} is ready."))
    }
    .await;
    shared.inner.lock().await.updating_tool = false;
    pump(shared, Some(app));
    result
}

#[tauri::command]
async fn check_setup(state: State<'_, Shared>, app: AppHandle) -> Result<(), String> {
    refresh_dependencies(state.inner(), &app).await
}

async fn refresh_dependencies(shared: &Shared, app: &AppHandle) -> Result<(), String> {
    let directory = shared.data_dir.clone();
    let dependencies =
        tauri::async_runtime::spawn_blocking(move || detect_dependencies(&directory))
            .await
            .map_err(|error| error.to_string())?;
    shared.inner.lock().await.dependencies = dependencies;
    emit_snapshot(shared, app).await;
    Ok(())
}

#[derive(Deserialize)]
struct YtdlpRelease {
    tag_name: String,
}

fn verify_ytdlp_checksum(checksums: &str, bytes: &[u8]) -> Result<(), String> {
    let expected = checksums
        .lines()
        .find_map(|line| {
            let (hash, filename) = line.split_once(char::is_whitespace)?;
            (filename.trim().trim_start_matches('*') == "yt-dlp.exe").then_some(hash)
        })
        .ok_or("The yt-dlp release checksum is missing.")?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("The yt-dlp download did not match its published SHA-256 checksum.".into());
    }
    Ok(())
}

async fn fetch_bounded(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut last_error = String::new();
    for attempt in 0..3 {
        match fetch_bounded_once(client, url, limit).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last_error = error,
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
        }
    }
    Err(last_error)
}

async fn fetch_bounded_once(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("Could not download the yt-dlp release: {error}"))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Could not read {url}: {error:?}"))?
    {
        if chunk.len() > limit - bytes.len() {
            return Err("The yt-dlp release file exceeds the download limit.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn install_managed_ytdlp(shared: &Shared) -> Result<String, String> {
    if !cfg!(windows) {
        return Err("Automatic yt-dlp setup is currently available in the Windows build.".into());
    }
    let bin_dir = shared.data_dir.join("bin");
    tokio::fs::create_dir_all(&bin_dir)
        .await
        .map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .user_agent("QuickFetch")
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|error| error.to_string())?;
    let metadata = fetch_bounded(
        &client,
        "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest",
        512 * 1024,
    )
    .await?;
    let release: YtdlpRelease =
        serde_json::from_slice(&metadata).map_err(|error| error.to_string())?;
    let version = release.tag_name;
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return Err("Unexpected stable yt-dlp release version.".into());
    }
    let destination = bin_dir.join("yt-dlp.exe");
    let base = format!("https://github.com/yt-dlp/yt-dlp/releases/download/{version}");
    let checksum_bytes = fetch_bounded(&client, &format!("{base}/SHA2-256SUMS"), 64 * 1024).await?;
    let checksums = std::str::from_utf8(&checksum_bytes).map_err(|error| error.to_string())?;
    let bytes = fetch_bounded(&client, &format!("{base}/yt-dlp.exe"), 64 * 1024 * 1024).await?;
    verify_ytdlp_checksum(checksums, &bytes)?;
    let temporary = bin_dir.join(format!("yt-dlp-{}.download", uuid::Uuid::new_v4()));
    let result = async {
        tokio::fs::write(&temporary, bytes).await?;
        // Replace in one operation so a failed rename leaves the existing binary in place.
        tokio::fs::rename(&temporary, destination).await
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!("Could not install managed yt-dlp: {error}"));
    }
    Ok(version)
}

fn take_next(inner: &mut Inner) -> Option<(String, CancellationToken)> {
    if inner.updating_tool || inner.active >= inner.settings.concurrency {
        return None;
    }
    inner.queue.pop_front().map(|id| {
        inner.active += 1;
        let token = CancellationToken::new();
        inner.cancellations.insert(id.clone(), token.clone());
        if let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) {
            job.status = JobStatus::Starting;
            job.download_dir
                .get_or_insert_with(|| inner.settings.download_dir.clone());
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

fn pump(shared: Shared, app: Option<AppHandle>) {
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

async fn run_job(
    id: String,
    cancellation: CancellationToken,
    shared: Shared,
    app: Option<AppHandle>,
) {
    let job_data = {
        let inner = shared.inner.lock().await;
        inner.jobs.iter().find(|job| job.id == id).map(|job| {
            (
                job.url.clone(),
                job.download_dir
                    .clone()
                    .unwrap_or_else(|| inner.settings.download_dir.clone()),
                inner.dependencies.clone(),
                inner.settings.cookies_file.clone(),
            )
        })
    };
    let Some((url, folder, dependencies, cookies_file)) = job_data else {
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
    if dependencies.ffmpeg_path.is_none() || dependencies.ffprobe_path.is_none() {
        fail_job(
            &id,
            "FFmpeg and FFprobe are required. Install both before downloading merged video and audio.",
            &shared,
            &app,
        )
        .await;
        return finish_slot(id, shared, app).await;
    }

    log_line(&shared, &format!("starting job {id}; args: -f <preset> --merge-output-format webm -P <download-dir> <redacted-url>"));
    let mut command = Command::new(ytdlp);
    if let Some(path) = cookies_file {
        command.arg("--cookies").arg(path);
    }
    if let Some(path) = dependencies.deno_path.as_deref() {
        command.arg("--js-runtimes").arg(format!("deno:{path}"));
    }
    command
        .arg("--ignore-config")
        .arg("--no-playlist")
        .arg("--progress")
        .arg("--output-na-placeholder").arg("null")
        .arg("-f")
        .arg(FORMAT_PRESET)
        .arg("--merge-output-format")
        .arg("webm")
        .arg("--newline")
        .arg("--progress-delta")
        .arg("0.2")
        .arg("--progress-template")
        .arg(PROGRESS_TEMPLATE)
        .arg("--print")
        .arg("before_dl:__QF_META_JSON__{\"title\":%(title)j,\"filename\":%(filename)j}")
        .arg("--print").arg("post_process:__QF_MERGE_JSON__{\"inputs\":%(__files_to_merge)j,\"filename\":%(filepath)j}")
        .arg("--print")
        .arg("after_move:__QF_DONE_JSON__%(filepath)j")
        .arg("-P")
        .arg(folder)
        .arg("--no-colors");
    if let Some(ffmpeg) = dependencies
        .ffmpeg_path
        .as_ref()
        .and_then(|p| PathBuf::from(p).parent().map(Path::to_path_buf))
    {
        command.arg("--ffmpeg-location").arg(ffmpeg);
    }
    command
        .arg("--")
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hidden_tokio(&mut command);
    let (mut child, tree) = match ProcessTree::spawn(&mut command) {
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
    let outcome = tokio::select! {
        result = child.wait() => Some(result),
        _ = cancellation.cancelled() => {
            terminate_process_tree(process_id).await;
            let _ = child.start_kill();
            let _ = child.wait().await;
            None
        }
    };
    drop(tree);
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    if let Some(outcome) = outcome.filter(|_| !cancellation.is_cancelled()) {
        // Drain both pipes before deciding the terminal state: progress and errors
        // can still be buffered after the child has exited.
        let failure = match outcome {
            Ok(status) if status.success() => None,
            Ok(status) => Some(format!("Download failed (yt-dlp exited with {status}).")),
            Err(error) => Some(format!("Could not wait for yt-dlp: {error}")),
        };
        let filename = shared
            .inner
            .lock()
            .await
            .jobs
            .iter()
            .find(|job| job.id == id)
            .and_then(|job| job.filename.clone());
        let mut valid_media = if let (Some(filename), Some(ffprobe)) =
            (filename.as_deref(), dependencies.ffprobe_path.as_deref())
        {
            validate_media(ffprobe, filename).await
        } else {
            false
        };
        if !valid_media && !cancellation.is_cancelled() {
            let inputs = shared
                .inner
                .lock()
                .await
                .jobs
                .iter()
                .find(|job| job.id == id)
                .map(|job| job.merge_inputs.clone())
                .unwrap_or_default();
            if let (Some(target), Some(ffmpeg), Some(ffprobe)) = (
                filename.as_deref(),
                dependencies.ffmpeg_path.as_deref(),
                dependencies.ffprobe_path.as_deref(),
            ) {
                if !inputs.is_empty() {
                    {
                        let mut inner = shared.inner.lock().await;
                        if let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) {
                            if job.status != JobStatus::Cancelled {
                                job.status = JobStatus::Merging;
                                job.error = None;
                                job.details.push_str(
                                    "QuickFetch: recovering downloaded streams into MKV.\n",
                                );
                            }
                        }
                    }
                    emit_snapshot(&shared, &app).await;
                    match recover_merge(ffmpeg, ffprobe, &inputs, target, &cancellation).await {
                        Ok(path) => {
                            valid_media = true;
                            if let Some(job) = shared
                                .inner
                                .lock()
                                .await
                                .jobs
                                .iter_mut()
                                .find(|job| job.id == id)
                            {
                                job.filename = Some(path);
                            }
                        }
                        Err(error) => {
                            if let Some(job) = shared
                                .inner
                                .lock()
                                .await
                                .jobs
                                .iter_mut()
                                .find(|job| job.id == id)
                            {
                                job.error = Some(error);
                            }
                        }
                    }
                }
            }
        }
        let mut inner = shared.inner.lock().await;
        if let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) {
            if job.status != JobStatus::Cancelled && !cancellation.is_cancelled() {
                if !valid_media {
                    job.status = JobStatus::Failed;
                    if job.error.is_none() {
                        job.error = Some(failure.unwrap_or_else(|| {
                            "The downloader finished, but no valid video with audio was found."
                                .into()
                        }));
                    }
                } else {
                    job.status = JobStatus::Completed;
                    job.progress = Some(100.0);
                    job.error = None;
                    log_line(&shared, &format!("completed job {id}"));
                }
            }
        }
    }
    emit_snapshot(&shared, &app).await;
    finish_slot(id, shared, app).await;
}

async fn recover_merge(
    ffmpeg: &str,
    ffprobe: &str,
    inputs: &[String],
    target: &str,
    cancellation: &CancellationToken,
) -> Result<String, String> {
    if cancellation.is_cancelled() {
        return Err("Merge recovery cancelled.".into());
    }
    if inputs.is_empty() || inputs.iter().any(|path| !Path::new(path).is_file()) {
        return Err(
            "Merge recovery could not find all downloaded streams. Retry the download.".into(),
        );
    }
    // A unique destination preserves the original streams and any existing output.
    let target = Path::new(target);
    let output = target.with_file_name(format!(
        "{}-recovered-{}.mkv",
        target.file_stem().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let mut command = Command::new(ffmpeg);
    command.args(["-nostdin", "-n", "-v", "error"]);
    for input in inputs {
        command.arg("-i").arg(input);
    }
    for index in 0..inputs.len() {
        command.arg("-map").arg(format!("{index}:v?"));
        command.arg("-map").arg(format!("{index}:a?"));
    }
    command
        .args(["-c", "copy"])
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hidden_tokio(&mut command);
    let (child, _tree) = ProcessTree::spawn(&mut command)
        .map_err(|e| format!("Could not start MKV recovery: {e}"))?;
    let pid = child.id();
    let result = tokio::select! {
        result = child.wait_with_output() => result.map_err(|e| format!("MKV recovery failed: {e}"))?,
        _ = cancellation.cancelled() => { terminate_process_tree(pid).await; return Err("Merge recovery cancelled.".into()); }
    };
    let path = output.to_string_lossy().into_owned();
    if validate_media(ffprobe, &path).await {
        return Ok(path);
    }
    Err(format!(
        "MKV recovery did not produce valid video and audio ({}): {}",
        result.status,
        String::from_utf8_lossy(&result.stderr).trim()
    ))
}

async fn validate_media(ffprobe: &str, filename: &str) -> bool {
    if !Path::new(filename).is_file() {
        return false;
    }
    let mut command = Command::new(ffprobe);
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:format=duration",
            "-of",
            "json",
            filename,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hidden_tokio(&mut command);
    let Ok((child, _tree)) = ProcessTree::spawn(&mut command) else {
        return false;
    };
    let Ok(Ok(output)) =
        tokio::time::timeout(std::time::Duration::from_secs(20), child.wait_with_output()).await
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return false;
    };
    valid_probe_result(&value)
}

fn valid_probe_result(value: &serde_json::Value) -> bool {
    let Some(streams) = value["streams"].as_array() else {
        return false;
    };
    let has_video = streams.iter().any(|stream| stream["codec_type"] == "video");
    let has_audio = streams.iter().any(|stream| stream["codec_type"] == "audio");
    let duration = value["format"]["duration"]
        .as_str()
        .and_then(|v| v.parse::<f64>().ok());
    has_video && has_audio && duration.is_some_and(|v| v.is_finite() && v > 0.0)
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
    app: Option<AppHandle>,
) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if apply_output_line(&id, &line, &shared).await {
            if let Some(app) = &app {
                let inner = shared.inner.lock().await;
                if let Some(job) = inner.jobs.iter().find(|job| job.id == id) {
                    let _ = app.emit("job-update", job);
                }
            }
        }
    }
}

async fn apply_output_line(id: &str, line: &str, shared: &Shared) -> bool {
    let mut inner = shared.inner.lock().await;
    let Some(job) = inner.jobs.iter_mut().find(|job| job.id == id) else {
        return false;
    };
    let first_log = job.details.is_empty();
    if job.details.len() < 24_000 {
        job.details.push_str(line);
        job.details.push('\n');
    }
    if matches!(
        job.status,
        JobStatus::Cancelled | JobStatus::Completed | JobStatus::Failed
    ) {
        return false;
    }
    if let Some(data) = line.strip_prefix("__QF_PROGRESS_JSON__") {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return first_log;
        };
        job.status = JobStatus::Downloading;
        job.progress = value["percent"].as_str().and_then(parse_percent);
        job.speed = value["speed"].as_str().and_then(clean_field);
        job.eta = value["eta"].as_str().and_then(clean_field);
        job.downloaded_bytes = byte_count(&value["downloaded"]);
        let exact = byte_count(&value["total"]);
        job.total_bytes = exact.or_else(|| byte_count(&value["estimated_total"]));
        job.total_is_estimate = exact.is_none() && job.total_bytes.is_some();
    } else if let Some(data) = line.strip_prefix("__QF_MERGE_JSON__") {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return first_log;
        };
        job.merge_inputs = value["inputs"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(path) = value["filename"].as_str().filter(|path| *path != "NA") {
            job.filename = Some(path.into());
        }
        job.status = JobStatus::Merging;
    } else if let Some(data) = line.strip_prefix("__QF_META_JSON__") {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return first_log;
        };
        job.title = value["title"].as_str().map(str::to_owned);
        job.filename = value["filename"].as_str().map(str::to_owned);
        job.status = JobStatus::Downloading;
    } else if let Some(path) = line.strip_prefix("__QF_DONE_JSON__") {
        if let Ok(path) = serde_json::from_str::<String>(path) {
            job.filename = Some(path);
        }
    } else if line.contains("[Merger]") || line.contains("Merging formats into") {
        job.status = JobStatus::Merging;
        job.progress = Some(100.0);
    } else if line.starts_with("ERROR:") {
        job.error = Some(human_error(line));
    } else {
        return first_log;
    }
    true
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

fn byte_count(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v < u64::MAX as f64)
            .map(|v| v.round() as u64)
    })
}

fn clean_field(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "NA" && value != "Unknown").then(|| value.to_string())
}

fn human_error(line: &str) -> String {
    let lowered = line.to_ascii_lowercase();
    if lowered.contains("429") || lowered.contains("too many requests") {
        "The site is rate-limiting downloads (HTTP 429). Wait before retrying, reduce concurrent downloads, or reconnect your network/VPN. You can also select a cookies.txt file in Settings to authenticate.".into()
    } else if lowered.contains("not a bot")
        || lowered.contains("confirm you’re")
        || lowered.contains("confirm you're")
        || lowered.contains("bot detection")
    {
        "The site requires authentication or a browser verification. Sign in and complete verification in your browser, then select an exported cookies.txt file in Settings and retry.".into()
    } else if lowered.contains("video unavailable") {
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

async fn fail_job(id: &str, message: &str, shared: &Shared, app: &Option<AppHandle>) {
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

async fn finish_slot(id: String, shared: Shared, app: Option<AppHandle>) {
    let open_folder = {
        let mut inner = shared.inner.lock().await;
        inner.active = inner.active.saturating_sub(1);
        inner.cancellations.remove(&id);
        inner.settings.open_folder_when_done && inner.active == 0 && inner.queue.is_empty()
    };
    emit_snapshot(&shared, &app).await;
    pump(shared.clone(), app.clone());
    if let Some(app) = app.filter(|_| open_folder) {
        let path = shared.inner.lock().await.settings.download_dir.clone();
        let _ = app.opener().open_path(path, None::<&str>);
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
            let jobs = load_queue(&config_dir);
            let dependencies = detect_dependencies(&data_dir);
            let shared = Shared {
                inner: Arc::new(Mutex::new(Inner {
                    settings,
                    jobs,
                    queue: VecDeque::new(),
                    active: 0,
                    cancellations: HashMap::new(),
                    dependencies: dependencies.clone(),
                    updating_tool: false,
                })),
                config_dir,
                data_dir,
            };
            log_line(
                &shared,
                &format!(
                    "QuickFetch 0.1.1 started; yt-dlp={:?}; ffmpeg={:?}",
                    dependencies.ytdlp_version, dependencies.ffmpeg_version
                ),
            );
            app.manage(shared);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_job_details,
            enqueue_urls,
            update_settings,
            choose_download_folder,
            choose_cookies_file,
            open_download_folder,
            reveal_download,
            open_ffmpeg_help,
            open_deno_help,
            cancel_job,
            retry_job,
            remove_job,
            clear_finished,
            update_ytdlp,
            check_setup,
        ])
        .run(tauri::generate_context!())
        .expect("error while running QuickFetch");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_checksum_for_the_exact_windows_asset() {
        let hash = format!("{:x}", Sha256::digest(b"expected binary"));
        let checksums = format!("{hash}  yt-dlp_x86.exe\n{hash} *yt-dlp.exe\n");
        assert!(verify_ytdlp_checksum(&checksums, b"expected binary").is_ok());
        assert!(verify_ytdlp_checksum(&checksums, b"modified binary").is_err());
        assert!(
            verify_ytdlp_checksum(&format!("{hash}  yt-dlp_x86.exe"), b"expected binary").is_err()
        );
    }

    #[test]
    fn progress_payloads_exclude_diagnostic_text_and_merge_inputs() {
        let mut inner = test_inner(1, 1);
        let job = &mut inner.jobs[0];
        job.details = "private diagnostic content".repeat(1000);
        job.merge_inputs = vec!["C:/temporary/video.webm".into()];
        let payload = serde_json::to_string(job).unwrap();
        assert!(payload.contains("\"has_details\":true"));
        assert!(!payload.contains("private diagnostic"));
        assert!(!payload.contains("merge_inputs"));
        assert!(payload.len() < 1000);
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "downloads the current yt-dlp release from GitHub"]
    async fn installs_verified_upstream_ytdlp() {
        let directory =
            std::env::temp_dir().join(format!("quickfetch-install-{}", uuid::Uuid::new_v4()));
        let shared = Shared {
            inner: Arc::new(Mutex::new(test_inner(1, 0))),
            config_dir: directory.clone(),
            data_dir: directory.clone(),
        };
        let result = install_managed_ytdlp(&shared).await;
        let binary = directory.join("bin/yt-dlp.exe");
        let detected_version = command_version(&binary, "--version");
        if directory.is_dir() {
            std::fs::remove_dir_all(directory).unwrap();
        }
        assert_eq!(detected_version, Some(result.unwrap()));
    }

    #[test]
    fn restores_active_jobs_without_restarting_and_retains_completed_files() {
        let directory =
            std::env::temp_dir().join(format!("quickfetch-queue-{}", uuid::Uuid::new_v4()));
        let mut inner = test_inner(2, 2);
        inner.jobs[0].status = JobStatus::Downloading;
        inner.jobs[0].progress = Some(42.0);
        inner.jobs[0].download_dir = Some("C:/Original".into());
        inner.jobs[0].details = "temporary diagnostics".into();
        inner.jobs[1].status = JobStatus::Completed;
        inner.jobs[1].filename = Some("C:/Original/output.mkv".into());
        persist_queue(&directory, &inner.jobs).unwrap();
        persist_queue(&directory, &inner.jobs).unwrap();
        let restored = load_queue(&directory);
        assert!(restored[0].status == JobStatus::Interrupted);
        assert_eq!(restored[0].progress, None);
        assert_eq!(restored[0].download_dir.as_deref(), Some("C:/Original"));
        assert!(restored[0].details.is_empty());
        assert!(restored[1].status == JobStatus::Completed);
        assert_eq!(
            restored[1].filename.as_deref(),
            Some("C:/Original/output.mkv")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Rust compiler, FFmpeg and FFprobe"]
    async fn worker_handles_nonzero_exit_recovery_and_rate_limit() {
        let directory =
            std::env::temp_dir().join(format!("quickfetch-worker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let mut deps = detect_dependencies(&directory);
        let fixture = directory.join(if cfg!(windows) {
            "downloader.exe"
        } else {
            "downloader"
        });
        let mut compile = Command::new("rustc");
        compile
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/downloader.rs"))
            .arg("-o")
            .arg(&fixture);
        hidden_tokio(&mut compile);
        assert!(compile.status().await.unwrap().success());
        let mut generate = Command::new(deps.ffmpeg_path.as_ref().unwrap());
        generate
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=1920x1080:rate=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
            ])
            .arg(directory.join("source.mp4"));
        hidden_tokio(&mut generate);
        assert!(generate.status().await.unwrap().success());
        deps.ytdlp_path = Some(fixture.to_string_lossy().into_owned());
        let mut inner = test_inner(3, 3);
        inner.dependencies = deps;
        inner.settings.download_dir = directory.to_string_lossy().into_owned();
        let cookies = directory.join("test cookies.txt");
        std::fs::write(&cookies, "# Netscape HTTP Cookie File\n").unwrap();
        inner.settings.cookies_file = Some(cookies.to_string_lossy().into_owned());
        for (job, mode) in inner
            .jobs
            .iter_mut()
            .zip(["posterror", "recover", "rate-limit"])
        {
            job.url = format!("https://fixture.invalid/{mode}");
        }
        let shared = Shared {
            inner: Arc::new(Mutex::new(inner)),
            config_dir: directory.join("config"),
            data_dir: directory.join("logs"),
        };
        pump(shared.clone(), None);
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let inner = shared.inner.lock().await;
                if inner.active == 0 && inner.queue.is_empty() {
                    break;
                }
                drop(inner);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let inner = shared.inner.lock().await;
        for job in &inner.jobs {
            assert!(job.details.contains("cookies argument accepted"));
        }
        assert!(
            inner.jobs[0].status == JobStatus::Completed,
            "{:?}",
            inner.jobs[0].error
        );
        assert!(inner.jobs[0].error.is_none());
        assert!(
            inner.jobs[1].status == JobStatus::Completed,
            "{:?}",
            inner.jobs[1].error
        );
        assert!(inner.jobs[1].filename.as_ref().unwrap().ends_with(".mkv"));
        assert!(inner.jobs[2].status == JobStatus::Failed);
        assert!(inner.jobs[2]
            .error
            .as_ref()
            .unwrap()
            .contains("rate-limiting"));
        drop(inner);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires yt-dlp, FFmpeg, FFprobe and Python; downloads from local HTTP fixture"]
    async fn real_parallel_webm_downloads_through_queue() {
        let directory =
            std::env::temp_dir().join(format!("quickfetch-downloads-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let deps = detect_dependencies(&directory);
        let mut generate = Command::new(deps.ffmpeg_path.as_ref().unwrap());
        generate.current_dir(&directory).args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "4",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libvpx-vp9",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            "-b:v",
            "1500k",
            "-c:a",
            "libopus",
            "-f",
            "dash",
            "-dash_segment_type",
            "webm",
            "-seg_duration",
            "2",
            "first.mpd",
        ]);
        hidden_tokio(&mut generate);
        assert!(generate.status().await.unwrap().success());
        std::fs::copy(directory.join("first.mpd"), directory.join("second.mpd")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut server = Command::new("python");
        server
            .args([
                "-m",
                "http.server",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--directory",
            ])
            .arg(&directory)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        hidden_tokio(&mut server);
        let mut server = server.spawn().unwrap();
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let mut inner = test_inner(2, 2);
        inner.settings.download_dir = directory.join("output").to_string_lossy().into_owned();
        inner.dependencies = deps;
        inner.jobs[0].url = format!("http://127.0.0.1:{port}/first.mpd");
        inner.jobs[1].url = format!("http://127.0.0.1:{port}/second.mpd");
        let shared = Shared {
            inner: Arc::new(Mutex::new(inner)),
            config_dir: directory.join("config"),
            data_dir: directory.join("logs"),
        };
        pump(shared.clone(), None);
        let mut parallel = false;
        let result = tokio::time::timeout(std::time::Duration::from_secs(90), async {
            loop {
                let inner = shared.inner.lock().await;
                parallel |= inner
                    .jobs
                    .iter()
                    .filter(|job| matches!(job.status, JobStatus::Downloading | JobStatus::Merging))
                    .count()
                    >= 2;
                if inner.active == 0 && inner.queue.is_empty() {
                    break;
                }
                drop(inner);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        let _ = server.kill().await;
        assert!(
            result.is_ok(),
            "download timed out; artifacts: {}",
            directory.display()
        );
        let inner = shared.inner.lock().await;
        assert!(parallel);
        for job in &inner.jobs {
            assert!(
                job.status == JobStatus::Completed,
                "{}: {:?}\n{}\nartifacts: {}",
                job.url,
                job.error,
                job.details,
                directory.display()
            );
            assert!(job.filename.as_ref().unwrap().ends_with(".webm"));
            assert!(job.details.contains("__QF_PROGRESS_JSON__"));
            assert!(job.details.contains("__QF_MERGE_JSON__"));
            assert!(
                job.downloaded_bytes.is_some(),
                "progress did not parse: {}",
                job.details
            );
            let last_progress = job
                .details
                .lines()
                .filter_map(|line| line.strip_prefix("__QF_PROGRESS_JSON__"))
                .next_back()
                .unwrap();
            let terminal: serde_json::Value = serde_json::from_str(last_progress).unwrap();
            assert_eq!(job.downloaded_bytes, byte_count(&terminal["downloaded"]));
            assert_eq!(
                job.total_bytes,
                byte_count(&terminal["total"]).or_else(|| byte_count(&terminal["estimated_total"]))
            );
            assert_eq!(job.speed, terminal["speed"].as_str().and_then(clean_field));
            assert_eq!(job.eta, terminal["eta"].as_str().and_then(clean_field));
        }
        drop(inner);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn parses_progress_values() {
        assert_eq!(parse_percent(" 72.4%"), Some(72.4));
        assert_eq!(parse_percent("NA"), None);
        assert_eq!(parse_percent("120%"), Some(100.0));
    }

    #[test]
    fn validation_requires_video_audio_and_positive_duration() {
        let mut probe = serde_json::json!({"streams": [{"codec_type":"video"},{"codec_type":"audio"}], "format":{"duration":"3.008"}});
        assert!(valid_probe_result(&probe));
        probe["format"]["duration"] = serde_json::json!("0");
        assert!(!valid_probe_result(&probe));
        probe["format"]["duration"] = serde_json::json!("3.008");
        probe["streams"] = serde_json::json!([{"codec_type":"video"}]);
        assert!(!valid_probe_result(&probe));
        assert!(!valid_probe_result(&serde_json::json!({})));
    }

    #[tokio::test]
    #[ignore = "requires installed FFmpeg and FFprobe"]
    async fn real_media_validation_and_dependency_resolution() {
        let deps =
            detect_dependencies(&std::env::temp_dir().join("quickfetch-no-managed-binaries"));
        let ffmpeg = deps.ffmpeg_path.expect("FFmpeg is required");
        let ffprobe = deps.ffprobe_path.expect("FFprobe is required");
        assert!(Path::new(&ffmpeg).is_absolute());
        assert!(Path::new(&ffprobe).is_absolute());
        let directory =
            std::env::temp_dir().join(format!("quickfetch-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let output = directory.join("sample.webm");
        let mut command = Command::new(ffmpeg);
        command
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=size=1920x1080:rate=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                "1",
                "-c:v",
                "libvpx-vp9",
                "-c:a",
                "libopus",
            ])
            .arg(&output);
        hidden_tokio(&mut command);
        assert!(command.status().await.unwrap().success());
        assert!(validate_media(&ffprobe, output.to_str().unwrap()).await);
        let invalid = directory.join("invalid.webm");
        std::fs::write(&invalid, b"not media").unwrap();
        assert!(!validate_media(&ffprobe, invalid.to_str().unwrap()).await);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires installed FFmpeg and FFprobe"]
    async fn incompatible_webm_streams_recover_to_valid_mkv() {
        let deps =
            detect_dependencies(&std::env::temp_dir().join("quickfetch-no-managed-binaries"));
        let ffmpeg = deps.ffmpeg_path.unwrap();
        let ffprobe = deps.ffprobe_path.unwrap();
        let directory =
            std::env::temp_dir().join(format!("quickfetch-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let video = directory.join("video.mp4");
        let audio = directory.join("audio.m4a");
        for (input, codec, path) in [
            ("color=size=1920x1080:rate=1", "libx264", &video),
            ("sine=frequency=440", "aac", &audio),
        ] {
            let mut command = Command::new(&ffmpeg);
            command
                .args([
                    "-v", "error", "-f", "lavfi", "-i", input, "-t", "1", "-c", codec,
                ])
                .arg(path);
            hidden_tokio(&mut command);
            assert!(command.status().await.unwrap().success());
        }
        let target = directory.join("failed.webm");
        let mut command = Command::new(&ffmpeg);
        command
            .args(["-v", "error", "-i"])
            .arg(&video)
            .arg("-i")
            .arg(&audio)
            .args(["-c", "copy"])
            .arg(&target)
            .stderr(Stdio::null());
        hidden_tokio(&mut command);
        assert!(!command.status().await.unwrap().success());
        assert!(!validate_media(&ffprobe, target.to_str().unwrap()).await);
        let inputs = vec![
            video.to_string_lossy().into_owned(),
            audio.to_string_lossy().into_owned(),
        ];
        let recovered = recover_merge(
            &ffmpeg,
            &ffprobe,
            &inputs,
            target.to_str().unwrap(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(recovered.ends_with(".mkv"));
        assert!(validate_media(&ffprobe, &recovered).await);
        assert!(video.is_file() && audio.is_file());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn maps_common_errors() {
        assert!(human_error("ERROR: HTTP Error 429: Too Many Requests").contains("rate-limiting"));
        assert!(human_error("ERROR: Sign in to confirm you’re not a bot").contains("cookies.txt"));
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
            cookies_file: None,
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
                downloaded_bytes: None,
                total_bytes: None,
                total_is_estimate: false,
                speed: None,
                eta: None,
                filename: None,
                download_dir: None,
                merge_inputs: Vec::new(),
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
            updating_tool: false,
        }
    }

    #[test]
    fn legacy_settings_load_without_authentication() {
        let settings: Settings = serde_json::from_str(
            r#"{"download_dir":"D:/Videos","concurrency":3,"open_folder_when_done":false}"#,
        )
        .unwrap();
        assert!(settings.cookies_file.is_none());
    }

    #[tokio::test]
    async fn late_progress_cannot_overwrite_terminal_status() {
        let mut inner = test_inner(1, 1);
        inner.jobs[0].status = JobStatus::Failed;
        let shared = Shared {
            inner: Arc::new(Mutex::new(inner)),
            config_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
        };
        apply_output_line("0", "__QF_PROGRESS_JSON__{\"percent\":\"99%\"}", &shared).await;
        assert!(shared.inner.lock().await.jobs[0].status == JobStatus::Failed);
    }

    #[tokio::test]
    async fn structured_progress_preserves_metrics_and_handles_unknown_totals() {
        let shared = Shared {
            inner: Arc::new(Mutex::new(test_inner(1, 1))),
            config_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
        };
        apply_output_line("0", r#"__QF_PROGRESS_JSON__{"percent":" 37.7%","speed":" 3.40MiB/s","eta":"00:23","downloaded":47395635,"total":125829120,"estimated_total":"NA"}"#, &shared).await;
        {
            let inner = shared.inner.lock().await;
            let job = &inner.jobs[0];
            assert_eq!(job.progress, Some(37.7));
            assert_eq!(job.downloaded_bytes, Some(47395635));
            assert_eq!(job.total_bytes, Some(125829120));
            assert!(!job.total_is_estimate);
            assert_eq!(job.speed.as_deref(), Some("3.40MiB/s"));
            assert_eq!(job.eta.as_deref(), Some("00:23"));
        }
        apply_output_line("0", r#"__QF_PROGRESS_JSON__{"percent":"NA","downloaded":1024,"total":"NA","estimated_total":2048}"#, &shared).await;
        {
            let inner = shared.inner.lock().await;
            assert!(inner.jobs[0].total_is_estimate);
            assert_eq!(inner.jobs[0].total_bytes, Some(2048));
            assert_eq!(inner.jobs[0].progress, None);
        }
        apply_output_line("0", r#"__QF_META_JSON__{"title":"Title | with \"quotes\"","filename":"C:\\Videos\\file.webm"}"#, &shared).await;
        assert_eq!(
            shared.inner.lock().await.jobs[0].title.as_deref(),
            Some("Title | with \"quotes\"")
        );
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
            cookies_file: Some("D:\\cookies.txt".into()),
        };
        persist_settings(&shared, &expected).unwrap();
        let actual = load_settings(&directory);
        assert_eq!(actual.download_dir, expected.download_dir);
        assert_eq!(actual.concurrency, 5);
        assert!(actual.open_folder_when_done);
        assert_eq!(actual.cookies_file, expected.cookies_file);
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

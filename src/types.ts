export type JobStatus = "waiting" | "interrupted" | "starting" | "downloading" | "merging" | "completed" | "failed" | "cancelled";

export interface DownloadJob {
  id: string;
  url: string;
  title: string | null;
  status: JobStatus;
  progress: number | null;
  downloaded_bytes: number | null;
  total_bytes: number | null;
  total_is_estimate: boolean;
  speed: string | null;
  eta: string | null;
  filename: string | null;
  error: string | null;
  has_details: boolean;
}

export interface Settings {
  download_dir: string;
  concurrency: number;
  open_folder_when_done: boolean;
  cookies_file: string | null;
}

export interface DependencyInfo {
  ytdlp_path: string | null;
  ytdlp_version: string | null;
  ytdlp_managed: boolean;
  ffmpeg_path: string | null;
  ffmpeg_version: string | null;
  ffprobe_path: string | null;
  ffprobe_version: string | null;
  deno_path?: string | null;
  deno_version?: string | null;
}

export interface AppSnapshot {
  settings: Settings;
  jobs: DownloadJob[];
  dependencies: DependencyInfo;
}

export interface EnqueueResult { added: number; duplicates: string[]; rejected: string[] }

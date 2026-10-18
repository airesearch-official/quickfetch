export type JobStatus = "waiting" | "starting" | "downloading" | "merging" | "completed" | "failed" | "cancelled";

export interface DownloadJob {
  id: string;
  url: string;
  title: string | null;
  status: JobStatus;
  progress: number | null;
  speed: string | null;
  eta: string | null;
  filename: string | null;
  error: string | null;
  details: string;
}

export interface Settings {
  download_dir: string;
  concurrency: number;
  open_folder_when_done: boolean;
}

export interface DependencyInfo {
  ytdlp_path: string | null;
  ytdlp_version: string | null;
  ytdlp_managed: boolean;
  ffmpeg_path: string | null;
  ffmpeg_version: string | null;
}

export interface AppSnapshot {
  settings: Settings;
  jobs: DownloadJob[];
  dependencies: DependencyInfo;
}

export interface EnqueueResult { added: number; duplicates: string[]; rejected: string[] }

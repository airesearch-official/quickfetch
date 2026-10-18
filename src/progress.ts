import type { DownloadJob } from "./types";

export function formatBytes(bytes: number | null): string {
  if (bytes == null) return "Unknown size";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const index = bytes > 0 ? Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1) : 0;
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

export function progressText(job: DownloadJob): string {
  return [
    job.progress == null ? null : `${job.progress.toFixed(1)}%`,
    job.downloaded_bytes == null ? null : `${formatBytes(job.downloaded_bytes)} / ${job.total_is_estimate ? "~" : ""}${formatBytes(job.total_bytes)}`,
    job.speed,
    job.eta ? `${job.eta} left` : null,
  ].filter(Boolean).join("  •  ");
}

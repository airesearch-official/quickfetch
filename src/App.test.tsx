// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import App from "./App";
import type { AppSnapshot, DownloadJob } from "./types";

const bridge = vi.hoisted(() => ({ receiveJob: undefined as undefined | ((event: { payload: DownloadJob }) => void), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: bridge.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (name, receive) => { if (name === "job-update") bridge.receiveJob = receive; return () => {}; }) }));
afterEach(cleanup);

it("renders incoming IPC progress and updates each concurrent row independently", async () => {
  const snapshot: AppSnapshot = {
    settings: { download_dir: "C:/Videos", concurrency: 2, open_folder_when_done: false, cookies_file: null },
    dependencies: { ytdlp_path: null, ytdlp_version: null, ytdlp_managed: false, ffmpeg_path: null, ffmpeg_version: null, ffprobe_path: null, ffprobe_version: null },
    jobs: ["one", "two"].map(id => ({ id, url: `https://example.com/${id}`, title: id, status: "downloading", progress: 37.7, downloaded_bytes: 47395635, total_bytes: 125829120, total_is_estimate: false, speed: "3.40MiB/s", eta: "00:23", filename: null, error: null, has_details: false })),
  };
  bridge.invoke.mockResolvedValue(snapshot);
  render(<App />);
  expect(await screen.findAllByText("37.7% • 45.2 MiB / 120.0 MiB • 3.40MiB/s • 00:23 left")).toHaveLength(4);
  const next = structuredClone(snapshot);
  next.jobs[0].progress = 50.0;
  next.jobs[0].downloaded_bytes = 62914560;
  await act(async () => { bridge.receiveJob?.({ payload: next.jobs[0] }); });
  expect(screen.getAllByRole("progressbar").map(bar => bar.getAttribute("aria-valuenow"))).toEqual(["50", "37.7"]);
  expect(screen.getAllByText("50.0% • 60.0 MiB / 120.0 MiB • 3.40MiB/s • 00:23 left")).toHaveLength(2);
  const completed = { ...next.jobs[0], status: "completed" as const, filename: "C:/Original/video.mkv", has_details: true };
  await act(async () => { bridge.receiveJob?.({ payload: completed }); });
  bridge.invoke.mockResolvedValueOnce(undefined);
  fireEvent.click(screen.getByText("Reveal file"));
  expect(bridge.invoke).toHaveBeenCalledWith("reveal_download", { id: "one" });
  bridge.invoke.mockResolvedValueOnce("Verified download log");
  fireEvent.click(screen.getByText("Details"));
  expect(await screen.findByText("Verified download log")).toBeTruthy();
  expect(bridge.invoke).toHaveBeenCalledWith("get_job_details", { id: "one" });
  const interrupted = { ...next.jobs[1], status: "interrupted" as const };
  await act(async () => { bridge.receiveJob?.({ payload: interrupted }); });
  bridge.invoke.mockResolvedValueOnce(undefined);
  fireEvent.click(screen.getByText("Resume"));
  expect(bridge.invoke).toHaveBeenCalledWith("retry_job", { id: "two" });
});

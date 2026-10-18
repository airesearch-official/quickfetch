import { describe, expect, it } from "vitest";
import { formatBytes, progressText } from "./progress";
import type { DownloadJob } from "./types";

describe("progress display", () => {
  it("preserves terminal percentage, speed and ETA while formatting byte counts", () => {
    const job = { progress: 37.7, downloaded_bytes: 47395635, total_bytes: 125829120,
      total_is_estimate: false, speed: "3.40MiB/s", eta: "00:23" } as DownloadJob;
    expect(progressText(job)).toBe("37.7%  •  45.2 MiB / 120.0 MiB  •  3.40MiB/s  •  00:23 left");
    expect(progressText({ ...job, total_is_estimate: true })).toContain("~120.0 MiB");
  });
  it("does not invent a total for unknown sizes", () => {
    expect(formatBytes(null)).toBe("Unknown size");
    expect(formatBytes(0)).toBe("0 B");
  });
});

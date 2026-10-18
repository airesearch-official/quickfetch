import { describe, expect, it } from "vitest";
import { parseUrlLines } from "./url-utils";

describe("parseUrlLines", () => {
  it("accepts supported web URLs and removes exact duplicates", () => {
    const result = parseUrlLines("https://youtu.be/a\nhttps://youtu.be/a\nhttps://example.com/video");
    expect(result.urls).toEqual(["https://youtu.be/a", "https://example.com/video"]);
    expect(result.rejected).toEqual([]);
  });

  it("rejects malformed and non-http input", () => {
    const result = parseUrlLines("not a URL\nfile:///tmp/video");
    expect(result.urls).toEqual([]);
    expect(result.rejected).toHaveLength(2);
  });
});

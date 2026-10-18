export function parseUrlLines(text: string): { urls: string[]; rejected: string[] } {
  const seen = new Set<string>();
  const urls: string[] = [];
  const rejected: string[] = [];
  for (const value of text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean)) {
    try {
      const parsed = new URL(value);
      if (!(["http:", "https:"].includes(parsed.protocol)) || seen.has(value)) {
        if (!seen.has(value) && !["http:", "https:"].includes(parsed.protocol)) rejected.push(value);
        continue;
      }
      seen.add(value);
      urls.push(value);
    } catch {
      rejected.push(value);
    }
  }
  return { urls, rejected };
}

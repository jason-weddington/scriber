import MCR, { getSnapshot } from 'monocart-coverage-reports';

import { coverageOptions, NODE_V8_DIR, REQUIRED_WEB_MODULES } from './options';

// Merges the browser coverage the e2e and web fixtures added with the Node
// coverage the unit workers wrote, prints the summary, verifies every new
// web-backend src module has nonzero canonical mapped hits, and enforces the
// floor given in COVERAGE_MIN (whole-number percent of lines; scripts/gate.sh
// owns the number).
export default async function globalTeardown(): Promise<void> {
  if (!process.env.COVERAGE) return;
  const results = await MCR({ ...coverageOptions, dataDir: NODE_V8_DIR }).generate();

  const files = results?.files ?? [];
  const missingOrUnhit: string[] = [];
  for (const mod of REQUIRED_WEB_MODULES) {
    const entry = files.find((f) => f.sourcePath === mod);
    const covered = entry?.summary.lines.covered ?? 0;
    if (entry === undefined || covered <= 0) {
      missingOrUnhit.push(mod);
    }
  }
  if (missingOrUnhit.length > 0) {
    throw new Error(
      `Frontend coverage: required web modules have no mapped line hits: ${missingOrUnhit.join(', ')}.`,
    );
  }

  const lines = results?.summary.lines.pct;
  if (typeof lines !== 'number') {
    throw new Error('Frontend coverage: no line coverage was collected.');
  }
  const min = Number(process.env.COVERAGE_MIN ?? '0');
  if (!(lines >= min)) {
    const snap = results !== undefined ? getSnapshot(results).files : {};
    const details = files
      .filter((f) => (f.summary.lines.uncovered ?? 0) > 0)
      .map((f) => {
        const u = snap[f.sourcePath]?.uncoveredLines ?? '';
        return `${f.sourcePath}: ${String(f.summary.lines.pct)}% (${String(f.summary.lines.uncovered)} uncovered${u !== '' ? ` [${u}]` : ''})`;
      })
      .join('; ');
    throw new Error(
      `Frontend line coverage ${String(lines)}% is below the floor of ${String(min)}%. (${details})`,
    );
  }
}

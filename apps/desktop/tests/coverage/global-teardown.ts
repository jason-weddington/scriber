import MCR from 'monocart-coverage-reports';

import { coverageOptions, NODE_V8_DIR } from './options';

// Merges the browser coverage the e2e fixture added with the Node coverage the
// unit workers wrote, prints the summary, and enforces the floor given in
// COVERAGE_MIN (whole-number percent of lines; scripts/gate.sh owns the
// number).
export default async function globalTeardown(): Promise<void> {
  if (!process.env.COVERAGE) return;
  const results = await MCR({ ...coverageOptions, dataDir: NODE_V8_DIR }).generate();
  const lines = results?.summary.lines.pct;
  if (typeof lines !== 'number') {
    throw new Error('Frontend coverage: no line coverage was collected.');
  }
  const min = Number(process.env.COVERAGE_MIN ?? '0');
  if (!(lines >= min)) {
    throw new Error(`Frontend line coverage ${String(lines)}% is below the floor of ${String(min)}%.`);
  }
}

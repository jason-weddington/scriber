import { mkdirSync, rmSync } from 'node:fs';

import MCR from 'monocart-coverage-reports';

import { COVERAGE_DIR, coverageOptions, NODE_V8_DIR } from './options';

// Runs only for `pnpm test:coverage` (COVERAGE=1). Workers are forked after
// this, so setting NODE_V8_COVERAGE here makes the unit project's Node
// workers write V8 coverage without also instrumenting Vite's dev server.
export default function globalSetup(): void {
  if (!process.env.COVERAGE) return;
  rmSync(COVERAGE_DIR, { recursive: true, force: true });
  mkdirSync(NODE_V8_DIR, { recursive: true });
  process.env.NODE_V8_COVERAGE = NODE_V8_DIR;
  MCR(coverageOptions).cleanCache();
}

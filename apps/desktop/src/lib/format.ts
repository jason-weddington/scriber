// Display formatting. Pure functions, unit-tested in tests/unit/format.spec.ts.

const UNITS = ['B', 'kB', 'MB', 'GB', 'TB', 'PB'] as const;

/**
 * A size the way Finder shows it: powers of 1000, one decimal below 10 (and
 * none when it is .0), carrying into the next unit when rounding reaches
 * 1000 (999.6 MB is "1 GB", never "1000 MB").
 */
export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  for (;;) {
    const digits = unit > 0 && value < 10 ? 1 : 0;
    const scale = 10 ** digits;
    const rounded = Math.round(value * scale) / scale;
    if (rounded < 1000 || unit === UNITS.length - 1) {
      return `${rounded.toFixed(digits).replace(/\.0$/, '')} ${UNITS[unit] ?? ''}`;
    }
    value /= 1000;
    unit += 1;
  }
}

/** A count with thousands separators, independent of the machine's locale. */
export function formatCount(n: number): string {
  return n.toLocaleString('en-US');
}

/** "1 file", "2 files". */
export function plural(n: number, one: string, many: string): string {
  return `${formatCount(n)} ${n === 1 ? one : many}`;
}

/** How a list row names an extension. */
export function extensionLabel(extension: string): string {
  return extension === '' ? 'No extension' : `.${extension}`;
}

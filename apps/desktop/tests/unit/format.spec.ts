import { expect, test } from '@playwright/test';

import { extensionLabel, formatBytes, formatCount, plural } from '../../src/lib/format';

test('sizes use powers of 1000, one decimal below 10, and carry on rounding', () => {
  const cases: [number, string][] = [
    [0, '0 B'],
    [999, '999 B'],
    [1000, '1 kB'],
    [1200, '1.2 kB'],
    [6148, '6.1 kB'],
    [9960, '10 kB'],
    [10_400, '10 kB'],
    [999_499, '999 kB'],
    [999_600, '1 MB'],
    [57_607_348, '58 MB'],
    [999_600_000, '1 GB'],
    [2_500_000_000_000, '2.5 TB'],
    [4.2e15, '4.2 PB'],
    [1e19, '10000 PB'],
  ];
  for (const [bytes, want] of cases) expect(formatBytes(bytes), String(bytes)).toBe(want);
});

test('counts carry thousands separators whatever the machine locale', () => {
  expect(formatCount(0)).toBe('0');
  expect(formatCount(1234567)).toBe('1,234,567');
});

test('plural picks the singular only for exactly one', () => {
  expect(plural(0, 'file', 'files')).toBe('0 files');
  expect(plural(1, 'file', 'files')).toBe('1 file');
  expect(plural(2000, 'file', 'files')).toBe('2,000 files');
});

test('an extension is shown with its dot, and a missing one in words', () => {
  expect(extensionLabel('jpg')).toBe('.jpg');
  expect(extensionLabel('')).toBe('No extension');
});

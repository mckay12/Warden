// Unit tests for the duration picker's pure logic (extension/duration-picker.js).
// Run from the repo root with:  node --test
const test = require('node:test');
const assert = require('node:assert/strict');
const d = require('../extension/duration-picker.js');

const DAY = 86400;

test('fields add up to seconds (a year is 365 days)', () => {
  assert.equal(d.totalSeconds(['0', '0', '1', '0', '0']), 3600);
  assert.equal(d.totalSeconds(['1', '2', '3', '4', '5']), 365 * DAY + 2 * DAY + 3 * 3600 + 4 * 60 + 5);
});

test('blank, negative and garbage fields count as zero; decimals round down', () => {
  assert.equal(d.totalSeconds(['', '', '', '10', '']), 600);
  assert.equal(d.totalSeconds(['-5', 'abc', '1.9', '0', '0']), 3600);
});

test('describe() reads naturally and skips zero units', () => {
  assert.equal(d.describe(5400), '1 hour, 30 minutes');
  assert.equal(d.describe(365 * DAY + 1), '1 year, 1 second');
  assert.equal(d.describe(90061), '1 day, 1 hour, 1 minute, 1 second');
});

test('validate() rejects zero and anything over 100 years', () => {
  assert.equal(d.validate(0), 'Enter a duration greater than zero.');
  assert.equal(d.validate(1), null);
  assert.equal(d.validate(d.MAX_SECONDS), null);
  assert.equal(d.validate(d.MAX_SECONDS + 1), 'The longest lock is 100 years.');
  assert.equal(d.totalSeconds(['100', '0', '0', '0', '0']), d.MAX_SECONDS);
});

test('the long-lock warning starts at exactly 7 days', () => {
  assert.equal(d.LONG_LOCK_SECONDS, 7 * DAY);
  assert.equal(d.isLong(7 * DAY - 1), false);
  assert.equal(d.isLong(7 * DAY), true);
  assert.equal(d.isLong(600), false);
});

test('warning messages name the length and say it cannot be undone', () => {
  const now = Date.UTC(2026, 9, 14, 12, 0, 0);
  const start = d.longLockMessage(8 * DAY, false, now);
  assert.match(start, /^This lock will last 8 days \(until /);
  assert.match(start, /cannot be shortened or cancelled\. Start it\?$/);
  assert.equal(
    d.longLockMessage(2 * 365 * DAY, true, now),
    'You are adding 2 years to this lock.\n\nOnce added, it cannot be shortened or cancelled. Extend it?',
  );
});

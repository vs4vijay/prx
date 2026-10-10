// Unit tests for the schedule picker's cron helpers.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const C = require("../settings-ui/cron.js");

test("presets round-trip and shorthands map onto them", () => {
  assert.equal(C.presetFor(null), C.PRESETS[0]);
  assert.equal(C.presetFor("  "), C.PRESETS[0]);
  assert.equal(C.presetFor("@hourly").label, "Every hour");
  assert.equal(C.presetFor("0  9 * * 1-5").label, "Weekday mornings");
  assert.equal(C.presetFor("15 3 * * *"), null);
  for (const preset of C.PRESETS.slice(1)) assert.equal(C.parse(preset.value).ok, true, preset.value);
});

test("parse accepts standard cron and explains what is wrong", () => {
  assert.equal(C.parse("*/15 8-18 * * mon-fri").ok, true);
  assert.equal(C.parse("0 0 1 jan,jul *").ok, true);
  assert.deepEqual(Array.from(C.parse("0 0 * * 7").fields[4]), [0]);
  assert.match(C.parse("0 9 * *").error, /5 fields/);
  assert.match(C.parse("61 * * * *").error, /minute must be 0–59/i);
  assert.match(C.parse("0 9-5 * * *").error, /reversed/);
  assert.match(C.parse("@sometimes").error, /shorthand/);
  assert.equal(C.parse("").ok, false);
});

test("describe reads simple schedules aloud and stays quiet otherwise", () => {
  assert.equal(C.describe("30 8 * * 1-5"), "Monday to Friday at 8:30.");
  assert.equal(C.describe("0 7,19 * * *"), "Every day at 7:00 and 19:00.");
  assert.equal(C.describe("15 * * * 6"), "Saturdays every hour at :15.");
  assert.equal(C.describe("0 9 * * 1-5"), "Monday to Friday at 9:00.");
  assert.equal(C.describe("*/7 * 3 * *"), null);
  assert.equal(C.describe("nope"), null);
});

test("nextRuns walks local time and skips ambiguous day rules", () => {
  // Wednesday 2026-10-07 10:30 local.
  const from = new Date(2026, 9, 7, 10, 30, 0);
  const stamp = (d) => [d.getDay(), d.getDate(), d.getHours(), d.getMinutes()];
  assert.deepEqual(C.nextRuns("0 9 * * 1-5", from, 3).map(stamp), [[4, 8, 9, 0], [5, 9, 9, 0], [1, 12, 9, 0]]);
  assert.deepEqual(C.nextRuns("0 */4 * * *", from, 2).map(stamp), [[3, 7, 12, 0], [3, 7, 16, 0]]);
  assert.deepEqual(C.nextRuns("30 10 * * *", from, 1).map(stamp), [[4, 8, 10, 30]]);
  assert.deepEqual(C.nextRuns("0 0 1 * *", from, 1).map(stamp), [[0, 1, 0, 0]]);
  assert.deepEqual(C.nextRuns("0 0 1 * 1", from, 2), []);
  assert.deepEqual(C.nextRuns("bogus", from, 2), []);
});

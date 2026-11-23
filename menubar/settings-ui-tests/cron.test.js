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

const values = (expr) => C.parse(expr).fields.map((set) => Array.from(set).sort((a, b) => a - b));

test("every field accepts its full range and refuses one step outside it", () => {
  assert.deepEqual(values("0 0 1 1 0"), [[0], [0], [1], [1], [0]]);
  assert.deepEqual(values("59 23 31 12 6"), [[59], [23], [31], [12], [6]]);
  const all = C.parse("* * * * *");
  assert.deepEqual(all.fields.map((set) => set.size), [60, 24, 31, 12, 7]);
  assert.deepEqual(all.any, [true, true, true, true, true]);
  assert.deepEqual(C.parse("59 23 31 12 6").any, [false, false, false, false, false]);
  assert.equal(C.parse("60 * * * *").error, "Minute must be 0–59, not 60");
  assert.equal(C.parse("* 24 * * *").error, "Hour must be 0–23, not 24");
  assert.equal(C.parse("* * 0 * *").error, "Day of month must be 1–31, not 0");
  assert.equal(C.parse("* * 32 * *").error, "Day of month must be 1–31, not 32");
  assert.equal(C.parse("* * * 0 *").error, "Month must be 1–12, not 0");
  assert.equal(C.parse("* * * 13 *").error, "Month must be 1–12, not 13");
  assert.equal(C.parse("* * * * 8").error, "Day of week must be 0–7, not 8");
});

test("lists, ranges, steps and names expand to the values they mean", () => {
  assert.deepEqual(values("1,3,5 * * * *")[0], [1, 3, 5]);
  assert.deepEqual(values("10-13 * * * *")[0], [10, 11, 12, 13]);
  assert.deepEqual(values("*/20 * * * *")[0], [0, 20, 40]);
  // A start with a step runs to the end of the field.
  assert.deepEqual(values("1/20 * * * *")[0], [1, 21, 41]);
  assert.deepEqual(values("0 0 1-31/10 * *")[2], [1, 11, 21, 31]);
  // A step larger than the field leaves only its start.
  assert.deepEqual(values("*/61 * * * *")[0], [0]);
  assert.deepEqual(values("0 0 * feb-apr *")[3], [2, 3, 4]);
  assert.deepEqual(values("0 0 * JAN,Dec *")[3], [1, 12]);
  assert.deepEqual(values("0 0 * * SUN-TUE")[4], [0, 1, 2]);
  assert.deepEqual(values("0 0 * * */2")[4], [0, 2, 4, 6]);
  // 7 is Sunday, alone or at the end of a range.
  assert.deepEqual(values("0 0 * * 5-7")[4], [0, 5, 6]);
  assert.deepEqual(values("0 0 * * 0,7")[4], [0]);
});

test("whitespace and shorthand case do not matter", () => {
  assert.equal(C.normalize(" 0\t9  * * 1-5 "), "0 9 * * 1-5");
  assert.equal(C.normalize(null), "");
  assert.equal(C.normalize(undefined), "");
  assert.equal(C.normalize("@Weekly"), "0 0 * * 0");
  assert.deepEqual(values("@DAILY"), values("0 0 * * *"));
  assert.deepEqual(values("@midnight"), values("@daily"));
  assert.deepEqual(values("@annually"), values("@yearly"));
  assert.deepEqual(values("@monthly"), values("0 0 1 * *"));
  assert.equal(C.presetFor(" @HOURLY "), C.PRESETS[1]);
});

test("malformed expressions each say what is wrong", () => {
  assert.equal(C.parse(null).error, "Enter a cron expression");
  assert.equal(C.parse("   ").error, "Enter a cron expression");
  assert.equal(C.parse("@reboot").error, "Unknown shorthand @reboot");
  assert.match(C.parse("* * * * * *").error, /5 fields .* this has 6$/);
  assert.match(C.parse("*").error, /this has 1$/);
  assert.equal(C.parse("*/0 * * * *").error, "'*/0' has an invalid step");
  assert.equal(C.parse("*/ * * * *").error, "'*/' has an invalid step");
  assert.equal(C.parse("*/1.5 * * * *").error, "'*/1.5' has an invalid step");
  assert.equal(C.parse("*/-2 * * * *").error, "'*/-2' has an invalid step");
  assert.equal(C.parse("1/2/3 * * * *").error, "'1/2/3' has more than one step");
  assert.equal(C.parse("1,,2 * * * *").error, "Empty entry in the minute field");
  assert.equal(C.parse("1-2-3 * * * *").error, "'1-2-3' is not a valid range");
  assert.equal(C.parse("-5 * * * *").error, "'' is not a valid minute");
  assert.equal(C.parse("a * * * *").error, "'a' is not a valid minute");
  // Month names are not weekday names, and the other way round.
  assert.equal(C.parse("0 0 * mon *").error, "'mon' is not a valid month");
  assert.equal(C.parse("0 0 * * jan").error, "'jan' is not a valid day of week");
  assert.equal(C.parse("0 0 * * fri-mon").error, "Range 'fri-mon' is reversed");
  // A failed parse carries no fields to preview from.
  assert.equal(C.parse("61 * * * *").fields, undefined);
});

test("describe covers each simple shape and gives up on the rest", () => {
  assert.equal(C.describe(""), C.PRESETS[0].detail);
  assert.equal(C.describe(null), C.PRESETS[0].detail);
  // A preset reads as its own detail line, however it is spelled.
  assert.equal(C.describe("@hourly"), "On the hour.");
  assert.equal(C.describe("0  9 * * 1"), "Mondays at 9:00.");
  assert.equal(C.describe("@daily"), "Every day at 0:00.");
  assert.equal(C.describe("0 0 * * 0"), "Sundays at 0:00.");
  assert.equal(C.describe("5 14 * * *"), "Every day at 14:05.");
  assert.equal(C.describe("0 */6 * * *"), "Every day at 0:00, 6:00, 12:00 and 18:00.");
  assert.equal(C.describe("0 * * * 1,3"), "Mondays and Wednesdays every hour, on the hour.");
  assert.equal(C.describe("0 9 * * 6,0"), "Sundays and Saturdays at 9:00.");
  assert.equal(C.describe("0 9 * * MON"), "Mondays at 9:00.");
  // A weekday list that is exactly Monday to Friday reads as the range.
  assert.equal(C.describe("0 12 * * 1,2,3,4,5"), "Monday to Friday at 12:00.");
  // Too many hours, several minutes, or a day/month rule: not spelled out.
  assert.equal(C.describe("0 1,2,3,4,5 * * *"), null);
  assert.equal(C.describe("*/15 * * * *"), null);
  assert.equal(C.describe("0,30 9 * * *"), null);
  assert.equal(C.describe("0 9 1 * *"), null);
  assert.equal(C.describe("0 9 * 6 *"), null);
  assert.equal(C.describe("0 25 * * *"), null);
});

test("nextRuns crosses days, months and years, and stops at the count", () => {
  const stamp = (d) => [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes()];
  // The minute after `from`, never `from` itself, seconds dropped.
  const newYear = C.nextRuns("* * * * *", new Date(2026, 11, 31, 23, 59, 30), 2);
  assert.deepEqual(newYear.map(stamp), [[2027, 1, 1, 0, 0], [2027, 1, 1, 0, 1]]);
  assert.ok(newYear.every((d) => d.getSeconds() === 0 && d.getMilliseconds() === 0));
  const from = new Date(2026, 9, 7, 10, 30, 0);
  assert.deepEqual(C.nextRuns("30 10 7 10 *", from, 1).map(stamp), [[2027, 10, 7, 10, 30]]);
  assert.deepEqual(C.nextRuns("0 0 * * 7", from, 1).map((d) => [d.getDay(), d.getDate()]), [[0, 11]]);
  // Only months that have the day: the 31st skips November.
  assert.deepEqual(C.nextRuns("0 0 31 * *", from, 3).map(stamp), [[2026, 10, 31, 0, 0], [2026, 12, 31, 0, 0], [2027, 1, 31, 0, 0]]);
  // Leap days are found years ahead; a date that never comes yields nothing.
  assert.deepEqual(C.nextRuns("0 0 29 2 *", from, 2).map(stamp), [[2028, 2, 29, 0, 0], [2032, 2, 29, 0, 0]]);
  assert.deepEqual(C.nextRuns("0 0 31 2 *", from, 1), []);
  assert.deepEqual(C.nextRuns("0 9 * * 1-5", from, 0), []);
  assert.equal(C.nextRuns("*/5 * * * *", from, 7).length, 7);
  // `from` is not modified.
  assert.equal(from.getTime(), new Date(2026, 9, 7, 10, 30, 0).getTime());
  // Every timed preset has a next run to show.
  for (const preset of C.PRESETS.slice(1)) assert.equal(C.nextRuns(preset.value, from, 1).length, 1, preset.value);
});

// How many times `run` moves a Date forward (nextRuns steps its cursor with
// these three setters, once per turn of its loop), and what it returned.
function counted(run) {
  const names = ["setDate", "setHours", "setMinutes"];
  const real = names.map((name) => Date.prototype[name]);
  let steps = 0;
  names.forEach((name, i) => { Date.prototype[name] = function (...args) { steps += 1; return real[i].apply(this, args); }; });
  try {
    const result = run();
    return { steps: steps, result: result };
  } finally {
    names.forEach((name, i) => { Date.prototype[name] = real[i]; });
  }
}

test("a date that never comes is ruled out from the fields, without walking the calendar", () => {
  const sets = (days, months) => C.dateExists(new Set(days), new Set(months));
  assert.equal(sets([31], [2]), false);
  assert.equal(sets([30], [2]), false);
  assert.equal(sets([29], [2]), true, "leap days exist");
  assert.equal(sets([31], [4, 6, 9, 11]), false);
  assert.equal(sets([31], [4, 6, 7]), true, "one month with the day is enough");
  assert.equal(sets([30, 31], [2, 4]), true);
  assert.equal(sets([1], [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]), true);
  const from = new Date(2026, 9, 7, 10, 30, 0);
  // The walk never starts: not one step is taken.
  for (const never of ["0 0 31 2 *", "0 0 30,31 feb *", "*/5 * 31 4,6,9,11 *", "0 0 31 feb,apr *"]) {
    const walk = counted(() => C.nextRuns(never, from, 3));
    assert.deepEqual(walk.result, [], never);
    assert.equal(walk.steps, 0, never);
  }
  // Rare dates are still found, in a walk of days rather than minutes: two
  // steps for each day skipped (to the next day, then to its midnight) and a
  // handful on the day itself.
  const stamp = (d) => [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes()];
  const perYear = 2 * 366;
  const leap = counted(() => C.nextRuns("0 0 29 2 *", from, 2));
  assert.deepEqual(leap.result.map(stamp), [[2028, 2, 29, 0, 0], [2032, 2, 29, 0, 0]]);
  assert.ok(leap.steps < 6 * perYear, leap.steps + " steps");
  // The longest wait there is: 2100 is not a leap year.
  const century = counted(() => C.nextRuns("30 6 29 2 *", new Date(2096, 2, 1, 0, 0, 0), 1));
  assert.deepEqual(century.result.map(stamp), [[2104, 2, 29, 6, 30]]);
  assert.ok(century.steps < 8 * perYear + 100, century.steps + " steps");
  assert.deepEqual(C.nextRuns("0 12 31 4,6,8 *", from, 2).map(stamp), [[2027, 8, 31, 12, 0], [2028, 8, 31, 12, 0]]);
});

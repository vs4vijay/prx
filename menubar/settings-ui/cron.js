// Pure helpers for the schedule picker (no DOM): the preset list, a small
// 5-field cron parser used only to describe an expression and preview its
// next runs. The daemon stays the authority on validity (config.rs
// parse_schedule) — anything this cannot read is simply not previewed.
(function (root) {
  "use strict";

  // value null = no schedule (reviews follow the poll interval).
  const PRESETS = [
    { value: null, label: "With every poll", detail: "No schedule. Reviews start as soon as a PR is due." },
    { value: "0 * * * *", label: "Every hour", detail: "On the hour." },
    { value: "0 */2 * * *", label: "Every 2 hours", detail: "On even hours." },
    { value: "0 */4 * * *", label: "Every 4 hours", detail: "At 0:00, 4:00, 8:00 and so on." },
    { value: "0 9 * * 1-5", label: "Weekday mornings", detail: "Monday to Friday at 9:00." },
    { value: "0 9,14 * * 1-5", label: "Twice each weekday", detail: "Monday to Friday at 9:00 and 14:00." },
    { value: "0 9 * * *", label: "Every morning", detail: "Daily at 9:00." },
    { value: "0 9 * * 1", label: "Weekly", detail: "Mondays at 9:00." },
  ];

  const SHORTHAND = {
    "@hourly": "0 * * * *",
    "@daily": "0 0 * * *",
    "@midnight": "0 0 * * *",
    "@weekly": "0 0 * * 0",
    "@monthly": "0 0 1 * *",
    "@yearly": "0 0 1 1 *",
    "@annually": "0 0 1 1 *",
  };
  const MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
  const DAYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
  const DAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
  const FIELDS = [
    { name: "minute", min: 0, max: 59 },
    { name: "hour", min: 0, max: 23 },
    { name: "day of month", min: 1, max: 31 },
    { name: "month", min: 1, max: 12, names: MONTHS, offset: 1 },
    { name: "day of week", min: 0, max: 7, names: DAYS, offset: 0 },
  ];

  function normalize(expr) {
    const text = String(expr === null || expr === undefined ? "" : expr).trim().replace(/\s+/g, " ");
    return SHORTHAND[text.toLowerCase()] || text;
  }

  function number(text, spec) {
    const lower = text.toLowerCase();
    if (spec.names) {
      const at = spec.names.indexOf(lower);
      if (at !== -1) return at + spec.offset;
    }
    if (!/^\d+$/.test(text)) throw new Error("'" + text + "' is not a valid " + spec.name);
    const value = Number(text);
    if (value < spec.min || value > spec.max) {
      throw new Error(spec.name + " must be " + spec.min + "–" + spec.max + ", not " + text);
    }
    return value;
  }

  function parseField(text, spec) {
    const values = new Set();
    text.split(",").forEach(function (item) {
      if (!item) throw new Error("empty entry in the " + spec.name + " field");
      const parts = item.split("/");
      if (parts.length > 2) throw new Error("'" + item + "' has more than one step");
      const step = parts.length === 2 ? Number(parts[1]) : 1;
      if (!Number.isInteger(step) || step < 1) throw new Error("'" + item + "' has an invalid step");
      let start;
      let end;
      if (parts[0] === "*") {
        start = spec.min;
        end = spec.name === "day of week" ? 6 : spec.max;
      } else if (parts[0].indexOf("-") !== -1) {
        const range = parts[0].split("-");
        if (range.length !== 2) throw new Error("'" + item + "' is not a valid range");
        start = number(range[0], spec);
        end = number(range[1], spec);
        if (start > end) throw new Error("range '" + parts[0] + "' is reversed");
      } else {
        start = number(parts[0], spec);
        end = parts.length === 2 ? (spec.name === "day of week" ? 6 : spec.max) : start;
      }
      for (let v = start; v <= end; v += step) values.add(spec.name === "day of week" ? v % 7 : v);
    });
    return values;
  }

  // -> { ok: true, fields: [Set x5], any: [bool x5] } | { ok: false, error }
  function parse(expr) {
    const text = normalize(expr);
    if (!text) return { ok: false, error: "Enter a cron expression" };
    if (text[0] === "@") return { ok: false, error: "Unknown shorthand " + text };
    const parts = text.split(" ");
    if (parts.length !== 5) {
      return { ok: false, error: "Cron needs 5 fields (minute hour day month weekday); this has " + parts.length };
    }
    try {
      return {
        ok: true,
        fields: parts.map(function (part, i) { return parseField(part, FIELDS[i]); }),
        any: parts.map(function (part) { return part === "*"; }),
      };
    } catch (error) {
      return { ok: false, error: error.message[0].toUpperCase() + error.message.slice(1) };
    }
  }

  function presetFor(value) {
    if (value === null || value === undefined || String(value).trim() === "") return PRESETS[0];
    const text = normalize(value);
    return PRESETS.find(function (p) { return p.value === text; }) || null;
  }

  function pad(n) { return (n < 10 ? "0" : "") + n; }

  function joinList(items) {
    if (items.length < 2) return items.join("");
    return items.slice(0, -1).join(", ") + " and " + items[items.length - 1];
  }

  function sorted(set) { return Array.from(set).sort(function (a, b) { return a - b; }); }

  // A plain-language reading of the expression, or null when it is not one
  // of the simple shapes worth spelling out.
  function describe(expr) {
    const preset = presetFor(expr);
    if (preset) return preset.detail;
    const parsed = parse(expr);
    if (!parsed.ok) return null;
    const minutes = sorted(parsed.fields[0]);
    const hours = sorted(parsed.fields[1]);
    const days = sorted(parsed.fields[4]);
    if (!parsed.any[2] || !parsed.any[3]) return null;
    let when;
    if (minutes.length === 1 && hours.length <= 4 && !parsed.any[1]) {
      when = "at " + joinList(hours.map(function (h) { return h + ":" + pad(minutes[0]); }));
    } else if (minutes.length === 1 && parsed.any[1]) {
      when = minutes[0] === 0 ? "every hour, on the hour" : "every hour at :" + pad(minutes[0]);
    } else {
      return null;
    }
    let which = "Every day";
    if (!parsed.any[4]) {
      const weekdays = days.join(",") === "1,2,3,4,5";
      which = weekdays ? "Monday to Friday" : joinList(days.map(function (d) { return DAY_NAMES[d] + "s"; }));
    }
    return which + " " + when + ".";
  }

  // The next `count` local times the schedule fires after `from`. Empty when
  // the expression restricts both day fields (cron dialects disagree on
  // whether those combine with AND or OR) or cannot be parsed.
  function nextRuns(expr, from, count) {
    const parsed = parse(expr);
    if (!parsed.ok || (!parsed.any[2] && !parsed.any[4])) return [];
    const f = parsed.fields;
    const runs = [];
    const at = new Date(from.getTime());
    at.setSeconds(0, 0);
    at.setMinutes(at.getMinutes() + 1);
    // One year of minutes, skipping whole days/hours that cannot match.
    for (let guard = 0; guard < 600000 && runs.length < count; guard++) {
      if (!f[3].has(at.getMonth() + 1) || !f[2].has(at.getDate()) || !f[4].has(at.getDay())) {
        at.setDate(at.getDate() + 1);
        at.setHours(0, 0, 0, 0);
      } else if (!f[1].has(at.getHours())) {
        at.setHours(at.getHours() + 1, 0, 0, 0);
      } else if (!f[0].has(at.getMinutes())) {
        at.setMinutes(at.getMinutes() + 1);
      } else {
        runs.push(new Date(at.getTime()));
        at.setMinutes(at.getMinutes() + 1);
      }
    }
    return runs;
  }

  const api = { PRESETS: PRESETS, normalize: normalize, parse: parse, presetFor: presetFor, describe: describe, nextRuns: nextRuns };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.prxCron = Object.freeze(api);
})(this);

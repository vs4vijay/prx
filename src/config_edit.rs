//! `prx config set <key> <value>` and `prx config edit` (spec §3.8).
//!
//! `set` navigates a dotted key path over the serialized config
//! (`defaults.poll_interval_minutes`, `notification_tiers.urgent.banner`,
//! `repos.<org/repo>.engine`, `engines.pi.args[0]`, …) so it works uniformly on
//! typed keys and future keys captured by the config's pass-through maps, and
//! never drops unknown keys. List elements are addressed with a `[N]` suffix
//! on the last segment: an index equal to the list's length appends, an
//! existing index replaces in place, and a gapped index is rejected. Values
//! are parsed as YAML scalars (`true`, `30`, `"some repo"` quoted for strings
//! that look like other types) — except prx's own `{{TEMPLATE}}` syntax,
//! which is always taken as a literal string. Legacy key paths (`agents.…`,
//! `defaults.agent`, `repos.<org/repo>.agent`) are rewritten to their
//! `engines`/`engine` form (design D1), and a fresh `engines.<name>` entry
//! starts as `type: agent`.
//!
//! After every set the result is round-tripped through the typed config and
//! the assignment is re-verified on the re-serialized value, so an update
//! that would be silently dropped by the schema (or lost to a duplicate map)
//! fails loudly instead of reaching disk.

use std::path::Path;

use serde_yaml::Value;

use crate::config::Config;

/// One dotted-path segment: a map key plus optional trailing `[N]` list
/// indices (`args[0]`, `branches[1]`, or nested `m[0][1]`).
struct Segment {
    key: String,
    indices: Vec<usize>,
}

/// Set `key` (dotted path with optional `[N]` indices) to `raw_value` on
/// `config` in memory. The caller validates and saves afterwards, so an
/// invalid resulting configuration never reaches disk. Repo overrides use
/// `repos.<org/repo>.<key>` and the repo entry is created on demand
/// (mirroring `prx repos add`).
pub fn set_value(config: &mut Config, key: &str, raw_value: &str) -> Result<(), String> {
    let value = parse_value(raw_value)?;
    if key.trim().is_empty() {
        return Err("key must be a dotted path, e.g. defaults.engine".to_string());
    }
    let mut segments: Vec<Segment> = key
        .split('.')
        .map(parse_segment)
        .collect::<Result<_, _>>()?;
    if segments.len() < 2 {
        return Err(format!(
            "key '{key}' needs at least two segments, e.g. defaults.engine or repos.org/repo.engine"
        ));
    }
    alias_legacy_segments(&mut segments);
    let mut root = serde_yaml::to_value(&*config)
        .map_err(|e| format!("failed to serialize the configuration: {e}"))?;

    if segments[0].key == "repos" && segments[0].indices.is_empty() {
        if segments.len() == 2 {
            return Err(
                "cannot set a whole repo entry; use `prx repos add <org/repo>` and \
                 repos.<org/repo>.<key> for overrides"
                    .to_string(),
            );
        }
        let repo_name = segments[1].key.clone();
        set_in_repo(&mut root, &repo_name, &segments[2..], &value)?;
    } else {
        if segments[0].key == "engines" && segments.len() > 2 && segments[1].indices.is_empty() {
            seed_engine_entry(&mut root, &segments[1].key);
        }
        set_in_maps(&mut root, &segments, &value)?;
    }

    *config = serde_yaml::from_value(root)
        .map_err(|e| format!("resulting configuration does not round-trip: {e}"))?;
    verify_applied(config, key, &segments, &value)
}

/// Rewrite legacy key paths to the engines form (design D1): `agents.<name>…`
/// → `engines.<name>…`, and a level's `agent` → `engine`; and a level's
/// `executive_summary…` → `brief…` (design B5).
fn alias_legacy_segments(segments: &mut [Segment]) {
    if segments[0].key == "agents" {
        segments[0].key = "engines".to_string();
    }
    let level_key = match segments[0].key.as_str() {
        "defaults" if segments.len() == 2 => Some(1),
        "repos" if segments.len() == 3 => Some(2),
        _ => None,
    };
    if let Some(i) = level_key {
        if segments[i].key == "agent" && segments[i].indices.is_empty() {
            segments[i].key = "engine".to_string();
        }
    }
    // Design B5: the deprecated `executive_summary` section is `brief`.
    let section = match segments[0].key.as_str() {
        "defaults" if segments.len() >= 2 => Some(1),
        "repos" if segments.len() >= 3 => Some(2),
        "orgs" if segments.len() >= 2 && !segments[0].indices.is_empty() => Some(1),
        _ => None,
    };
    if let Some(i) = section {
        if segments[i].key == crate::config::LEGACY_BRIEF_KEY && segments[i].indices.is_empty() {
            segments[i].key = "brief".to_string();
        }
    }
}

/// A fresh `engines.<name>` entry starts as an agent engine, so setting
/// `engines.<name>.command` alone yields a parseable entry.
fn seed_engine_entry(root: &mut Value, name: &str) {
    let Some(map) = root.as_mapping_mut() else {
        return;
    };
    let engines = map
        .entry(Value::String("engines".to_string()))
        .or_insert(Value::Mapping(Default::default()));
    if let Some(engines) = engines.as_mapping_mut() {
        let key = Value::String(name.to_string());
        if !engines.contains_key(&key) {
            let mut entry = serde_yaml::Mapping::new();
            entry.insert(
                Value::String("type".to_string()),
                Value::String("agent".to_string()),
            );
            engines.insert(key, Value::Mapping(entry));
        }
    }
}

/// Parse the raw value as YAML — except prx's own `{{TEMPLATE}}` syntax
/// (`{{PROMPT}}`, `{{MANIFEST_PATH}}`, …), which is never meaningful YAML
/// (it would parse as a flow mapping) and is taken as a literal string. A
/// well-formed `{{…}}` pair anywhere in the value opts in; anything else
/// must be valid YAML.
fn parse_value(raw_value: &str) -> Result<Value, String> {
    if raw_value.contains("{{") && raw_value.contains("}}") {
        return Ok(Value::String(raw_value.to_string()));
    }
    serde_yaml::from_str(raw_value).map_err(|e| format!("invalid value '{raw_value}': {e}"))
}

fn parse_segment(raw: &str) -> Result<Segment, String> {
    let (key, mut rest) = match raw.split_once('[') {
        Some((key, rest)) => (key, Some(rest)),
        None => (raw, None),
    };
    if key.is_empty() {
        return Err(format!("empty key segment in '{raw}'"));
    }
    let mut indices = Vec::new();
    while let Some(current) = rest {
        let Some((index_text, after)) = current.split_once(']') else {
            return Err(format!("unterminated '[' in key segment '{raw}'"));
        };
        let index: usize = index_text
            .parse()
            .map_err(|_| format!("'{index_text}' is not a valid list index in '{raw}'"))?;
        indices.push(index);
        rest = match after.strip_prefix('[') {
            Some(next) => Some(next),
            None if after.is_empty() => None,
            None => {
                return Err(format!(
                    "unexpected text '{after}' after an index in '{raw}'"
                ))
            }
        };
    }
    Ok(Segment {
        key: key.to_string(),
        indices,
    })
}

fn mapping_entry_mut<'a>(node: &'a mut Value, key: &str) -> &'a mut Value {
    let map = node
        .as_mapping_mut()
        .expect("traversal only creates mappings");
    let key = Value::String(key.to_string());
    map.entry(key).or_insert(Value::Mapping(Default::default()))
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Sequence(_) => "a list",
        Value::Mapping(_) => "a mapping",
        Value::Tagged(_) => "a tagged value",
    }
}

/// Walk `segments` from `node`, creating intermediate mappings, and set the
/// leaf (a plain value or, when the last segment carries `[N]`, a list
/// element).
fn set_in_maps(node: &mut Value, segments: &[Segment], value: &Value) -> Result<(), String> {
    let mut current = node;
    for segment in &segments[..segments.len() - 1] {
        if !segment.indices.is_empty() {
            return Err(format!(
                "only the last key segment may index a list ('{}[{}]')",
                segment.key, segment.indices[0]
            ));
        }
        let next = mapping_entry_mut(current, &segment.key);
        if !next.is_mapping() {
            return Err(format!(
                "key segment '{}' already holds {}; cannot descend into it",
                segment.key,
                kind_of(next)
            ));
        }
        current = next;
    }
    let last = segments.last().expect("checked non-empty by caller");
    if last.indices.is_empty() {
        // Fresh leaf keys are fine anywhere (that is how overrides start).
        current
            .as_mapping_mut()
            .expect("traversal only creates mappings")
            .insert(Value::String(last.key.clone()), value.clone());
        Ok(())
    } else {
        assign_in_sequence(current, last, value)
    }
}

/// Assign `segment.key[indices…] = value` where `map` is the parent mapping.
/// A missing (or null) key becomes an empty list; an index equal to the
/// length appends, an existing index replaces, a gapped index is rejected.
fn assign_in_sequence(map: &mut Value, segment: &Segment, value: &Value) -> Result<(), String> {
    let key = Value::String(segment.key.clone());
    {
        let entries = map
            .as_mapping_mut()
            .expect("traversal only creates mappings");
        if matches!(entries.get(&key), None | Some(Value::Null)) {
            entries.insert(key.clone(), Value::Sequence(Vec::new()));
        }
    }
    let mut cursor: &mut Value = map
        .as_mapping_mut()
        .expect("traversal only creates mappings")
        .get_mut(&key)
        .expect("just ensured the key exists");
    for (depth, &index) in segment.indices.iter().enumerate() {
        let is_last = depth + 1 == segment.indices.len();
        match step_into_list(cursor, &segment.key, index, is_last, value)? {
            Some(next) => cursor = next,
            None => return Ok(()), // step_into_list appended the value
        }
    }
    // The final index addressed an existing element: replace it in place.
    *cursor = value.clone();
    Ok(())
}

/// One indexed step into the list at `cursor`. `Ok(None)` means the value was
/// appended (the walk is done); `Ok(Some(next))` descends into the element.
fn step_into_list<'a>(
    cursor: &'a mut Value,
    label: &str,
    index: usize,
    is_last: bool,
    value: &Value,
) -> Result<Option<&'a mut Value>, String> {
    if !cursor.is_sequence() {
        return Err(format!(
            "'{label}' already holds {}; indexed assignment needs a list",
            kind_of(cursor)
        ));
    }
    let list = cursor.as_sequence_mut().expect("checked sequence");
    let len = list.len();
    if index > len {
        return Err(format!(
            "index {index} is out of range for '{label}' (length {len}); \
             set '{label}[{len}]' next to append without gaps"
        ));
    }
    if index == len {
        if !is_last {
            return Err(format!(
                "cannot create '{label}[{index}]': the inner list does not exist yet"
            ));
        }
        list.push(value.clone());
        return Ok(None);
    }
    Ok(Some(&mut list[index]))
}

/// `repos.<org/repo>.<key…>`: find (or append) the repo entry by `name`,
/// then set within it.
fn set_in_repo(
    root: &mut Value,
    repo_name: &str,
    rest: &[Segment],
    value: &Value,
) -> Result<(), String> {
    if !root.is_mapping() {
        return Err("config root is not a mapping".to_string());
    }
    let repos = mapping_entry_mut(root, "repos");
    if !repos.is_sequence() {
        return Err(format!(
            "'repos' already holds {}; the configuration is malformed",
            kind_of(repos)
        ));
    }
    let seq = repos.as_sequence_mut().expect("checked sequence");
    let name_key = Value::String("name".to_string());
    // Find the repo entry by `name`, appending a fresh one when absent.
    let found = seq.iter().position(|item| {
        item.as_mapping()
            .and_then(|m| m.get(&name_key))
            .and_then(|v| v.as_str())
            .is_some_and(|name| name == repo_name)
    });
    let index = match found {
        Some(index) => index,
        None => {
            let mut entry = serde_yaml::Mapping::new();
            entry.insert(name_key, Value::String(repo_name.to_string()));
            seq.push(Value::Mapping(entry));
            seq.len() - 1
        }
    };
    set_in_maps(&mut seq[index], rest, value)
}

/// Confirm the assignment survived the round-trip through the typed config:
/// every segment must resolve on the re-serialized value and the leaf must
/// equal the requested value. A key the schema has no capture map for would
/// otherwise be dropped silently.
fn verify_applied(
    config: &Config,
    key: &str,
    segments: &[Segment],
    value: &Value,
) -> Result<(), String> {
    let root = serde_yaml::to_value(config)
        .map_err(|e| format!("failed to serialize the configuration: {e}"))?;
    let mut cursor: &Value = &root;

    // `repos.<name>.…` walks the repo list by its `name` field.
    let mut start = 0usize;
    if segments[0].key == "repos" && segments[0].indices.is_empty() && segments.len() > 2 {
        let repos = root
            .as_mapping()
            .and_then(|m| m.get(Value::String("repos".into())))
            .and_then(|v| v.as_sequence())
            .ok_or_else(|| format!("setting '{key}' did not take effect: 'repos' is not a list"))?;
        cursor = repos
            .iter()
            .find(|entry| {
                entry
                    .get(Value::String("name".into()))
                    .and_then(|v| v.as_str())
                    == Some(segments[1].key.as_str())
            })
            .ok_or_else(|| {
                format!("setting '{key}' did not take effect: the repo entry is missing")
            })?;
        start = 2;
    }
    for segment in &segments[start..] {
        cursor = cursor
            .get(Value::String(segment.key.clone()))
            .ok_or_else(|| {
                format!(
                "setting '{key}' did not take effect: '{}' was dropped by the configuration schema",
                segment.key
            )
            })?;
        for &index in &segment.indices {
            cursor = cursor
                .as_sequence()
                .and_then(|list| list.get(index))
                .ok_or_else(|| {
                    format!("setting '{key}' did not take effect: element [{index}] is missing")
                })?;
        }
    }
    if cursor != value {
        return Err(format!(
            "setting '{key}' did not take effect: the stored value does not match the request"
        ));
    }
    Ok(())
}

/// Open `path` in the given editor command (argv words allowed, git-style).
pub fn open_with(path: &Path, editor: &str) -> Result<(), String> {
    let mut words = editor.split_whitespace();
    let (Some(program), args) = (words.next(), words.collect::<Vec<_>>()) else {
        return Err(format!("$EDITOR '{editor}' is empty"));
    };
    let status = std::process::Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .map_err(|e| format!("cannot run editor '{program}': {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("editor '{program}' exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).unwrap()
    }

    fn agent<'a>(cfg: &'a Config, name: &str) -> &'a crate::config::AgentConfig {
        cfg.engines[name].as_agent().expect("agent engine")
    }

    #[test]
    fn set_overrides_typed_defaults() {
        let mut cfg = config(
            "defaults:\n  agent: pi\n  poll_interval_minutes: 15\nrepos:\n  - name: org/one\n",
        );
        set_value(&mut cfg, "defaults.poll_interval_minutes", "30").unwrap();
        set_value(&mut cfg, "defaults.agent", "claude").unwrap();
        set_value(
            &mut cfg,
            "defaults.cooldown.before_first_review_minutes",
            "5",
        )
        .unwrap();
        set_value(&mut cfg, "defaults.skip.drafts", "false").unwrap();
        let repos = cfg.effective_repos().unwrap();
        assert_eq!(repos[0].poll_interval_minutes, 30);
        assert_eq!(repos[0].engine, "claude");
        assert_eq!(repos[0].cooldown.before_first_review_minutes, 5);
        assert!(!repos[0].skip.drafts);
    }

    #[test]
    fn set_parses_yaml_scalars() {
        let mut cfg = config("defaults:\n  agent: pi\nrepos: []\n");
        set_value(&mut cfg, "defaults.poll_interval_minutes", "45").unwrap();
        set_value(&mut cfg, "defaults.skip.drafts", "true").unwrap();
        // A quoted value stays a string; an unquoted number stays a number.
        set_value(&mut cfg, "defaults.agent", "\"123\"").unwrap();
        assert_eq!(cfg.defaults.engine, "123");
        assert!(cfg.defaults.skip.drafts);
        let saved = serde_yaml::to_string(&cfg).unwrap();
        assert!(saved.contains("45"), "{saved}");
    }

    #[test]
    fn set_creates_repo_override_entry_on_demand() {
        let mut cfg = config("defaults:\n  agent: pi\nrepos:\n  - name: org/one\n");
        set_value(&mut cfg, "repos.org/two.agent", "claude").unwrap();
        set_value(&mut cfg, "repos.org/one.poll_interval_minutes", "5").unwrap();
        assert_eq!(cfg.repos.len(), 2);
        let two = cfg.repos.iter().find(|r| r.name == "org/two").unwrap();
        assert_eq!(two.overrides.engine.as_deref(), Some("claude"));
        let repos = cfg.effective_repos().unwrap();
        assert_eq!(repos[0].poll_interval_minutes, 5);
        assert_eq!(repos[1].engine, "claude");
    }

    #[test]
    fn set_preserves_unknown_keys() {
        let mut cfg = config(
            "defaults:\n  agent: pi\nfuture_thing:\n  on: true\nrepos:\n  - name: org/a\n    weird: 7\n",
        );
        set_value(&mut cfg, "defaults.agent", "claude").unwrap();
        assert_eq!(cfg.extra["future_thing"]["on"], Value::Bool(true));
        assert_eq!(
            cfg.repos[0].overrides.extra["weird"],
            Value::Number(7.into())
        );
    }

    #[test]
    fn set_rejects_bad_paths_and_values() {
        let mut cfg = config("defaults:\n  agent: pi\nrepos: []\n");
        assert!(set_value(&mut cfg, "agent", "x").is_err());
        assert!(set_value(&mut cfg, "repos", "x").is_err());
        assert!(set_value(&mut cfg, "repos.org/one", "x").is_err());
        assert!(set_value(&mut cfg, "defaults.agent", "{{{").is_err());
        // A scalar cannot be descended into.
        assert!(set_value(&mut cfg, "defaults.agent.sub", "x").is_err());
    }

    #[test]
    fn set_takes_template_syntax_as_a_literal_string() {
        // `{{PROMPT}}` would otherwise parse as a nested YAML flow mapping.
        let mut cfg =
            config("defaults:\n  agent: pi\nagents:\n  pi:\n    command: fake\n    args: []\n");
        set_value(&mut cfg, "agents.pi.args[0]", "{{PROMPT}}").unwrap();
        set_value(&mut cfg, "agents.pi.command", "run {{MANIFEST_PATH}}").unwrap();
        assert_eq!(agent(&cfg, "pi").args, vec!["{{PROMPT}}".to_string()]);
        assert_eq!(agent(&cfg, "pi").command, "run {{MANIFEST_PATH}}");
    }

    #[test]
    fn set_indexed_elements_append_replace_and_never_duplicate() {
        let mut cfg = config("defaults:\n  agent: pi\nagents:\n  pi:\n    command: fake\n");
        // Missing list + index 0 creates it.
        set_value(&mut cfg, "agents.pi.args[0]", "{{PROMPT}}").unwrap();
        assert_eq!(agent(&cfg, "pi").args, vec!["{{PROMPT}}".to_string()]);
        // Index == length appends.
        set_value(&mut cfg, "agents.pi.args[1]", "-p").unwrap();
        assert_eq!(
            agent(&cfg, "pi").args,
            vec!["{{PROMPT}}".to_string(), "-p".to_string()]
        );
        // Existing index replaces in place.
        set_value(&mut cfg, "agents.pi.args[0]", "exec").unwrap();
        assert_eq!(
            agent(&cfg, "pi").args,
            vec!["exec".to_string(), "-p".to_string()]
        );
        // One serialized map, updated in place — never a duplicate key.
        let saved = serde_yaml::to_string(&cfg).unwrap();
        assert_eq!(
            saved.lines().filter(|l| l.starts_with("engines:")).count(),
            1,
            "{saved}"
        );
    }

    #[test]
    fn set_indexed_rejects_gaps_and_bad_targets_cleanly() {
        let mut cfg = config("defaults:\n  agent: pi\nagents:\n  pi:\n    command: fake\n");
        // Sparse index: the list does not have 3 elements yet.
        let err = set_value(&mut cfg, "agents.pi.args[2]", "x").unwrap_err();
        assert!(err.contains("out of range"), "{err}");
        assert!(agent(&cfg, "pi").args.is_empty(), "{:?}", agent(&cfg, "pi"));
        // Indexing something that is not a list.
        let err = set_value(&mut cfg, "agents.pi.command[0]", "x").unwrap_err();
        assert!(err.contains("needs a list"), "{err}");
        // Malformed index syntax.
        assert!(set_value(&mut cfg, "agents.pi.args[x]", "1").is_err());
        assert!(set_value(&mut cfg, "agents.pi.args[1", "1").is_err());
        // Indices on intermediate segments are rejected.
        assert!(set_value(&mut cfg, "agents.pi.args[0].sub", "1").is_err());
    }

    /// Design D1: legacy key paths land on the engines form, and a fresh
    /// `engines.<name>` entry starts as an agent engine.
    #[test]
    fn legacy_agent_paths_alias_to_engines() {
        let mut cfg = config("defaults:\n  agent: pi\nrepos:\n  - name: org/one\n");
        set_value(&mut cfg, "defaults.agent", "claude").unwrap();
        set_value(&mut cfg, "repos.org/one.agent", "codex").unwrap();
        set_value(&mut cfg, "agents.mine.command", "my-agent").unwrap();
        set_value(&mut cfg, "engines.other.command", "other").unwrap();
        assert_eq!(cfg.defaults.engine, "claude");
        assert_eq!(cfg.repos[0].overrides.engine.as_deref(), Some("codex"));
        assert_eq!(agent(&cfg, "mine").command, "my-agent");
        assert_eq!(agent(&cfg, "other").command, "other");
        let saved = serde_yaml::to_string(&cfg).unwrap();
        assert!(!saved.contains("agents:"), "{saved}");
        assert!(!saved.contains(" agent:"), "{saved}");

        set_value(&mut cfg, "engines.cheap.type", "llm_api").unwrap_err();
        set_value(&mut cfg, "defaults.comments.inline", "false").unwrap();
        assert!(!cfg.defaults.comments.inline);
    }

    /// Design B5: edits always write `brief` — a legacy section in the file
    /// is migrated on the way, and legacy key paths alias to `brief`.
    #[test]
    fn edits_write_brief_only() {
        let mut cfg = config(
            "defaults:\n  executive_summary:\n    timeout_seconds: 60\nrepos:\n  - name: o/r\n    \
             executive_summary:\n      enabled: false\n",
        );
        set_value(&mut cfg, "defaults.brief.enabled", "false").unwrap();
        assert!(!cfg.defaults.brief.enabled);
        assert_eq!(cfg.defaults.brief.timeout_seconds, 60);
        set_value(&mut cfg, "defaults.executive_summary.timeout_seconds", "90").unwrap();
        assert_eq!(cfg.defaults.brief.timeout_seconds, 90);
        set_value(
            &mut cfg,
            "repos.o/r.executive_summary.timeout_seconds",
            "30",
        )
        .unwrap();
        let repo = cfg.repos[0].overrides.brief.clone().unwrap();
        assert_eq!(repo.enabled, Some(false));
        assert_eq!(repo.timeout_seconds, Some(30));
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(!yaml.contains("executive_summary"), "{yaml}");
        assert_eq!(yaml.matches("brief:").count(), 2, "{yaml}");
    }

    #[test]
    fn set_reports_dropped_keys_instead_of_losing_them() {
        // The schema has no capture map for unknown keys under
        // `notification_tiers`, so a bogus tier must fail loudly rather than
        // vanish on save.
        let mut cfg = config("defaults:\n  agent: pi\n");
        let err = set_value(&mut cfg, "notification_tiers.bogus.banner", "true").unwrap_err();
        assert!(err.contains("did not take effect"), "{err}");
    }

    #[test]
    fn open_with_runs_the_editor_on_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("prx-cfg-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let editor = dir.join("fake-editor");
        std::fs::write(&editor, "#!/bin/sh\necho \"# edited by $1\" >> \"$1\"\n").unwrap();
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("config.yml");
        std::fs::write(&path, "defaults:\n  agent: pi\n").unwrap();

        open_with(&path, editor.to_str().unwrap()).unwrap();
        let edited = std::fs::read_to_string(&path).unwrap();
        assert!(edited.contains("# edited by"), "{edited}");
        // The editor received the file path as its last argument.
        assert!(edited.contains("config.yml"), "{edited}");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&editor);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn open_with_reports_editor_failures() {
        let path = Path::new("/definitely/not/a/file");
        assert!(open_with(path, "true").is_ok()); // `true` ignores its args
        assert!(open_with(path, "definitely-not-a-real-editor").is_err());
        assert!(open_with(path, "").is_err());
    }
}

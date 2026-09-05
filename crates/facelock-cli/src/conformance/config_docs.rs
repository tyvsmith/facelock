//! Compare documented defaults with the real serde configuration, including
//! optional fields omitted from a serialized default via the shipped template.
use std::collections::BTreeMap;

use facelock_core::Config;

const DOC: &str = include_str!("../../../../docs/configuration.md");
const TEMPLATE: &str = include_str!("../../../../config/facelock.toml");

fn flatten(prefix: &str, value: &toml::Value, out: &mut BTreeMap<String, toml::Value>) {
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            let key = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            flatten(&key, value, out);
        }
    } else {
        out.insert(prefix.to_owned(), value.clone());
    }
}

fn defaults() -> BTreeMap<String, toml::Value> {
    let mut result = BTreeMap::new();
    flatten(
        "",
        &toml::Value::try_from(Config::default()).expect("serialize defaults"),
        &mut result,
    );
    result.extend(pam_policy_defaults());
    result
}

/// These two lists are read only by the thin PAM module. Read its field
/// declarations, as man_pam does, to avoid linking a cdylib or changing its
/// dependency boundary. Fail if its simple serde-default shape changes.
fn pam_policy_defaults() -> BTreeMap<String, toml::Value> {
    let source = include_str!("../../../../crates/pam-facelock/src/lib.rs");
    let body = source
        .split_once("struct PamPolicyConfig {")
        .expect("PAM policy type")
        .1
        .split_once('}')
        .expect("PAM policy fields")
        .0;
    let mut result = BTreeMap::new();
    let mut default = false;
    for line in body.lines().map(str::trim) {
        if line == "#[serde(default)]" {
            default = true;
            continue;
        }
        if let Some((name, ty)) = line.split_once(':') {
            assert!(
                default && ty.trim() == "Vec<String>,",
                "PAM policy default shape changed: {line}"
            );
            result.insert(
                format!("security.pam_policy.{name}"),
                toml::Value::Array(Vec::new()),
            );
            default = false;
        }
    }
    assert!(result.len() >= 2, "PAM policy extraction lost its fields");
    result
}

fn documented_defaults(doc: &str) -> BTreeMap<String, String> {
    let mut section = "";
    let mut default_column = None;
    let mut result = BTreeMap::new();
    for line in doc.lines() {
        if line.starts_with('#') {
            default_column = None;
            if let Some(name) = line
                .trim_start_matches('#')
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
            {
                section = name;
            } else {
                section = "";
            }
        }
        if line.starts_with("| Key |") {
            default_column = line
                .split('|')
                .map(str::trim)
                .position(|cell| cell == "Default");
        }
        if !section.is_empty() && line.starts_with("| `") {
            let cells: Vec<_> = line.split('|').map(str::trim).collect();
            if let Some(value) = default_column.and_then(|column| cells.get(column)) {
                result.insert(
                    format!("{section}.{}", cells[1].trim_matches('`')),
                    value.trim_matches('`').to_string(),
                );
            }
        }
    }
    result
}

fn equivalent(actual: &toml::Value, documented: &toml::Value) -> bool {
    match (actual, documented) {
        (toml::Value::Float(a), toml::Value::Float(b)) => (a - b).abs() < 0.000001,
        _ => actual == documented,
    }
}

fn failures(doc: &str) -> Vec<String> {
    let rows = documented_defaults(doc);
    let mut failures = Vec::new();
    for (key, actual) in defaults() {
        let Some(text) = rows.get(&key) else {
            failures.push(format!("{key}: missing table row (default {actual})"));
            continue;
        };
        let parsed = format!("value = {text}").parse::<toml::Table>();
        if !parsed
            .as_ref()
            .ok()
            .and_then(|t| t.get("value"))
            .is_some_and(|documented| equivalent(&actual, documented))
        {
            failures.push(format!("{key}: documented {text}, actual {actual}"));
        }
    }
    failures
}

#[test]
fn configuration_reference_covers_actual_defaults() {
    let failures = failures(DOC);
    assert!(
        failures.is_empty(),
        "docs/configuration.md:\n{}",
        failures.join("\n")
    );
}

#[test]
fn configuration_guard_detects_missing_and_stale_security_defaults() {
    let missing = DOC
        .lines()
        .filter(|line| !line.starts_with("| `require_ir`"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        failures(&missing)
            .iter()
            .any(|failure| failure.starts_with("security.require_ir: missing"))
    );
    let stale = DOC.replace(
        "| `require_ir` | bool | `true`",
        "| `require_ir` | bool | `false`",
    );
    assert!(
        failures(&stale)
            .iter()
            .any(|failure| failure.starts_with("security.require_ir: documented false"))
    );
}

#[test]
fn book_configuration_defaults_agree_with_the_implementation() {
    // The book may omit details, but every default it does state is a
    // behavioral promise, independently checked against Config::default.
    let rows = documented_defaults(include_str!("../../../../book/src/configuration.md"));
    let actual = defaults();
    let mut failures = Vec::new();
    for (key, text) in rows {
        let Some(actual) = actual.get(&key) else {
            continue;
        };
        let parsed = format!("value = {text}").parse::<toml::Table>();
        if !parsed
            .as_ref()
            .ok()
            .and_then(|t| t.get("value"))
            .is_some_and(|documented| equivalent(actual, documented))
        {
            failures.push(format!("{key}: documented {text}, actual {actual}"));
        }
    }
    assert!(
        failures.is_empty(),
        "book/src/configuration.md:\n{}",
        failures.join("\n")
    );
}

#[test]
fn shipped_template_and_reference_cover_the_same_schema() {
    let text = TEMPLATE
        .lines()
        .filter_map(|line| {
            let candidate = line.strip_prefix("# ").unwrap_or(line);
            let assignment = candidate.split_once('=').is_some_and(|(key, _)| {
                !key.trim().is_empty()
                    && key
                        .trim()
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
            }) && candidate.parse::<toml::Table>().is_ok();
            (candidate.starts_with('[') || assignment).then_some(candidate)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let template: toml::Value =
        toml::from_str(&text).expect("commented template assignments are TOML");
    let config: Config = template.clone().try_into().expect("template fits Config");
    let mut schema = BTreeMap::new();
    flatten(
        "",
        &toml::Value::try_from(config).expect("serialize template Config"),
        &mut schema,
    );
    schema.extend(pam_policy_defaults());
    let mut shipped = BTreeMap::new();
    flatten("", &template, &mut shipped);
    let documented = documented_defaults(DOC);
    let actual_defaults = defaults();
    let mut failures = Vec::new();
    for key in schema.keys() {
        if !shipped.contains_key(key) {
            failures.push(format!("{key}: absent from shipped template"));
        }
        if !documented.contains_key(key) {
            failures.push(format!("{key}: absent from docs/configuration.md"));
        }
        if !actual_defaults.contains_key(key)
            && let Some(default) = documented.get(key)
            && default != "unset"
            && !(key == "device.path" && default == "Auto-detect")
        {
            failures.push(format!(
                "{key}: optional field defaults to unset, documented {default}"
            ));
        }
    }
    for key in shipped.keys().chain(documented.keys()) {
        if !schema.contains_key(key) {
            failures.push(format!("{key}: unknown configuration key"));
        }
    }
    assert!(
        failures.is_empty(),
        "configuration schema drift:\n{}",
        failures.join("\n")
    );
}

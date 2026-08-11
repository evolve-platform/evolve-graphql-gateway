//! Environment interpolation for the router config file.
//!
//! Hive Router deliberately does not interpolate `${VAR}` in its YAML — it only
//! honours a fixed allowlist of built-in overrides (`PORT`, `HIVE_CDN_ENDPOINT`,
//! …; see `hive_router_config::EnvVarOverrides`). That is too narrow for an image
//! that is reused across projects, where the config must be driven entirely from
//! the environment.
//!
//! So the whole config file is treated as a template and rendered before
//! hive-router loads it. This replaces the `envsubst` step that used to live in
//! the container entrypoint, and unlike envsubst it needs no allowlist, supports
//! defaults, and fails loudly on a missing variable instead of substituting an
//! empty string.
//!
//! Syntax:
//!
//! | Form                 | Meaning                                            |
//! | -------------------- | -------------------------------------------------- |
//! | `${NAME}`            | required — startup fails if unset                   |
//! | `${NAME:-default}`   | `default` when unset **or empty**                   |
//! | `${NAME-default}`    | `default` when unset (an empty value stays empty)   |
//! | `$${...}`            | literal `${...}`                                    |
//!
//! A `$` that is not followed by `{` is left alone, so VRL expressions and any
//! other literal `$` in the config pass through untouched.

use std::fmt;

/// Everything that can go wrong while rendering a config template.
#[derive(Debug)]
pub enum RenderError {
    /// One or more `${VAR}` references had no value and no default. All of them
    /// are collected so a misconfigured deployment reports every missing
    /// variable at once instead of one per restart.
    MissingVars(Vec<String>),
    /// A `${` that never closed, or one wrapping something that is not a valid
    /// variable name. Silently passing these through would ship a broken config.
    Malformed { offset: usize, reason: String },
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::MissingVars(vars) => write!(
                f,
                "missing required environment variable(s): {}. \
                 Set them, or give the placeholder a default with ${{NAME:-value}}",
                vars.join(", ")
            ),
            RenderError::Malformed { offset, reason } => {
                write!(f, "malformed placeholder at byte {offset}: {reason}")
            }
        }
    }
}

impl std::error::Error for RenderError {}

/// Render `input`, resolving placeholders through `lookup`.
///
/// Split from [`render`] so the substitution rules can be tested without
/// touching the process environment.
pub fn render_with<F>(input: &str, lookup: F) -> Result<String, RenderError>
where
    F: Fn(&str) -> Option<String>,
{
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut missing: Vec<String> = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        // Only `$` is special, and only when it opens a placeholder. Copy the
        // run of ordinary bytes up to the next `$` in one go.
        let Some(rel) = input[i..].find('$') else {
            out.push_str(&input[i..]);
            break;
        };
        out.push_str(&input[i..i + rel]);
        i += rel;

        // `$${` -> literal `${`. A `$$` not followed by `{` is not an escape
        // (nothing to escape), so it is copied verbatim.
        if bytes[i..].starts_with(b"$${") {
            out.push_str("${");
            i += 3;
            continue;
        }

        // A lone `$` (not opening a placeholder) is literal.
        if !bytes[i..].starts_with(b"${") {
            out.push('$');
            i += 1;
            continue;
        }

        let start = i;
        let body_start = i + 2;
        let Some(close_rel) = input[body_start..].find('}') else {
            return Err(RenderError::Malformed {
                offset: start,
                reason: "unterminated '${' (no closing '}')".to_string(),
            });
        };
        let body = &input[body_start..body_start + close_rel];
        i = body_start + close_rel + 1;

        let (name, default) = split_placeholder(body).map_err(|reason| RenderError::Malformed {
            offset: start,
            reason,
        })?;

        match (lookup(name), default) {
            // `${NAME:-d}` treats an empty value as absent; `${NAME-d}` does not.
            (Some(value), Some(Default::WhenUnsetOrEmpty(d))) if value.is_empty() => {
                out.push_str(d)
            }
            (Some(value), _) => out.push_str(&value),
            (None, Some(Default::WhenUnsetOrEmpty(d) | Default::WhenUnset(d))) => out.push_str(d),
            (None, None) => {
                // Keep going so every missing variable is reported together.
                if !missing.iter().any(|v| v == name) {
                    missing.push(name.to_string());
                }
            }
        }
    }

    if !missing.is_empty() {
        return Err(RenderError::MissingVars(missing));
    }

    Ok(out)
}

/// Render `input` against the process environment.
pub fn render(input: &str) -> Result<String, RenderError> {
    render_with(input, |name| std::env::var(name).ok())
}

/// Which flavour of `-` separated the name from its default.
enum Default<'a> {
    /// `${NAME:-value}`
    WhenUnsetOrEmpty(&'a str),
    /// `${NAME-value}`
    WhenUnset(&'a str),
}

/// Split a placeholder body into its variable name and optional default.
fn split_placeholder(body: &str) -> Result<(&str, Option<Default<'_>>), String> {
    let (name, default) = match body.find(":-") {
        Some(idx) => (
            &body[..idx],
            Some(Default::WhenUnsetOrEmpty(&body[idx + 2..])),
        ),
        None => match body.find('-') {
            Some(idx) => (&body[..idx], Some(Default::WhenUnset(&body[idx + 1..]))),
            None => (body, None),
        },
    };

    if !is_valid_name(name) {
        return Err(format!(
            "'{name}' is not a valid environment variable name \
             (expected [A-Za-z_][A-Za-z0-9_]*)"
        ));
    }

    Ok((name, default))
}

fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn substitutes_set_variables() {
        let out = render_with("a: ${FOO}\nb: ${BAR}", env(&[("FOO", "1"), ("BAR", "2")])).unwrap();
        assert_eq!(out, "a: 1\nb: 2");
    }

    #[test]
    fn applies_defaults_for_unset_variables() {
        let out = render_with("a: ${FOO:-4000}", env(&[])).unwrap();
        assert_eq!(out, "a: 4000");
    }

    #[test]
    fn colon_dash_treats_empty_as_unset_but_dash_does_not() {
        assert_eq!(
            render_with("${FOO:-fallback}", env(&[("FOO", "")])).unwrap(),
            "fallback"
        );
        assert_eq!(
            render_with("${FOO-fallback}", env(&[("FOO", "")])).unwrap(),
            ""
        );
    }

    #[test]
    fn default_may_be_empty_or_contain_dashes_and_urls() {
        assert_eq!(render_with("${FOO:-}", env(&[])).unwrap(), "");
        assert_eq!(
            render_with("${FOO:-http://localhost:4318}", env(&[])).unwrap(),
            "http://localhost:4318"
        );
        // Only the FIRST separator splits, so defaults keep their own dashes.
        assert_eq!(render_with("${FOO:-a-b-c}", env(&[])).unwrap(), "a-b-c");
    }

    #[test]
    fn reports_every_missing_variable_at_once() {
        let err = render_with("${A} ${B} ${A}", env(&[])).unwrap_err();
        match err {
            // Deduplicated, and both names present.
            RenderError::MissingVars(vars) => assert_eq!(vars, vec!["A", "B"]),
            other => panic!("expected MissingVars, got {other:?}"),
        }
    }

    #[test]
    fn leaves_lone_dollars_untouched() {
        // VRL and shell-ish snippets in a mounted config must survive intact.
        let src = r#"expression: .request.headers."x-a" != "$500" && $ok"#;
        assert_eq!(render_with(src, env(&[])).unwrap(), src);
    }

    #[test]
    fn double_dollar_escapes_a_literal_placeholder() {
        assert_eq!(
            render_with("literal: $${NOT_A_VAR}", env(&[])).unwrap(),
            "literal: ${NOT_A_VAR}"
        );
    }

    #[test]
    fn substituted_values_are_not_rescanned() {
        // A value that itself looks like a placeholder must not be expanded
        // again — that would leak arbitrary env vars through user input.
        let out = render_with("${FOO}", env(&[("FOO", "${BAR}"), ("BAR", "leaked")])).unwrap();
        assert_eq!(out, "${BAR}");
    }

    #[test]
    fn rejects_unterminated_placeholder() {
        assert!(matches!(
            render_with("a: ${FOO", env(&[])),
            Err(RenderError::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_invalid_variable_name() {
        assert!(matches!(
            render_with("a: ${not a var}", env(&[])),
            Err(RenderError::Malformed { .. })
        ));
    }

    #[test]
    fn handles_multibyte_content_around_placeholders() {
        let out = render_with("héllo ${FOO} wörld", env(&[("FOO", "→")])).unwrap();
        assert_eq!(out, "héllo → wörld");
    }
}

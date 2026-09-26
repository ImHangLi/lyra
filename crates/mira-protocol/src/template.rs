//! `{input.NAME}` placeholders in a command action's `run.argv`.
//!
//! Only arguments that contain `{input.` are templated; every other argument passes
//! unchanged, so existing braces (for example Go templates such as `{{.Names}}`) keep
//! working. In a templated argument, `{{` and `}}` are literal braces. The host replaces
//! each placeholder with the effective input value (after schema defaults): strings as they
//! are, numbers and booleans in their JSON text form. No shell is involved, so a value is
//! never split or interpreted.

use serde_json::{Map, Value};

use crate::error::{Issue, Issues, pointer_token};
use crate::schema_profile::SchemaDoc;

const OPEN: &str = "{input.";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Input(String),
}

/// True when the argument is templated (contains `{input.`).
pub fn is_templated(arg: &str) -> bool {
    arg.contains(OPEN)
}

fn parse(arg: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut rest = arg;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("{{") {
            text.push('{');
            rest = r;
        } else if let Some(r) = rest.strip_prefix("}}") {
            text.push('}');
            rest = r;
        } else if let Some(r) = rest.strip_prefix(OPEN) {
            let end = r
                .find('}')
                .ok_or("`{input.` has no closing `}`; write `{{` for a literal brace")?;
            let name = &r[..end];
            if name.is_empty() {
                return Err("`{input.}` needs an input name".into());
            }
            if name.contains('{') {
                return Err(format!("`{{input.{name}}}` is not a valid placeholder"));
            }
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut text)));
            }
            pieces.push(Piece::Input(name.to_owned()));
            rest = &r[end + 1..];
        } else {
            text.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

/// Checks every templated argument: the syntax, and that each NAME is a top-level
/// property of the input schema.
pub fn check_argv(argv: &[String], schema: &SchemaDoc, pointer: &str, issues: &mut Issues) {
    for (i, arg) in argv.iter().enumerate() {
        if !is_templated(arg) {
            continue;
        }
        let p = format!("{pointer}/{i}");
        match parse(arg) {
            Err(m) => issues.push(Issue::schema(p, m)),
            Ok(pieces) => {
                for piece in pieces {
                    if let Piece::Input(name) = piece
                        && !schema.has_property(&name)
                    {
                        issues.push(Issue::schema(
                            p.clone(),
                            format!(
                                "`{{input.{name}}}` names no top-level property of input_schema"
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// Replaces placeholders with the effective input. Missing, null, object, and array values
/// are SCHEMA_INVALID issues located at `/input/NAME`.
pub fn render_argv(argv: &[String], input: &Map<String, Value>) -> Result<Vec<String>, Issues> {
    let mut issues = Issues::default();
    let mut out = Vec::with_capacity(argv.len());
    for (i, arg) in argv.iter().enumerate() {
        if !is_templated(arg) {
            out.push(arg.clone());
            continue;
        }
        let pieces = match parse(arg) {
            Ok(p) => p,
            Err(m) => {
                issues.push(Issue::schema(format!("/argv/{i}"), m));
                continue;
            }
        };
        let mut s = String::new();
        for piece in pieces {
            match piece {
                Piece::Text(t) => s.push_str(&t),
                Piece::Input(name) => {
                    let at = format!("/input/{}", pointer_token(&name));
                    let why = match input.get(&name) {
                        Some(Value::String(v)) => {
                            s.push_str(v);
                            continue;
                        }
                        Some(v @ (Value::Number(_) | Value::Bool(_))) => {
                            s.push_str(&v.to_string());
                            continue;
                        }
                        None => "has no value and no default",
                        Some(Value::Null) => "is null",
                        Some(Value::Object(_)) => "is an object",
                        Some(Value::Array(_)) => "is an array",
                    };
                    issues.push(Issue::schema(
                        at,
                        format!(
                            "`{{input.{name}}}` in argv needs a string, number, or boolean; the input {why}"
                        ),
                    ));
                }
            }
        }
        out.push(s);
    }
    issues.into_result(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(v: Value) -> SchemaDoc {
        SchemaDoc::check(v.as_object().unwrap(), "", true).unwrap()
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn input(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn renders_whole_and_partial_arguments() {
        let got = render_argv(
            &argv(&[
                "serve",
                "{input.port}",
                "--port={input.port}",
                "--name",
                "{input.name}",
            ]),
            &input(json!({"port": 8080, "name": "a b; rm -rf /"})),
        )
        .unwrap();
        assert_eq!(
            got,
            argv(&["serve", "8080", "--port=8080", "--name", "a b; rm -rf /"])
        );
    }

    #[test]
    fn renders_numbers_and_booleans_as_json_text() {
        let got = render_argv(
            &argv(&["{input.n}", "{input.f}", "{input.b}", "{input.s}"]),
            &input(json!({"n": -3, "f": 1.5, "b": true, "s": "1.50"})),
        )
        .unwrap();
        assert_eq!(got, argv(&["-3", "1.5", "true", "1.50"]));
    }

    #[test]
    fn doubled_braces_are_literal_in_templated_arguments() {
        let got = render_argv(
            &argv(&["{{input.x}}={input.x}", "{{}}"]),
            &input(json!({"x": "v"})),
        )
        .unwrap();
        assert_eq!(got, argv(&["{input.x}=v", "{{}}"]));
    }

    #[test]
    fn arguments_without_placeholders_pass_unchanged() {
        let a = argv(&["docker", "ps", "--format", "{{.Names}}", "{", "}", "{input"]);
        assert_eq!(render_argv(&a, &Map::new()).unwrap(), a);
    }

    #[test]
    fn rejects_objects_arrays_null_and_missing_values() {
        let err = render_argv(
            &argv(&["{input.o}", "{input.a}", "{input.z}", "{input.m}"]),
            &input(json!({"o": {"k": 1}, "a": [1], "z": null})),
        )
        .unwrap_err();
        let at: Vec<&str> = err.0.iter().map(|i| i.pointer.as_str()).collect();
        assert_eq!(at, ["/input/o", "/input/a", "/input/z", "/input/m"]);
        assert!(err.0.iter().all(|i| i.code.as_str() == "SCHEMA_INVALID"));
        assert!(err.0[0].message.contains("is an object"));
    }

    #[test]
    fn validation_requires_top_level_properties() {
        let s = schema(json!({
            "type": "object",
            "properties": {"port": {"type": "integer"}, "deep": {"type": "object", "properties": {"x": {}}}}
        }));
        let mut issues = Issues::default();
        check_argv(
            &argv(&[
                "run",
                "{input.port}",
                "{input.x}",
                "{input.deep.x}",
                "plain {x}",
            ]),
            &s,
            "/actions/0/run/argv",
            &mut issues,
        );
        let at: Vec<&str> = issues.0.iter().map(|i| i.pointer.as_str()).collect();
        assert_eq!(at, ["/actions/0/run/argv/2", "/actions/0/run/argv/3"]);
    }

    #[test]
    fn validation_rejects_malformed_placeholders() {
        let s = schema(json!({"type": "object", "properties": {"a": {}}}));
        for bad in ["{input.a", "{input.}", "x{input.{a}"] {
            let mut issues = Issues::default();
            check_argv(&argv(&[bad]), &s, "/argv", &mut issues);
            assert_eq!(issues.0.len(), 1, "{bad}");
        }
    }
}

//! Parser acceptance for #107.
//!
//! Decodes the same vendored cel-spec corpus as the conformance oracle (#90) and
//! asserts that EVERY `test.expr` in the `parse` and `plumbing` feature files
//! parses without error. This is the achievable acceptance for the parser layer:
//! the corpus's *evaluated* values cannot be checked until the evaluator (#108)
//! lands, but every expression in these files must lex + parse today.
//!
//! Any expression genuinely outside CEL *expression* scope is recorded in the
//! explicit `SKIP` list with a reason — nothing is silently passed.

#[allow(clippy::all, clippy::pedantic, clippy::nursery, dead_code, unused)]
mod proto {
    // Same machine-generated decode types the oracle uses; pedantic lints scoped
    // out here rather than suppressed in engine logic.
    include!(concat!(env!("OUT_DIR"), "/_includes.rs"));
}

use prost::Message;
use proto::cel::expr::conformance::test::SimpleTestFile;

use schema_forge_cel::parse;

/// Feature files whose every `test.expr` must parse.
const PARSE_FEATURES: &[&str] = &["parse", "plumbing"];

/// Expressions out of CEL *expression* scope, classified explicitly. Each entry
/// is `(expr, reason)`. Expected empty: proto-message struct construction is
/// syntactically in scope and parses (the evaluator, not the parser, would need
/// proto types). If the parser cannot handle a construct, it is recorded here.
const SKIP: &[(&str, &str)] = &[];

#[test]
fn every_parse_corpus_expr_parses() {
    let dir = env!("CEL_CONFORMANCE_BINPB");

    let mut total = 0usize;
    let mut parsed = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();

    for feature in PARSE_FEATURES {
        let path = std::path::Path::new(dir).join(format!("{feature}.binpb"));
        let bytes =
            std::fs::read(&path).unwrap_or_else(|e| panic!("read {} ({e})", path.display()));
        let file = SimpleTestFile::decode(&bytes[..])
            .unwrap_or_else(|e| panic!("decode {} ({e})", path.display()));

        for section in &file.section {
            for test in &section.test {
                let expr = test.expr.as_str();
                total += 1;

                if let Some((_, reason)) = SKIP.iter().find(|(e, _)| *e == expr) {
                    skipped += 1;
                    eprintln!("SKIP [{feature}/{}]: {reason}", test.name);
                    continue;
                }

                match parse(expr) {
                    Ok(_) => parsed += 1,
                    Err(e) => failures.push((format!("{feature}/{}", test.name), format!("{e}"))),
                }
            }
        }
    }

    eprintln!(
        "\n=== parse smoke === total {total}, parsed {parsed}, skipped {skipped}, failed {}",
        failures.len()
    );
    for (name, err) in &failures {
        eprintln!("  FAIL {name}: {err}");
    }

    assert!(total > 0, "decoded zero parse-corpus expressions");
    assert!(
        failures.is_empty(),
        "{} parse-corpus expression(s) failed to parse",
        failures.len()
    );
}

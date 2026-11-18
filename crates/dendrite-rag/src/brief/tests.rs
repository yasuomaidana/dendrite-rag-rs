use std::collections::HashSet;

use super::*;
use crate::scripted::ScriptedGateway;
use loom_rag::engine::Config;
use loom_rag::testutil::project;
use rstest::{fixture, rstest};

#[fixture]
fn engine() -> (tempfile::TempDir, Engine) {
    let dir = project();
    let mut e = Engine::open_with_embedder(dir.path(), Config::default(), None).unwrap();
    e.index(false).unwrap();
    (dir, e)
}

const BRIEF: &str = "Files to read:\n- pkg/sidecar/sidecar.go:1-4 - defines LocateBinary\nKey symbols:\n- LocateBinary (pkg/sidecar/sidecar.go:4) - finds the binary\nGaps:\n- no callers shown";

fn request() -> BriefRequest {
    BriefRequest {
        question: "where is LocateBinary defined".into(),
        local_model: Some("local".into()),
        fallback_model: Some("fallback".into()),
        // These tests script the gateway per call; the cache has its own test.
        fresh: true,
        ..BriefRequest::default()
    }
}

fn run(e: &Engine, req: &BriefRequest, g: &ScriptedGateway) -> BriefAnswer {
    brief(e, &AskSettings::default(), &BriefSettings::default(), req, g).unwrap()
}

#[rstest]
fn the_local_model_writes_the_brief_from_the_map_and_the_matches(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok(BRIEF))]);
    let a = run(&e, &request(), &g);
    assert_eq!(
        (a.source_tier, a.model.as_str(), a.brief.as_str()),
        (BriefTier::Local, "local", BRIEF)
    );
    assert_eq!(a.patterns, ["LocateBinary"]);
    assert!(
        a.matches
            .iter()
            .any(|m| m.path == "pkg/sidecar/sidecar.go" && m.line == 4)
    );
    assert!(a.hits.iter().any(|h| h.path == "pkg/sidecar/sidecar.go"));
    assert!(a.local_error.is_none() && a.warnings.is_empty() && !a.cached);
    let (_, system, user) = g.calls.borrow()[0].clone();
    let cap = g.caps.borrow()[0];
    assert_eq!(
        (system.as_str(), cap),
        (prompt::SYSTEM_PROMPT, DEFAULT_MAX_TOKENS)
    );
    assert!(
        user.contains("[1] pkg/")
            && user.contains("Grep matches:\npkg/sidecar/sidecar.go:3: // LocateBinary")
            && user.contains("pkg/sidecar/sidecar.go:4: func LocateBinary")
            && user.ends_with("Brief:"),
        "{user}"
    );
}

#[rstest]
fn a_failed_local_model_hands_over_to_the_fallback(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Err("down")), ("fallback", Ok(BRIEF))]);
    let a = run(&e, &request(), &g);
    assert_eq!(
        (a.source_tier, a.model.as_str()),
        (BriefTier::Fallback, "fallback")
    );
    assert_eq!(a.local_error.as_deref(), Some("down"));
    assert_eq!(g.models(), ["local", "fallback"]);
}

#[rstest]
fn when_both_models_fail_the_map_and_matches_come_back_as_they_are(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Err("down")), ("fallback", Err("quota"))]);
    let a = run(&e, &request(), &g);
    assert_eq!((a.source_tier, a.model.as_str()), (BriefTier::Raw, ""));
    assert!(
        a.brief.starts_with("Files (ranked):") && a.brief.contains("Matches:\n- pkg/sidecar/sidecar.go:3:"),
        "{}",
        a.brief
    );
    let why = a.local_error.unwrap();
    assert!(
        why.contains("down") && why.contains("fallback fallback failed: quota"),
        "{why}"
    );
}

#[rstest]
#[case::fallback_not_allowed(BriefRequest { allow_fallback: false, ..request() })]
#[case::no_fallback_model(BriefRequest { fallback_model: None, ..request() })]
fn no_second_model_is_asked_unless_allowed_and_named(
    engine: (tempfile::TempDir, Engine),
    #[case] req: BriefRequest,
) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Err("down")), ("fallback", Ok(BRIEF))]);
    let a = run(&e, &req, &g);
    assert_eq!(a.source_tier, BriefTier::Raw);
    assert_eq!(g.models(), ["local"], "nothing was paid for");
}

#[rstest]
fn lines_citing_a_path_that_was_not_given_are_dropped(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let invented = "Files to read:\n- pkg/sidecar/sidecar.go:1-4 - real\n- pkg/invented/ghost.go:1-9 - made up\nGaps:\n- none";
    let g = ScriptedGateway::new(&[("local", Ok(invented))]);
    let a = run(&e, &request(), &g);
    assert_eq!(a.source_tier, BriefTier::Local);
    assert!(
        a.brief.contains("sidecar.go") && !a.brief.contains("ghost"),
        "{}",
        a.brief
    );
}

#[rstest]
fn a_brief_that_cites_only_invented_paths_becomes_the_raw_one(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok("- pkg/invented/ghost.go:1-9 - made up"))]);
    let a = run(&e, &request(), &g);
    assert_eq!((a.source_tier, a.model.as_str()), (BriefTier::Raw, ""));
    assert!(
        a.warnings.iter().any(|w| w.contains("local cited only paths")),
        "{:?}",
        a.warnings
    );
    assert!(a.brief.starts_with("Files (ranked):"));
}

#[rstest]
fn an_invalid_pattern_is_a_warning_not_a_failure(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok(BRIEF))]);
    let a = run(
        &e,
        &BriefRequest {
            patterns: vec!["(".into()],
            ..request()
        },
        &g,
    );
    assert_eq!(a.source_tier, BriefTier::Local);
    assert!(
        a.matches.is_empty() && a.warnings.iter().any(|w| w.starts_with("grep skipped")),
        "{:?}",
        a.warnings
    );
}

#[rstest]
fn the_same_brief_twice_asks_the_model_once(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("local", Ok(BRIEF))]);
    let req = BriefRequest {
        fresh: false,
        ..request()
    };
    let (first, second) = (run(&e, &req, &g), run(&e, &req, &g));
    assert!(!first.cached && second.cached);
    assert_eq!(first.brief, second.brief);
    assert_eq!(g.models().len(), 1);
    run(&e, &BriefRequest { fresh: true, ..req }, &g);
    assert_eq!(g.models().len(), 2, "fresh asks again");
}

#[rstest]
fn settings_choose_the_models_the_cap_and_the_match_limit(engine: (tempfile::TempDir, Engine)) {
    let (_d, e) = engine;
    let g = ScriptedGateway::new(&[("from-brief", Err("down")), ("cheap", Ok(BRIEF))]);
    let settings = BriefSettings {
        local_model: Some("from-brief".into()),
        fallback_model: Some("cheap".into()),
        max_tokens: Some(123),
        max_matches: Some(1),
    };
    let req = BriefRequest {
        local_model: None,
        fallback_model: None,
        patterns: vec!["sidecar|Binary".into()],
        ..request()
    };
    let a = brief(&e, &AskSettings::default(), &settings, &req, &g).unwrap();
    assert_eq!((a.source_tier, a.model.as_str()), (BriefTier::Fallback, "cheap"));
    assert!(!g.caps.borrow().is_empty() && g.caps.borrow().iter().all(|c| *c == 123));
    assert_eq!(a.matches.len(), 1, "[brief] max_matches");
}

fn some(s: &str) -> Option<String> {
    Some(s.to_string())
}

#[rstest]
#[case::request_wins(
    BriefRequest { local_model: some("r"), fallback_model: some("rf"), ..BriefRequest::default() },
    some("a"), some("b"), some("bf"), ("r", Some("rf"))
)]
#[case::brief_then_ask(BriefRequest::default(), some("a"), some("b"), some("bf"), ("b", Some("bf")))]
#[case::ask_then_default(BriefRequest::default(), some("a"), None, None, ("a", None))]
#[case::the_default_local_model(BriefRequest::default(), None, None, None, (DEFAULT_LOCAL_MODEL, None))]
#[case::no_fallback_when_not_allowed(
    BriefRequest { allow_fallback: false, ..BriefRequest::default() },
    None, None, some("bf"), (DEFAULT_LOCAL_MODEL, None)
)]
fn models_come_from_the_request_then_brief_then_ask(
    #[case] req: BriefRequest,
    #[case] ask_local: Option<String>,
    #[case] brief_local: Option<String>,
    #[case] brief_fallback: Option<String>,
    #[case] want: (&str, Option<&str>),
) {
    let ask = AskSettings {
        local_model: ask_local,
        ..AskSettings::default()
    };
    let settings = BriefSettings {
        local_model: brief_local,
        fallback_model: brief_fallback,
        ..BriefSettings::default()
    };
    let (local, fallback) = models(&req, &ask, &settings);
    assert_eq!((local.as_str(), fallback.as_deref()), want);
}

#[rstest]
#[case::camel("where is LocateBinary defined", &["LocateBinary"])]
#[case::snake("what does fold_signatures keep", &["fold_signatures"])]
#[case::dotted("how is tokens.count computed?", &["tokens\\.count"])]
#[case::path_of_names("see std::mem::take here", &["std::mem::take"])]
#[case::lower_camel("the parseJson helper", &["parseJson"])]
#[case::ticket("status of DEN-903.", &["DEN\\-903"])]
#[case::quoted("find \"no such file\" in `src/main.rs`", &["no such file", "src/main\\.rs"])]
#[case::plain_words_only("how does the retrieval work", &[])]
#[case::capitalised_word_is_prose("Dendrite is a tool", &[])]
#[case::abbreviation_is_prose("use e.g. a list", &[])]
#[case::duplicates_once("LocateBinary and LocateBinary", &["LocateBinary"])]
fn patterns_are_the_identifiers_and_quoted_strings(#[case] question: &str, #[case] want: &[&str]) {
    assert_eq!(derive::patterns(question), want);
}

#[test]
fn at_most_six_patterns_are_derived() {
    let q = "aaa_bbb ccc_ddd eee_fff ggg_hhh iii_jjj kkk_lll mmm_nnn";
    assert_eq!(derive::patterns(q).len(), 6);
}

#[rstest]
#[case::given_path("- pkg/sidecar/sidecar.go:1-4 - ok", true)]
#[case::suffix_of_a_known_path("see sidecar/sidecar.go", true)]
#[case::bare_filename_is_prose("uses serde.json and v1.2", true)]
#[case::invented("- pkg/invented/ghost.go:1-9", false)]
#[case::one_of_two_invented("pkg/sidecar/sidecar.go and pkg/x/y.rs", false)]
#[case::no_path("Gaps: nothing", true)]
#[case::url_is_not_a_path("see https://example.com/page", true)]
#[case::copied_placeholder("- fold (<path>:<line>) - what it is", false)]
fn a_line_may_only_cite_given_paths(#[case] line: &str, #[case] kept: bool) {
    let known: HashSet<&str> = ["pkg/sidecar/sidecar.go"].into_iter().collect();
    assert_eq!(prompt::keep_cited(line, &known) == line, kept, "{line}");
}

fn m(path: &str, line: usize, text: &str) -> GrepMatch {
    GrepMatch {
        path: path.into(),
        line,
        text: text.into(),
    }
}

#[test]
fn matches_are_thinned_per_file_and_in_all_and_cut_short() {
    let noisy: Vec<GrepMatch> = (1..=9).map(|i| m("a.go", i, "x")).collect();
    let all: Vec<GrepMatch> = noisy
        .into_iter()
        .chain([m("b.go", 1, &"y".repeat(500)), m("c.go", 1, "z")])
        .collect();
    let thinned = thin(all.clone(), 100);
    assert_eq!(thinned.iter().filter(|x| x.path == "a.go").count(), PER_FILE);
    let b = thinned.iter().find(|x| x.path == "b.go").unwrap();
    assert_eq!(b.text.chars().count(), MATCH_CHARS + 1);
    assert!(b.text.ends_with('…'));
    assert_eq!(thin(all, 3).len(), 3);
}

#[test]
fn the_raw_brief_says_so_when_nothing_matched() {
    assert_eq!(
        prompt::raw_brief(&[], &[]),
        "Nothing in the project matched the question."
    );
}

#[test]
fn briefs_serialize_with_lowercase_tiers() {
    assert_eq!(
        serde_json::to_string(&BriefTier::Fallback).unwrap(),
        "\"fallback\""
    );
    let req: BriefRequest = serde_json::from_str(r#"{"question":"q"}"#).unwrap();
    assert!(req.allow_fallback && req.patterns.is_empty() && req.fallback_model.is_none());
}

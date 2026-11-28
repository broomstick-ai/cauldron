//! The hostile corpus: one fixture per rule, each refused by its rule's
//! name, and the sample `reading-list@1` passing.

use super::*;
use serde_json::{json, Value};

const SAMPLE: &str = include_str!("../../../../schema/soup/examples/reading-list.grammar.json");

fn sample() -> Value {
    serde_json::from_str(SAMPLE).unwrap()
}

fn rules_of(v: &Value, pins: &[&PoolGrammar]) -> Vec<&'static str> {
    let bytes = serde_json::to_vec(v).unwrap();
    match parse_file(&bytes, None) {
        Err(ps) => ps.into_iter().map(|p| p.rule).collect(),
        Ok(u) => check_user(&u, pins).into_iter().map(|p| p.rule).collect(),
    }
}

fn soup_pins() -> Vec<PoolGrammar> {
    vec![crate::grammar_v6::workspace_v6("ws"), crate::grammar_cauldron::cauldron_v1("cauldron")]
}

fn refused_by(v: Value, rule: &str) {
    let pins = soup_pins();
    let refs: Vec<&PoolGrammar> = pins.iter().collect();
    let got = rules_of(&v, &refs);
    assert!(got.contains(&rule), "expected `{rule}`, got {got:?}");
}

#[test]
fn the_sample_passes_alone_and_against_the_soup() {
    let u = parse_file(SAMPLE.as_bytes(), None).unwrap();
    assert_eq!(u.grammar.pool.as_str(), "reading-list");
    assert!(check_user(&u, &[]).is_empty(), "{:?}", check_user(&u, &[]));
    let pins = soup_pins();
    let refs: Vec<&PoolGrammar> = pins.iter().collect();
    assert!(check_user(&u, &refs).is_empty(), "{:?}", check_user(&u, &refs));
}

#[test]
fn the_built_in_grammars_are_refused_as_reserved_kinds() {
    for dir in ["workspace-v6", "cauldron-v1"] {
        let text = std::fs::read_to_string(format!("{}/../../schema/soup/{dir}/grammar.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["format"] = json!(FORMAT);
        let got = rules_of(&v, &[]);
        assert!(got.contains(&"kind_needs_hyphen"), "{dir}: {got:?}");
    }
}

#[test]
fn size_and_depth_are_judged_before_parsing() {
    let big = vec![b' '; MAX_FILE_BYTES + 1];
    assert_eq!(parse_file(&big, None).unwrap_err()[0].rule, "file_too_large");
    let ten_mb = vec![b'['; 10 * 1024 * 1024];
    assert_eq!(parse_file(&ten_mb, None).unwrap_err()[0].rule, "file_too_large");
    let deep = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
    assert_eq!(parse_file(deep.as_bytes(), None).unwrap_err()[0].rule, "too_deep");
    // Brackets inside strings do not count.
    let mut v = sample();
    v["node_types"]["book"]["fields"]["handle"]["kind"] = json!("text");
    v["note_in_string"] = json!("[[[[[[[[[[[[[[[[[[");
    let bytes = serde_json::to_vec(&v).unwrap();
    assert_ne!(parse_file(&bytes, None).unwrap_err()[0].rule, "too_deep");
}

#[test]
fn format_and_unknown_keys_refuse_by_name() {
    let mut v = sample();
    v.as_object_mut().unwrap().remove("format");
    refused_by(v, "wrong_format");
    let mut v = sample();
    v["format"] = json!("cauldron-grammar/2");
    refused_by(v, "wrong_format");
    // A key of a later format, at any depth, is never ignored.
    let mut v = sample();
    v["node_types"]["book"]["fields"]["handle"]["pattern"] = json!("(a+)+$");
    refused_by(v, "unknown_or_malformed_key");
    let mut v = sample();
    v["ripples"] = json!({"may_write": ["book"]});
    refused_by(v, "unknown_or_malformed_key");
    refused_by(json!("not an object"), "wrong_format");
    let bytes = b"{not json";
    assert_eq!(parse_file(bytes, None).unwrap_err()[0].rule, "not_json");
}

#[test]
fn kinds_and_pools() {
    let mut v = sample();
    v["kind"] = json!("notes");
    refused_by(v, "kind_needs_hyphen");
    let mut v = sample();
    v["kind"] = json!("workspace");
    v["node_types"] = json!({"region": sample()["node_types"]["author"].clone()});
    let pins = soup_pins();
    let refs: Vec<&PoolGrammar> = pins.iter().collect();
    let got = rules_of(&v, &refs);
    assert!(got.contains(&"kind_needs_hyphen") && got.contains(&"type_name_taken"), "{got:?}");
    let mut v = sample();
    v["kind"] = json!("Reading-List");
    refused_by(v, "name_charset");
    let mut v = sample();
    v["pool"] = json!("ws");
    refused_by(v, "pool_reserved");
    let mut v = sample();
    v["pool"] = json!("../../etc");
    refused_by(v, "name_charset");
    let mut v = sample();
    v["version"] = json!(0);
    refused_by(v, "version_range");
    let err = parse_file(SAMPLE.as_bytes(), Some("other-pool"));
    assert_eq!(err.unwrap().grammar.pool.as_str(), "other-pool");
    let mut v = sample();
    v["pool"] = json!("books-a");
    let bytes = serde_json::to_vec(&v).unwrap();
    assert_eq!(parse_file(&bytes, Some("books-b")).unwrap_err()[0].rule, "pool_disagrees");
}

#[test]
fn names_that_would_splice_into_statements_or_patterns_are_refused() {
    let mut v = sample();
    let book = v["node_types"]["book"].clone();
    v["node_types"]["bo)ok\nMATCH"] = book;
    refused_by(v, "name_charset");
    let mut v = sample();
    v["node_types"]["book"]["id_prefix"] = json!("(a+)+");
    refused_by(v, "prefix_charset");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["a.b"] = json!({"kind": "text", "max_chars": 10});
    refused_by(v, "name_charset");
    let mut v = sample();
    v["node_types"]["book"]["states"] = json!(["ok", "no way"]);
    refused_by(v, "name_charset");
    let mut v = sample();
    v["edge_kinds"]["bad kind"] = v["edge_kinds"]["written_by"].clone();
    refused_by(v, "name_charset");
}

#[test]
fn prefixes_and_types_are_once_per_cauldron() {
    let mut v = sample();
    v["node_types"]["book"]["id_prefix"] = json!("R");
    refused_by(v, "prefix_taken");
    let mut v = sample();
    v["node_types"]["author"]["id_prefix"] = json!("BK");
    refused_by(v, "prefix_taken");
    let mut v = sample();
    v["node_types"]["book"].as_object_mut().unwrap().remove("id_prefix");
    refused_by(v, "prefix_missing");
    let mut v = sample();
    let a = v["node_types"]["author"].clone();
    v["node_types"]["decision"] = a;
    v["node_types"]["decision"]["id_prefix"] = json!("DX");
    refused_by(v, "type_name_taken");
    // Against another registered user grammar.
    let other = parse_file(SAMPLE.as_bytes(), None).unwrap().grammar;
    let mut v = sample();
    v["kind"] = json!("library-list");
    let refs = [&other];
    let got = rules_of(&v, &refs);
    assert!(got.contains(&"type_name_taken") && got.contains(&"prefix_taken"), "{got:?}");
    let mut v = sample();
    v["pool"] = json!("reading-two");
    let got = rules_of(&v, &refs);
    assert!(got.contains(&"kind_registered"), "{got:?}");
}

#[test]
fn the_display_contract() {
    let mut v = sample();
    v["node_types"]["book"]["fields"].as_object_mut().unwrap().remove("handle");
    refused_by(v, "handle_required");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["handle"]["required"] = json!(false);
    refused_by(v, "handle_required");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["handle"]["max_chars"] = json!(236);
    refused_by(v, "handle_required");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["handle"]["kind"] = json!("any");
    refused_by(v, "handle_required");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["body"]["max_chars"] = json!(20000);
    refused_by(v, "body_shape");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["meta"]["kind"] = json!("text");
    refused_by(v, "meta_shape");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["edges"] = json!({"kind": "any"});
    refused_by(v, "field_reserved");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["state"] = json!({"kind": "text", "max_chars": 10});
    refused_by(v, "field_reserved");
    let mut v = sample();
    v["edge_kinds"]["about"]["fields"]["to"] = json!({"kind": "text", "max_chars": 10});
    refused_by(v, "field_reserved");
    let mut v = sample();
    v["node_types"]["book"]["fields"]["year"] = json!({"kind": "text", "max_chars": 10001});
    refused_by(v, "cap_too_large");
}

#[test]
fn reaches_and_edge_names() {
    let mut v = sample();
    v["edge_kinds"]["in_region"] = json!({"from": [{"at": "local", "node_type": "book"}],
                                          "to": [{"at": "reach", "kind": "workspace", "node_type": "region"}]});
    refused_by(v, "edge_name_reserved");
    // Even local, `in_region` and `claims` are never a user edge.
    let mut v = sample();
    v["edge_kinds"]["claims"] = v["edge_kinds"]["written_by"].clone();
    refused_by(v, "edge_name_reserved");
    let mut v = sample();
    v["edge_kinds"]["depends_on"] = json!({"from": [{"at": "local", "node_type": "book"}],
                                           "to": [{"at": "reach", "kind": "workspace", "node_type": "task"}]});
    refused_by(v, "edge_name_reserved");
    let mut v = sample();
    v["edge_kinds"]["about"]["to"] = json!([{"at": "reach", "kind": "workspace", "node_type": "viewer"}]);
    refused_by(v, "reach_not_allowed");
    let mut v = sample();
    v["edge_kinds"]["about"]["to"] = json!([{"at": "reach", "kind": "cauldron", "node_type": "cluster"}]);
    refused_by(v, "reach_not_allowed");
    let mut v = sample();
    v["edge_kinds"]["about"]["to"] = json!([{"at": "reach", "kind": "dumps", "node_type": "dump"}]);
    refused_by(v, "reach_not_allowed");
    let mut v = sample();
    v["edge_kinds"]["about"]["to"] = json!([{"at": "reach", "kind": "film-list", "node_type": "film"}]);
    refused_by(v, "reach_unknown_kind");
    let mut v = sample();
    v["edge_kinds"]["about"]["to"] = json!([{"at": "reach", "kind": "reading-list", "node_type": "film"}]);
    refused_by(v, "reach_unknown_type");
    // A reach into another registered user kind is allowed.
    let mut films = sample();
    films["kind"] = json!("film-list");
    films["node_types"] = json!({"film": {"id_prefix": "FM", "fields": {"handle": {"required": true, "kind": "text", "min_chars": 1, "max_chars": 235}}}});
    films["edge_kinds"] = json!({});
    let films = parse_file(&serde_json::to_vec(&films).unwrap(), None).unwrap().grammar;
    let mut v = sample();
    v["edge_kinds"]["about"]["to"] = json!([{"at": "reach", "kind": "film-list", "node_type": "film"}]);
    let ws = crate::grammar_v6::workspace_v6("ws");
    assert!(rules_of(&v, &[&ws, &films]).is_empty(), "{:?}", rules_of(&v, &[&ws, &films]));
}

#[test]
fn counts_are_bounded() {
    let mut v = sample();
    let a = v["node_types"]["author"].clone();
    for i in 0..65 {
        let mut t = a.clone();
        t["id_prefix"] = json!(format!("X{i}"));
        v["node_types"][format!("t{i}")] = t;
    }
    refused_by(v, "too_many");
    let mut v = sample();
    v["node_types"]["book"]["count_cells"] = json!({"n": {"edge_kind": "written_by", "direction": "out"}});
    refused_by(v, "count_cells_unsupported");
    let mut v = sample();
    v["node_types"]["book"]["required_edges_at_mint"] = json!({"written_by": 17});
    refused_by(v, "too_many");
    let mut pins: Vec<PoolGrammar> = Vec::new();
    for i in 0..MAX_USER_POOLS {
        let mut g = parse_file(SAMPLE.as_bytes(), None).unwrap().grammar;
        g.kind = PoolKind(format!("k-{i}"));
        g.pool = PoolName(format!("p-{i}"));
        g.node_types = BTreeMap::new();
        pins.push(g);
    }
    let refs: Vec<&PoolGrammar> = pins.iter().collect();
    assert!(rules_of(&sample(), &refs).contains(&"too_many_user_pools"));
}

#[test]
fn the_spine_takes_what_the_check_passes() {
    // The mirror reads every key the pinned grammar writes: a pin turned
    // into a file reads back as the same grammar.
    let g = parse_file(SAMPLE.as_bytes(), None).unwrap().grammar;
    let again = parse_file(&serde_json::to_vec(&to_file(&g)).unwrap(), None).unwrap().grammar;
    assert_eq!(g, again);
    for b in built_ins() {
        let mut f = to_file(&b);
        f.as_object_mut().unwrap().remove("pool");
        let back = parse_file(&serde_json::to_vec(&f).unwrap(), Some(b.pool.as_str())).unwrap().grammar;
        assert_eq!(back, b, "{}", b.kind);
    }
    g.check_wellformed().unwrap();
}

#[test]
fn digest_ignores_the_pool_name_and_nothing_else() {
    let g = parse_file(SAMPLE.as_bytes(), None).unwrap().grammar;
    let mut h = g.clone();
    h.pool = PoolName("books-elsewhere".into());
    assert_eq!(digest(&g), digest(&h));
    h.version = 2;
    assert_ne!(digest(&g), digest(&h));
    assert!(digest(&g).starts_with("sha256:") && digest(&g).len() == 7 + 64);
}

/// The drift test of the published meta-schema.
#[test]
fn published_meta_schema_matches_the_generator() {
    let path = format!("{}/../../schema/soup/grammar.schema.json", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("SPINE_SCHEMA_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(&path, meta_schema_text()).unwrap();
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        on_disk == meta_schema_text(),
        "schema/soup/grammar.schema.json drifted; regenerate: SPINE_SCHEMA_BLESS=1 cargo test -p spine --lib user_grammar"
    );
}

/// A byte order mark is skipped; a key named twice in one object is
/// refused (a JSON reader would keep one silently: `"kind": "workspace",
/// "kind": "zz-test"` must not pass as `zz-test`). (Inspection F9.)
#[test]
fn a_byte_order_mark_is_skipped_and_a_key_named_twice_is_refused() {
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(SAMPLE.as_bytes());
    assert_eq!(parse_file(&bom, None).expect("a BOM is not content").grammar.kind.as_str(), "reading-list");
    let twice = SAMPLE.replacen("\"kind\": \"reading-list\",", "\"kind\": \"workspace\", \"kind\": \"reading-list\",", 1);
    assert_ne!(twice, SAMPLE);
    let ps = parse_file(twice.as_bytes(), None).expect_err("a duplicate key");
    assert_eq!(ps[0].rule, "duplicate_key", "{ps:?}");
    let nested = SAMPLE.replacen("\"id_prefix\": \"BK\",", "\"id_prefix\": \"BK\", \"id_prefix\": \"DEC\",", 1);
    assert_eq!(parse_file(nested.as_bytes(), None).expect_err("nested too")[0].rule, "duplicate_key");
}

/// The pins as read from a soup's files are judged as `add` judged them:
/// the sample beside the built-ins passes; a pin declaring a built-in type,
/// one taking a built-in prefix, and a built-in kind under a user pool's
/// name are each refused by name. (Inspection F4.)
#[test]
fn the_pins_a_soup_holds_are_judged_as_add_judged_them() {
    let mut pins = soup_pins();
    let u = parse_file(SAMPLE.as_bytes(), None).unwrap().grammar;
    pins.push(u.clone());
    let refs: Vec<&PoolGrammar> = pins.iter().collect();
    assert!(judge_pins(&refs).is_empty(), "{:?}", judge_pins(&refs));
    let rules = |g: PoolGrammar| {
        let mut pins = soup_pins();
        pins.push(g);
        let refs: Vec<&PoolGrammar> = pins.iter().collect();
        judge_pins(&refs).into_iter().map(|(p, x)| (p.as_str().to_string(), x.rule)).collect::<Vec<_>>()
    };
    let mut hijack = u.clone();
    hijack.node_types.insert("decision".into(), hijack.node_types["author"].clone());
    hijack.node_types.get_mut("decision").unwrap().id_prefix = Some("DEC".into());
    let got = rules(hijack);
    assert!(got.contains(&("reading-list".into(), "type_name_taken")) && got.contains(&("reading-list".into(), "prefix_taken")), "{got:?}");
    let mut prefix = u.clone();
    prefix.node_types.get_mut("author").unwrap().id_prefix = Some("R".into());
    assert!(rules(prefix).contains(&("reading-list".into(), "prefix_taken")));
    let mut builtin = crate::grammar_v6::workspace_v6("reading-list");
    builtin.pool = PoolName::new("reading-list");
    assert_eq!(rules(builtin), [("reading-list".to_string(), "builtin_kind_in_user_pool")]);
    // Alone (no other workspace pool), a workspace pin under any name is the
    // spine's business, not a user grammar's.
    let lone = crate::grammar_v6::workspace_v6("ws-v6");
    assert!(judge_pins(&[&lone]).is_empty());
}

/// **The registry digest is pinned** (U3i D2): the spine keeps every user
/// pool's digest and compares it at each open, so it must not move when a
/// later release adds a field to the grammar structs. Its input is written
/// field by field (`digest_input`, version 1); this pins the example
/// grammar's digest and that input's shape. If this fails, a soup made by
/// an earlier release would refuse every user pool: move
/// `DIGEST_INPUT_VERSION` and migrate the registered digests instead.
#[test]
fn the_registry_digest_of_the_example_grammar_is_pinned() {
    let g = parse_file(SAMPLE.as_bytes(), None).unwrap().grammar;
    assert_eq!(digest(&g), "sha256:6ce1a429f83735b7600203aa10338401a90385eeec5a1a9eaa680871c22f20b5");
    let input = digest_input(&g);
    let keys: Vec<&String> = input.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["digest_input", "edge_kinds", "kind", "node_types", "version"]);
    assert_eq!(input["digest_input"], 1);
    let book: Vec<&String> = input["node_types"]["book"].as_object().unwrap().keys().collect();
    assert_eq!(book, ["count_cells", "fields", "id_prefix", "required_at_mint_any_of", "required_edges_at_mint", "states"]);
    let mut renamed = g.clone();
    renamed.pool = PoolName::new("other-name");
    assert_eq!(digest(&renamed), digest(&g), "the pool's name is not in the digest");
    // Form 0, what development builds before form 1 recorded (their digest
    // of the example): known, so such a row migrates.
    assert_eq!(digest_v0(&g), "sha256:42ca8afdc414112730bff88ea345562659d7c84c17fd2f1f2ee23c2539147809");
    assert_eq!(digest_forms().iter().map(|(v, _)| *v).collect::<Vec<_>>(), [0, DIGEST_INPUT_VERSION]);
}

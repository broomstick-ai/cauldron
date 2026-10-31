//! **Seeding a new cluster** — his msg 112: making a cluster (or a sub-cluster)
//! asks him for its BOUNDARIES in his own words, then ONE type-one ripple call
//! finds what in the soup falls inside them and the cluster opens populated.
//!
//! The ripple is `seed_cluster`, a definition in the policy like any other
//! (Status › Bus lists it): it wakes on `cluster_created`, walks every
//! candidate — the soup's regions, the registered repos, the open questions
//! and the tasks not done — asks ONE yes/no question of each ("does this fall
//! inside the boundaries he stated?") in one batched call, and its MAP RULES
//! turn a yes at `p ≥ min_p` into an action: `claim_region`, `claim_repo` or
//! `link_node`. The rules are the policy's; nothing in the face edits them
//! (msg 235: "there's no system level config stuff").
//!
//! **THE SEAM** is [`TypeOneCall`]: the one place the call is made. The REAL
//! call is [`type_one::TypeOneCli`] — substrate-ripple's `type-one` CLI, one
//! batched ask, Haiku (threads) or Jev (OpenRouter) as the ripple's MODEL row
//! says: its `model` if it names one, else its `trusted` side, or with none
//! [`TRUSTED`] (`primary` → the
//! `type_one.primary` role's model, `secondary` → `type_one.secondary`'s), all
//! read from the LIVE settings ([`Live`]). The seed policy's row is `trusted =
//! secondary` (jev): Jev answers in about 0.4 s, Haiku through threads in
//! about 19 s. Switching the seeding to Haiku is that one row.
//!
//! The face runs [`seed`] OFF its UI loop (`App::create_cluster` spawns it;
//! `App::poll_seeding` lands it on a quiet tick): the cluster opens at once,
//! marked as seeding, and fills in when the answer lands.
//!
//! [`LocalMatcher`] is the EXPLICIT FALLBACK — a deterministic word-overlap
//! matcher over the soup, a deferred-richness stand-in (core:stub-doctrine). It
//! runs only when the real call cannot (no binary, `CAULDRON_TYPE_ONE=off`, the
//! adapter failed), and it never passes for the model: its adapter name is
//! `local matcher`, every probability is recorded under that name, the run's
//! line opens `stand-in:` and ENDS with why the real call did not answer.

use std::collections::{BTreeMap, BTreeSet};

use crate::cauldron::ripples::{IterRec, Kind, MapRule, Policy, RippleDef, Run, StepRec};

#[path = "type_one.rs"]
pub mod type_one;
use type_one::{Adapter, TypeOneCli};

/// `off` forces the stand-in (said on the run); anything else leaves the
/// choice to the ripple's MODEL row.
pub const SWITCH_ENV: &str = "CAULDRON_TYPE_ONE";

/// The ripple that seeds a new cluster.
pub const SEED_RIPPLE: &str = "seed_cluster";

/// What a candidate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandKind {
    Region,
    Repo,
    /// An open question or a task not done, of some cluster.
    Node,
}

impl CandKind {
    /// The `item_kind` a map rule names it by.
    pub fn word(self) -> &'static str {
        match self {
            CandKind::Region => "region",
            CandKind::Repo => "repo",
            CandKind::Node => "node",
        }
    }
}

/// One thing in the soup the seeding may claim or link.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub kind: CandKind,
    /// `R-3`, a repo's name, or `<cluster>/Q-2` · `<cluster>/T-4`.
    pub id: String,
    /// What it is about: the text the question is asked over.
    pub text: String,
}

/// What one batched call answered.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answer {
    /// Per item, in order: the probability of `true`.
    pub p: Vec<f64>,
    pub cost_usd: Option<f64>,
    /// The adapter's own latency, when it reports one.
    pub latency_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    /// The adapter's ask id (its catalogue key), when it has one.
    pub ask_id: Option<String>,
}

/// **THE SEAM**: one batched type-one call. `question` is the ripple's
/// question fragment (slots unfilled); `boundaries` is his text. The answer
/// is, per item, the probability of `true` for the yes/no question.
pub trait TypeOneCall {
    /// The adapter's name, as every step record carries it.
    fn adapter(&self) -> String;
    /// Whether this is a stand-in for the model (said on the run).
    fn stand_in(&self) -> bool;
    fn ask(&self, ripple: &RippleDef, question: &str, boundaries: &str, items: &[Candidate], run_id: &str, policy: &str) -> Result<Answer, String>;
}

/// The side a type-one ripple trusts when its MODEL row names none. It is
/// not a setting: nothing about the ripples is configured (8e4fca10 msg
/// 235: "there's no system level config stuff. That's my call").
pub const TRUSTED: &str = "primary";

/// The LIVE settings the seeding reads, resolved by the face from the
/// cauldron's `settings.toml` at the moment the cluster is made: the
/// type-one roles' models, and [`TRUSTED`], which a ripple whose MODEL row
/// names no side takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    /// The `type_one.primary` role's model.
    pub primary: String,
    /// The `type_one.secondary` role's model.
    pub secondary: String,
    /// The cauldron's trusted side.
    pub trusted: String,
}

impl Live {
    /// The built-in defaults (no settings file): what a face with no
    /// override would resolve.
    pub fn defaults() -> Live {
        let model = |id: &str| crate::cauldron::settings::role_of(id).map_or("—", |r| r.model).to_string();
        Live { primary: model("type_one.primary"), secondary: model("type_one.secondary"), trusted: TRUSTED.to_string() }
    }
}

/// The adapter `ripple`'s MODEL row names: its `model` if that is an
/// adapter, else its `trusted` side (or, with none, the cauldron's) resolved
/// through the type-one roles' LIVE models. `blend` is refused (it needs both
/// adapters to answer, and the CLI's blend weight is not this row's to set).
pub fn wanted_adapter(ripple: &RippleDef, live: &Live) -> Result<Adapter, String> {
    if let Some(a) = ripple.model.model.as_deref().and_then(Adapter::from_model) {
        return Ok(a);
    }
    let side = ripple.model.trusted.as_deref().unwrap_or(&live.trusted);
    let (role, model) = match side {
        "primary" => ("type_one.primary", &live.primary),
        "secondary" => ("type_one.secondary", &live.secondary),
        other => return Err(format!("trusted `{other}` is not built for the seeding; pick primary or secondary")),
    };
    Adapter::from_model(model).ok_or_else(|| format!("role {role} runs `{model}`, which is not a type-one adapter"))
}

/// The real call for `ripple`, or why there is none.
pub fn real_call(ripple: &RippleDef, live: &Live) -> Result<TypeOneCli, String> {
    if std::env::var(SWITCH_ENV).is_ok_and(|v| v.trim() == "off") {
        return Err(format!("the real call is off ({SWITCH_ENV}=off)"));
    }
    let adapter = wanted_adapter(ripple, live)?;
    let bin = type_one::find_bin()?;
    Ok(TypeOneCli::new(bin, adapter))
}

/// The adapter a seeding of `policy` will try first, as a word for the
/// "seeding" line: the real adapter's name, or `local matcher` when the
/// real call cannot be made (the run will say why).
pub fn will_ask(policy: &Policy, live: &Live) -> String {
    match policy.get(SEED_RIPPLE).map(|r| real_call(r, live)) {
        Some(Ok(c)) => c.adapter(),
        _ => LocalMatcher.adapter(),
    }
}

/// The stand-in: word overlap between his boundaries and each candidate,
/// with words that half the candidates share ignored. Deterministic.
pub struct LocalMatcher;

const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "what", "how", "its", "his", "her", "are", "was", "not", "but", "all", "any", "can",
    "into", "from", "out", "about", "inside", "outside", "here", "there", "they", "them", "who", "one", "each", "every", "has", "have",
    "will", "should", "would", "where", "which", "when", "than", "then", "also", "only", "own", "our", "your", "you", "everything",
    "anything", "thing", "things", "belongs", "belong", "goes", "cluster", "clusters",
];

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .map(|w| if w.len() > 4 && w.ends_with('s') && !w.ends_with("ss") { w[..w.len() - 1].to_string() } else { w })
        .collect()
}

impl LocalMatcher {
    /// `p(true)` per item.
    pub fn score(&self, boundaries: &str, items: &[Candidate]) -> Vec<f64> {
        let b = words(boundaries);
        let sets: Vec<BTreeSet<String>> = items.iter().map(|c| words(&format!("{} {}", c.id, c.text))).collect();
        // A word half the candidates share says nothing about which one.
        let mut df: BTreeMap<&String, usize> = BTreeMap::new();
        for s in &sets {
            for w in s {
                *df.entry(w).or_default() += 1;
            }
        }
        let common = |w: &String| df.get(w).copied().unwrap_or(0) * 2 > items.len().max(2);
        sets.iter()
            .map(|s| {
                let hits = b.iter().filter(|w| s.contains(*w) && !common(w)).count() as i32;
                if hits == 0 {
                    0.05
                } else {
                    ((1.0 - 0.45f64.powi(hits)) * 100.0).round() / 100.0
                }
            })
            .collect()
    }
}

impl TypeOneCall for LocalMatcher {
    fn adapter(&self) -> String {
        "local matcher".into()
    }
    fn stand_in(&self) -> bool {
        true
    }
    fn ask(&self, _ripple: &RippleDef, _question: &str, boundaries: &str, items: &[Candidate], _run_id: &str, _policy: &str) -> Result<Answer, String> {
        Ok(Answer { p: self.score(boundaries, items), ..Answer::default() })
    }
}

/// What a seeding decided.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seeded {
    pub regions: Vec<String>,
    pub repos: Vec<String>,
    /// The soup ids of the questions and tasks it links (`Q-2`): an id is
    /// the soup's, unique across the cauldron (msg 235: "There's only one
    /// soup"), so it names no cluster.
    pub links: Vec<String>,
    pub run: Option<Run>,
    /// Why nothing was seeded, when nothing was.
    pub skipped: Option<String>,
}

/// The rule that fires for `item` at probability `p` of `true`, if any.
fn fires<'a>(rules: &'a [MapRule], key: &str, item: &Candidate, p: f64) -> Option<&'a MapRule> {
    rules.iter().find(|m| {
        let kind_ok = m.item_kind.is_empty() || m.item_kind == item.kind.word();
        let pa = if m.answer == "false" { 1.0 - p } else { p };
        m.key == key && kind_ok && pa >= m.min_p
    })
}

/// The real call for a ripple, or why there is none.
pub type Pick<'a> = &'a dyn Fn(&RippleDef) -> Result<Box<dyn TypeOneCall>, String>;

/// Run the seeding ripple of `policy` over `items` for his `boundaries`
/// through the REAL type-one call its MODEL row selects under the `live`
/// settings ([`real_call`]),
/// and through `fallback` — said so on the run — only when that cannot
/// answer. `run_id` and `started` name the run it records; `cluster` is the
/// new cluster's KEY (its id), which the run records so a rename never loses it.
#[allow(clippy::too_many_arguments)]
pub fn seed(policy: &Policy, live: &Live, fallback: &dyn TypeOneCall, boundaries: &str, items: &[Candidate], cluster: &str, run_id: &str, started: chrono::DateTime<chrono::Local>) -> Seeded {
    let pick = |r: &RippleDef| real_call(r, live).map(|c| Box::new(c) as Box<dyn TypeOneCall>);
    seed_through(policy, &pick, fallback, boundaries, items, cluster, run_id, started)
}

/// [`seed`] with the real call chosen by `pick` (tests hand in their own).
#[allow(clippy::too_many_arguments)]
pub fn seed_through(policy: &Policy, pick: Pick<'_>, fallback: &dyn TypeOneCall, boundaries: &str, items: &[Candidate], cluster: &str, run_id: &str, started: chrono::DateTime<chrono::Local>) -> Seeded {
    let mut out = Seeded::default();
    let Some(r) = policy.get(SEED_RIPPLE) else {
        out.skipped = Some(format!("the policy has no `{SEED_RIPPLE}` ripple"));
        return out;
    };
    if !r.enabled || !r.triggers.iter().any(|t| t.enabled && t.on == "cluster_created") {
        out.skipped = Some(format!("`{SEED_RIPPLE}` is off"));
        return out;
    }
    if r.kind != Kind::TypeOne || r.questions.is_empty() {
        out.skipped = Some(format!("`{SEED_RIPPLE}` is not a type one with a question"));
        return out;
    }
    let t0 = std::time::Instant::now();
    let q = &r.questions[0];
    let policy_ref = format!("{}@{}", policy.file.policy.name, policy.hash8());
    let question = r.prompt.as_ref().and_then(|pd| policy.fragment(&pd.fragment, pd.version).map(str::to_string));
    // The real call first; the stand-in only when it cannot answer, and why.
    let real = pick(r).and_then(|c| {
        let question = question.as_deref().ok_or_else(|| {
            let pd = r.prompt.as_ref();
            format!("its question fragment {} v{} is missing", pd.map_or("—", |p| p.fragment.as_str()), pd.map_or(0, |p| p.version))
        })?;
        let a = c.ask(r, question, boundaries, items, run_id, &policy_ref).map_err(|e| format!("{} did not answer: {e}", c.adapter()))?;
        if a.p.len() != items.len() {
            return Err(format!("{} answered {} of {} items", c.adapter(), a.p.len(), items.len()));
        }
        Ok((c.adapter(), c.stand_in(), a))
    });
    let (adapter, stand_in, answer, why_not) = match real {
        Ok((name, stand, a)) => (name, stand, a, None),
        Err(why) => match fallback.ask(r, question.as_deref().unwrap_or(""), boundaries, items, run_id, &policy_ref) {
            Ok(a) if a.p.len() == items.len() => (fallback.adapter(), fallback.stand_in(), a, Some(why)),
            Ok(_) | Err(_) => {
                out.skipped = Some(format!("the type-one call did not answer ({why}) and neither did the {}", fallback.adapter()));
                return out;
            }
        },
    };
    let ps = &answer.p;
    let mut iters = Vec::new();
    for (item, &p) in items.iter().zip(ps.iter()) {
        let rule = fires(&r.map, &q.key, item, p);
        let did = match rule {
            Some(m) => {
                let ok = match (m.name.as_str(), item.kind) {
                    ("claim_region", CandKind::Region) => {
                        out.regions.push(item.id.clone());
                        true
                    }
                    ("claim_repo", CandKind::Repo) => {
                        out.repos.push(item.id.clone());
                        true
                    }
                    ("link_node", CandKind::Node) => {
                        out.links.push(item.id.clone());
                        true
                    }
                    _ => false,
                };
                if ok {
                    format!("true → {}.{}", m.set, m.name)
                } else {
                    format!("true → {}.{} does not take a {}: nothing", m.set, m.name, item.kind.word())
                }
            }
            None => "→ nothing".to_string(),
        };
        let mut pm = BTreeMap::new();
        pm.insert(adapter.clone(), p);
        iters.push(IterRec {
            text: format!("{} {}", item.id, crate::cauldron::view::crop(&item.text, 60)),
            ms: 0,
            steps: vec![StepRec {
                step: q.key.clone(),
                kind: "type_one".into(),
                model: adapter.clone(),
                line: did,
                p: pm,
                input: format!("boundaries: {boundaries}\nitem: {} — {}", item.id, item.text),
                output: format!("{{\"{}\": {{\"true\": {p:.2}, \"false\": {:.2}}}}}", q.key, 1.0 - p),
                ..Default::default()
            }],
        });
    }
    let ms = t0.elapsed().as_millis() as u64;
    let stand = if stand_in { "stand-in: " } else { "" };
    let mut line = format!(
        "{stand}{adapter} · {} region{} · {} repo{} · {} link{}",
        out.regions.len(),
        if out.regions.len() == 1 { "" } else { "s" },
        out.repos.len(),
        if out.repos.len() == 1 { "" } else { "s" },
        out.links.len(),
        if out.links.len() == 1 { "" } else { "s" },
    );
    if let Some(why) = &why_not {
        line.push_str(&format!(" · not the model: {why}"));
    }
    let effects: Vec<crate::cauldron::ripples::Effect> = out
        .regions
        .iter()
        .chain(out.repos.iter())
        .map(|x| crate::cauldron::ripples::Effect { node: x.clone(), rev_before: None, rev_after: Some(1) })
        .chain(out.links.iter().map(|l| crate::cauldron::ripples::Effect { node: l.clone(), rev_before: None, rev_after: Some(1) }))
        .collect();
    let model = if stand_in { format!("{adapter} — stand-in for the type-one call") } else { format!("{adapter} · type-one") };
    let mut said = line.clone();
    if !stand_in {
        said.push_str(&format!(
            "\nlatency {} ms · cost {} · input tokens {}{}",
            answer.latency_ms.map_or("—".into(), |l| l.to_string()),
            answer.cost_usd.map_or("—".into(), |c| format!("${c:.6}")),
            answer.input_tokens.map_or("—".into(), |t| t.to_string()),
            answer.ask_id.as_ref().map_or(String::new(), |a| format!(" · ask {a}")),
        ));
    }
    let tree = vec![StepRec {
        step: SEED_RIPPLE.into(),
        kind: "type_one".into(),
        model,
        line: format!("asked {} · one batched call", items.len()),
        ms,
        input: format!("boundaries: {boundaries}"),
        output: said,
        cost: answer.cost_usd,
        effects,
        foreach: Some(items.len() as u32),
        items: iters,
        ..Default::default()
    }];
    out.run = Some(Run {
        id: run_id.to_string(),
        ripple: SEED_RIPPLE.into(),
        cluster: cluster.to_string(),
        cite: String::new(),
        started: started.format("%H:%M:%S").to_string(),
        day: started.format("%Y-%m-%d").to_string(),
        at: Some(started.to_rfc3339()),
        elapsed: format!("{ms} ms"),
        cost: answer.cost_usd,
        state: "done".into(),
        line,
        account: "—".into(),
        policy_hash: Some(policy.hash8()),
        steps: 1 + items.len() as u32,
        tree,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(kind: CandKind, id: &str, text: &str) -> Candidate {
        Candidate { kind, id: id.into(), text: text.into() }
    }

    #[test]
    fn the_matcher_scores_overlap_and_ignores_common_words() {
        let items = vec![
            c(CandKind::Region, "R-3", "RIPPLES - scheduled background work the cauldron runs"),
            c(CandKind::Region, "R-4", "DREAMING - the overnight consolidation ripple"),
            c(CandKind::Region, "R-20", "LANDING - the public page: copy, pricing, the waitlist form"),
        ];
        let ps = LocalMatcher.score("the overnight dreaming and its consolidation", &items);
        assert!(ps[1] >= 0.8, "{ps:?}");
        assert!(ps[0] < 0.5 && ps[2] < 0.5, "{ps:?}");
    }

    fn policy() -> Policy {
        let tmp = std::env::temp_dir().join(format!("seeding-unit-{}", uuid::Uuid::new_v4().simple()));
        crate::cauldron::ripples::Store::open(&crate::cauldron::store::seed_dir(), &tmp, false).expect("the seed policy").seed_policy
    }

    fn with(p: &mut Policy, f: impl FnOnce(&mut RippleDef)) {
        let i = p.index(SEED_RIPPLE).unwrap();
        f(&mut p.file.ripple[i]);
    }

    fn items() -> Vec<Candidate> {
        vec![
            c(CandKind::Region, "R-4", "DREAMING - the overnight consolidation ripple"),
            c(CandKind::Region, "R-20", "LANDING - the public page"),
            c(CandKind::Repo, "cauldron", "cauldron ~/code/cauldron"),
            c(CandKind::Node, "Q-2", "is the dream replay nightly?"),
        ]
    }

    /// A real (non-stand-in) adapter with canned answers, to prove the seam.
    struct Canned(Vec<f64>, Option<f64>);
    impl TypeOneCall for Canned {
        fn adapter(&self) -> String {
            "jev".into()
        }
        fn stand_in(&self) -> bool {
            false
        }
        fn ask(&self, _r: &RippleDef, q: &str, _b: &str, _i: &[Candidate], _run: &str, _p: &str) -> Result<Answer, String> {
            assert!(q.contains("{{slot:item}}"), "the definition's own question reaches the call: {q}");
            Ok(Answer { p: self.0.clone(), cost_usd: self.1, latency_ms: Some(400), ..Answer::default() })
        }
    }

    #[test]
    fn the_real_call_answers_and_its_map_rules_decide() {
        let p = policy();
        let pick = |_: &RippleDef| Ok(Box::new(Canned(vec![0.9, 0.1, 0.6, 0.8], Some(0.0001))) as Box<dyn TypeOneCall>);
        let s = seed_through(&p, &pick, &LocalMatcher, "the dreaming", &items(), "Dream", "r-1", chrono::Local::now());
        assert_eq!(s.regions, vec!["R-4".to_string()]);
        assert_eq!(s.repos, vec!["cauldron".to_string()]);
        // link_node needs p >= 0.75: 0.8 links.
        assert_eq!(s.links, vec!["Q-2".to_string()]);
        let run = s.run.unwrap();
        assert!(run.line.starts_with("jev · 1 region"), "{}", run.line);
        assert!(!run.line.contains("stand-in"), "{}", run.line);
        assert_eq!(run.cost, Some(0.0001));
        assert_eq!(run.tree[0].model, "jev · type-one");
        assert!(run.tree[0].items.iter().all(|i| i.steps[0].p.keys().all(|k| k == "jev")));
    }

    #[test]
    fn when_the_real_call_cannot_answer_the_stand_in_does_and_says_why() {
        let p = policy();
        let pick = |_: &RippleDef| Err::<Box<dyn TypeOneCall>, _>("no `type-one` binary".to_string());
        let s = seed_through(&p, &pick, &LocalMatcher, "the overnight dreaming", &items(), "Dream", "r-1", chrono::Local::now());
        let run = s.run.unwrap();
        assert!(run.line.starts_with("stand-in: local matcher · "), "{}", run.line);
        assert!(run.line.ends_with("not the model: no `type-one` binary"), "{}", run.line);
        assert!(run.tree[0].model.contains("stand-in for the type-one call"));
        // A failing adapter, too — named, never silent.
        struct Fails;
        impl TypeOneCall for Fails {
            fn adapter(&self) -> String {
                "haiku".into()
            }
            fn stand_in(&self) -> bool {
                false
            }
            fn ask(&self, _: &RippleDef, _: &str, _: &str, _: &[Candidate], _: &str, _: &str) -> Result<Answer, String> {
                Err("unknown method: threads.create".into())
            }
        }
        let pick = |_: &RippleDef| Ok(Box::new(Fails) as Box<dyn TypeOneCall>);
        let s = seed_through(&p, &pick, &LocalMatcher, "the overnight dreaming", &items(), "Dream", "r-1", chrono::Local::now());
        assert!(s.run.unwrap().line.ends_with("not the model: haiku did not answer: unknown method: threads.create"));
        // A short answer is a failure, not a partial seeding.
        let pick = |_: &RippleDef| Ok(Box::new(Canned(vec![0.9], None)) as Box<dyn TypeOneCall>);
        let s = seed_through(&p, &pick, &LocalMatcher, "x", &items(), "D", "r", chrono::Local::now());
        assert!(s.run.unwrap().line.contains("not the model: jev answered 1 of 4 items"));
    }

    #[test]
    fn the_model_row_selects_the_adapter() {
        let mut p = policy();
        let live = Live::defaults();
        // The seed policy's row: trusted secondary → type_one.secondary → jev.
        assert_eq!(wanted_adapter(p.get(SEED_RIPPLE).unwrap(), &live), Ok(Adapter::Jev));
        // No side on the row: the cauldron's trusted (primary) → haiku.
        with(&mut p, |r| r.model.trusted = None);
        assert_eq!(wanted_adapter(p.get(SEED_RIPPLE).unwrap(), &live), Ok(Adapter::Haiku));
        // ...and a LIVE override of that setting is what decides.
        let flipped = Live { trusted: "secondary".into(), ..live.clone() };
        assert_eq!(wanted_adapter(p.get(SEED_RIPPLE).unwrap(), &flipped), Ok(Adapter::Jev));
        // A live role override: type_one.primary set to jev in settings.toml.
        let over = Live { primary: "jev".into(), ..live.clone() };
        assert_eq!(wanted_adapter(p.get(SEED_RIPPLE).unwrap(), &over), Ok(Adapter::Jev));
        let not_one = Live { primary: "sonnet".into(), ..live.clone() };
        assert!(wanted_adapter(p.get(SEED_RIPPLE).unwrap(), &not_one).unwrap_err().contains("`sonnet`, which is not a type-one adapter"));
        with(&mut p, |r| r.model.trusted = Some("blend".into()));
        assert!(wanted_adapter(p.get(SEED_RIPPLE).unwrap(), &live).is_err());
        with(&mut p, |r| r.model.model = Some("haiku".into()));
        assert_eq!(wanted_adapter(p.get(SEED_RIPPLE).unwrap(), &live), Ok(Adapter::Haiku));
    }

    #[test]
    fn the_ask_is_one_batched_call_with_the_definitions_question() {
        let p = policy();
        let r = p.get(SEED_RIPPLE).unwrap();
        let q = p.fragment("q_seed_inside", 1).unwrap();
        let a = type_one::ask_json(r, q, "the overnight dreaming", &items(), "r-1", "curator@x").unwrap();
        let qs = a["questions"].as_array().unwrap();
        assert_eq!(qs.len(), 4);
        assert_eq!(qs[0]["key"], "inside_0");
        assert_eq!(qs[3]["key"], "inside_3");
        assert_eq!(qs[0]["template_id"], "cluster.seed.inside.v0");
        assert_eq!(qs[0]["kind"]["type"], "yes_no");
        assert_eq!(qs[0]["kind"]["if_true"], "it falls inside the boundaries he stated");
        let ins = qs[1]["instructions"].as_str().unwrap();
        assert!(ins.contains("the overnight dreaming") && ins.contains("[1] region R-20 — LANDING"), "{ins}");
        assert!(!ins.contains("{{slot:"), "{ins}");
        assert!(a["state"].as_str().unwrap().contains("[2] repo cauldron"));
    }

    #[test]
    fn a_decision_missing_an_answer_is_an_error_never_a_default() {
        let d = serde_json::json!({
            "ask_id": "a1",
            "resolved": {"inside_0": {"true": 0.7, "false": 0.3}},
            "per_adapter": [{"adapter": "jev", "cost_usd": 0.00002, "latency_ms": 500, "input_tokens": 900}]
        });
        let a = type_one::parse_decision(&d, "inside", 1, Adapter::Jev).unwrap();
        assert_eq!((a.p, a.cost_usd, a.latency_ms, a.input_tokens), (vec![0.7], Some(0.00002), Some(500), Some(900)));
        assert!(type_one::parse_decision(&d, "inside", 2, Adapter::Jev).unwrap_err().contains("inside_1"));
        assert!(type_one::parse_decision(&d, "inside", 1, Adapter::Haiku).is_err());
    }

    /// The CLI adapter end to end against a scripted `type-one`: the args it
    /// is run with, the decision it parses, and a failing exit named.
    #[cfg(unix)]
    #[test]
    fn the_cli_adapter_runs_type_one_ask_and_reads_its_decision() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("seeding-cli-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("type-one");
        let args = dir.join("args");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$@\" > {}\nif [ \"$5\" = haiku ]; then echo 'Error: threads: unknown method: threads.create' >&2; exit 1; fi\n\
                 echo '{{\"ask_id\":\"a\",\"resolved\":{{\"inside_0\":{{\"true\":0.9,\"false\":0.1}},\"inside_1\":{{\"true\":0.2,\"false\":0.8}}}},\"per_adapter\":[{{\"adapter\":\"jev\",\"cost_usd\":0.00001,\"latency_ms\":321}}]}}'\n",
                args.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let p = policy();
        let r = p.get(SEED_RIPPLE).unwrap();
        let q = p.fragment("q_seed_inside", 1).unwrap();
        let two = &items()[..2];
        let a = TypeOneCli::new(bin.clone(), Adapter::Jev).ask(r, q, "dreaming", two, "r-1", "c@x").unwrap();
        assert_eq!(a.p, vec![0.9, 0.2]);
        assert_eq!(a.latency_ms, Some(321));
        let said = std::fs::read_to_string(&args).unwrap();
        assert!(said.starts_with("ask --ask-file ") && said.trim_end().ends_with("--adapters jev --json"), "{said}");
        let e = TypeOneCli::new(bin, Adapter::Haiku).ask(r, q, "dreaming", two, "r-1", "c@x").unwrap_err();
        assert!(e.contains("exited 1") && e.contains("unknown method: threads.create"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

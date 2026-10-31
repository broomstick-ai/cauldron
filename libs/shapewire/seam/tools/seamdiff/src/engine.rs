// ─────────────────────────────────────────────────────────────────────────────
//  THE REFERENCE DIFF — the mechanism `docs/consumer-seam.md` §7.1 specifies
// ─────────────────────────────────────────────────────────────────────────────
//
// "Discipline is not a mechanism." This file is the mechanism: a pure engine
// over two texts that answers whether a copy of the seam has drifted from the
// reference, and refuses to be satisfied by anything but byte identity outside
// a fence whose id is on a list.
//
// It is `include!`-ed by three things — the CLI beside it, its own forcing
// tests, and each face's `tests/seam_reference.rs` in the consumer repo — so it
// has NO dependencies, not even serde: a face must be able to compile its own
// guard without adding a dev-dependency to do it. `tiny_json` at the bottom is
// there for exactly that reason and does exactly as much as the inventory
// guard needs.
//
// ## Why a copy may not simply be "close enough"
//
// The seam's hand-authored files are a BACKEND copied into two programs. The thing that
// makes that honest is not that the copies are similar, it is that a reviewer
// can diff them and read NOTHING — so every line of difference has to be
// inside a fence that names a reason, and the set of reasons is a closed list
// in `docs/consumer-seam.md` §5. A difference nobody can name is drift, and
// drift in a mirror shows the operator a wrong number on a screen.

/// **THE SEVEN FILES, AND NOTHING ELSE IS READ.**
///
/// Named rather than globbed, deliberately. A tool that walked a directory
/// would read its own source the moment somebody put it in that directory, and
/// a source-scanning guard that witnesses itself is a guard that passes because
/// of the comment explaining it. See [`stray_fences`], which does walk a tree,
/// for the exclusion that case needs and the proof that the exclusion is not
/// vacuous.
///
/// **`inventory.rs` IS THE SEVENTH, AND IT IS HERE FOR A REASON THE OTHER SIX DO
/// NOT HAVE.** It is GENERATED, so it has no fence and never will, and a
/// byte-diff of it looks like a formality. It is not: `check_generated` proves
/// the copy's `inventory.rs` agrees with the copy's OWN `inventory.json`, which a
/// face that vendored a stale PAIR satisfies perfectly. The byte-diff against the
/// reference is what catches that — the copy compiling against a surface the
/// chassis has since moved past. Both halves are needed and neither subsumes the
/// other.
pub const FILES: &[&str] = &[
    "mod.rs",
    "link.rs",
    "held.rs",
    "view.rs",
    "commands.rs",
    "ledger.rs",
    "inventory.rs",
];

/// **THE CLOSED LIST OF SANCTIONED DIVERGENCE IDS** — `docs/consumer-seam.md`
/// §5's table. An id outside this list is refused even if the reference and the
/// copy agree on it, because the table is the operator's lever and a fence
/// invented in a face is a divergence nobody ruled on.
pub const ALLOWED: &[&str] = &["D1", "D2", "D3", "D4", "D5", "D6"];

/// **THE LINE BUDGET.** `docs/consumer-seam.md`: *"fewer than sixty lines may
/// differ"*.
///
/// It counts CODE lines inside fences — not the two marker lines, not blanks,
/// and not comment-only lines. That is a ruling and here is the argument: a
/// divergence has to be allowed to explain itself, and D1's explanation cites
/// an operator message id. Counting the explanation against the code budget
/// creates pressure to delete the explanation, which is the opposite of what
/// the fence is for. What the budget bounds is divergent BEHAVIOUR, and
/// behaviour is code.
pub const BUDGET: usize = 60;

const OPEN: &str = "// SEAM-DIVERGE(";
const CLOSE: &str = "// SEAM-END";

/// **A MARKER IS A LINE WHOSE WHOLE CONTENT IS THE MARKER COMMENT**, and that
/// strictness is the first half of not witnessing yourself.
///
/// The looser rule — the literal anywhere on the line — was what this engine
/// shipped with for one test run, and it reddened on the reference's OWN
/// `mod.rs`, whose module doc describes the mechanism in prose:
///
/// ```text
/// //! Each LIVE row is a `// SEAM-DIVERGE(D<n>)` … `// SEAM-END` fence in the
/// ```
///
/// That is the self-witnessing class in its purest form — a file documenting a
/// guard, parsed by the guard — and the fix is structural rather than an
/// exclusion: a doc comment (`//!`, `///`) is prose, prose fences nothing, and
/// a marker that is not alone on its own `//` line is not a marker. So the
/// documentation can say the word and the parser does not see it, with no list
/// to keep up to date.
fn marker_open(line: &str) -> bool {
    line.trim_start().starts_with(OPEN)
}

/// And the close is EXACT: `// SEAM-END` and nothing after it. A trailing
/// remark would be prose on a structural line, and there is a line above it to
/// put prose on.
fn marker_close(line: &str) -> bool {
    line.trim() == CLOSE
}

// ─────────────────────────────────────────────────────────────────────────────
//  What a fence is
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    pub id: String,
    /// Whatever the author wrote after `SEAM-DIVERGE(D<n>):`. Required and
    /// required to be non-empty: a fence with no reason is the thing this whole
    /// mechanism exists to prevent.
    pub reason: String,
    /// 1-based line of the `SEAM-DIVERGE` marker.
    pub line: usize,
    /// The lines between the two markers, markers excluded.
    pub body: Vec<String>,
}

impl Fence {
    /// Lines of this fence that count against [`BUDGET`].
    pub fn code_lines(&self) -> usize {
        self.body
            .iter()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with("//")
            })
            .count()
    }
}

/// One file, split into the text that must match and the fences that need not.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    /// Every line outside every fence, with its 1-based line number. **The
    /// marker lines are NOT here**: they are part of the fence, so a reference
    /// that opens a fence where a copy does not shows up as a difference in the
    /// surrounding text, which is exactly what it is.
    pub outside: Vec<(usize, String)>,
    pub fences: Vec<Fence>,
}

impl Parsed {
    pub fn ids(&self) -> Vec<String> {
        self.fences.iter().map(|f| f.id.clone()).collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Findings
// ─────────────────────────────────────────────────────────────────────────────

/// Everything this engine can say went wrong. One variant per independently
/// reddenable property, so a test can force one without forcing the others —
/// which is what *"oppose the forcings"* needs: a diff that permits everything
/// reddens [`Finding::OutsideDiffers`], and a diff that permits nothing reddens
/// [`Finding::FenceSequence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// `SEAM-DIVERGE(` with no `SEAM-END` after it.
    Unclosed { file: String, id: String, line: usize },
    /// `SEAM-END` with no open fence.
    Unopened { file: String, line: usize },
    /// A fence inside a fence. Refused rather than supported: nested fences
    /// make the id sequence ambiguous and the budget double-count.
    Nested { file: String, line: usize, outer: String },
    /// The marker did not parse as `SEAM-DIVERGE(D<n>): <reason>`.
    Malformed { file: String, line: usize, text: String },
    /// A fence with no reason after the colon.
    NoReason { file: String, id: String, line: usize },
    /// An id that is not in [`ALLOWED`].
    IdNotAllowed { file: String, id: String, line: usize },
    /// The copy's fence ids, in order, are not the reference's. Covers both
    /// halves: a fence the reference declares and the copy dropped, and a fence
    /// the copy invented.
    FenceSequence {
        file: String,
        reference: Vec<String>,
        copy: Vec<String>,
    },
    /// The text outside the fences is not byte-identical.
    OutsideDiffers {
        file: String,
        reference_line: Option<usize>,
        copy_line: Option<usize>,
        reference: Option<String>,
        copy: Option<String>,
    },
    /// Over [`BUDGET`].
    BudgetExceeded {
        counted: usize,
        budget: usize,
        per_fence: Vec<(String, String, usize)>,
    },
    /// A file named in [`FILES`] that is not there at all.
    FileMissing { file: String, side: String },
    /// A `SEAM-DIVERGE` marker somewhere in the tree that is not one of the seven
    /// seam files. See [`stray_fences`].
    StrayFence { path: String, line: usize },
    /// The inventory guard's findings.
    MethodNotRouted { template: String },
    ShapeNotServed { shape: String },
    ContractNotServed { shape: String, version: u64, serves: Vec<u64> },
    StampDisagrees { declared: String, inventory: String },
    /// **THE CELL GUARD'S FINDINGS** — `shapewire::cell`, routed the way methods
    /// are. A `view.rs` read names a cell the producer does not declare on that
    /// shape: no export will ever carry it, so the read answers *not yet* for
    /// ever. This is the `pending` hole, at the cell layer.
    CellNotDeclared {
        file: String,
        line: usize,
        shape: String,
        cell: String,
        shared: bool,
        declares: Vec<String>,
    },
    /// A read names a shape const `held.rs` does not define, so the guard
    /// cannot even say which shape to check it against. A scanner that skipped
    /// it would be a read nobody checks.
    ShapeConstUnknown { file: String, line: usize, name: String },
    /// The inventory carries no manifest for a shape a read names: it was taken
    /// before the producer declared its cells, and a read cannot be checked
    /// against a manifest that is not there.
    ManifestUnstated { shape: String },
    /// A word `view.rs` compares a value against that no closed vocabulary the
    /// producer declares contains — a predicate that is false for ever. This is
    /// the `pending` hole, at the word layer.
    WordNotDeclared {
        file: String,
        line: usize,
        word: String,
        vocabularies: Vec<(String, Vec<String>)>,
    },
    /// A `.get("name")` in `view.rs` naming a field no cell on any shape
    /// declares — a read that can only ever answer `None`.
    FieldNotDeclared { file: String, line: usize, name: String },
    /// **`seam/inventory.rs` IS NOT THERE, OR IS NOT A GENERATED INVENTORY.**
    /// The kit's seventh file is what the consumer COMPILES against; without it
    /// the surface is a JSON file a test reads and nothing else.
    GeneratedMissing { why: String },
    /// `inventory.rs` and `inventory.json` were taken at different commits, which
    /// one run of the generator cannot produce.
    GeneratedStale { rust: String, json: String },
    /// `inventory.rs` and `inventory.json` do not describe the same surface.
    GeneratedDisagrees {
        what: String,
        missing: Vec<String>,
        extra: Vec<String>,
    },
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Finding::Unclosed { file, id, line } => write!(
                f,
                "{file}:{line}: SEAM-DIVERGE({id}) is never closed by a SEAM-END"
            ),
            Finding::Unopened { file, line } => {
                write!(f, "{file}:{line}: SEAM-END with no fence open")
            }
            Finding::Nested { file, line, outer } => write!(
                f,
                "{file}:{line}: a fence inside SEAM-DIVERGE({outer}); fences do not nest"
            ),
            Finding::Malformed { file, line, text } => write!(
                f,
                "{file}:{line}: a marker that is not `SEAM-DIVERGE(D<n>): <reason>`: {text}"
            ),
            Finding::NoReason { file, id, line } => write!(
                f,
                "{file}:{line}: SEAM-DIVERGE({id}) states no reason; §5's table is the list of reasons"
            ),
            Finding::IdNotAllowed { file, id, line } => write!(
                f,
                "{file}:{line}: {id} is not a sanctioned divergence id (allowed: {})",
                ALLOWED.join(" ")
            ),
            Finding::FenceSequence {
                file,
                reference,
                copy,
            } => write!(
                f,
                "{file}: the fence ids differ from the reference's.\n  \
                 reference: [{}]\n  copy:      [{}]\n  \
                 a fence the reference declares and this copy dropped means the copy took the \
                 reference's answer where it was supposed to give its own; a fence the copy \
                 invented is a divergence nobody ruled on.",
                reference.join(" "),
                copy.join(" ")
            ),
            Finding::OutsideDiffers {
                file,
                reference_line,
                copy_line,
                reference,
                copy,
            } => {
                writeln!(f, "{file}: the text outside the fences differs.")?;
                match (reference_line, reference) {
                    (Some(n), Some(t)) => writeln!(f, "  reference:{n}: {t}")?,
                    _ => writeln!(f, "  reference: (the file ends here)")?,
                }
                match (copy_line, copy) {
                    (Some(n), Some(t)) => write!(f, "  copy:{n}: {t}"),
                    _ => write!(f, "  copy: (the file ends here)"),
                }
            }
            Finding::BudgetExceeded {
                counted,
                budget,
                per_fence,
            } => {
                writeln!(
                    f,
                    "the divergence budget is {budget} code lines and this copy spends {counted}:"
                )?;
                for (file, id, n) in per_fence {
                    writeln!(f, "  {file}  {id}  {n}")?;
                }
                write!(
                    f,
                    "  (code lines only — markers, blanks and comments are free)"
                )
            }
            Finding::FileMissing { file, side } => {
                write!(f, "{side}: {file} is not there")
            }
            Finding::StrayFence { path, line } => write!(
                f,
                "{path}:{line}: a SEAM-DIVERGE marker outside the seven seam files. A fence is \
                 only meaningful where the diff looks."
            ),
            Finding::MethodNotRouted { template } => write!(
                f,
                "`{template}` does not route against the producer's inventory \
                 (WS_METHODS u CHASSIS_METHODS). The daemon answers `unknown_method`."
            ),
            Finding::ShapeNotServed { shape } => {
                write!(f, "the book names shape `{shape}`, which the producer does not serve")
            }
            Finding::ContractNotServed {
                shape,
                version,
                serves,
            } => write!(
                f,
                "`{shape}` serves contract versions {serves:?} and the seam declares {version}; \
                 rule 2.6 refuses that link"
            ),
            Finding::StampDisagrees {
                declared,
                inventory,
            } => write!(
                f,
                "SHAPE_VERSION says `{declared}` and inventory.json was taken at `{inventory}`; \
                 vendor-chassis.sh is the only thing that may move either"
            ),
            Finding::CellNotDeclared {
                file,
                line,
                shape,
                cell,
                shared,
                declares,
            } => write!(
                f,
                "{file}:{line}: reads {level} cell `{cell}` of `{shape}`, which the producer does \
                 not declare. A cell the producer does not declare is a cell no export will ever \
                 carry: the mirror answers `NotHydrated` for the life of the process, which reads \
                 as \"not yet\" and means \"never\". `{shape}` declares: {list}. WHAT IS MISSING: \
                 either the cell on the producer (`Shape::CELLS`, then re-run \
                 tools/make-inventory.py) or this read.",
                level = if *shared { "shape-level" } else { "per-key" },
                list = if declares.is_empty() {
                    "nothing at this level".to_string()
                } else {
                    declares.join(", ")
                }
            ),
            Finding::ShapeConstUnknown { file, line, name } => write!(
                f,
                "{file}:{line}: reads a cell of `{name}`, which is not a shape const held.rs \
                 declares, so the guard cannot say which manifest to check it against. Name the \
                 shape by its held.rs const."
            ),
            Finding::ManifestUnstated { shape } => write!(
                f,
                "inventory.json carries no cell manifest for `{shape}`: it was taken before the \
                 producer declared its cells. A read cannot be checked against a manifest that is \
                 not there, and an unchecked read is the hole the manifest exists to close. \
                 Re-run tools/make-inventory.py against a producer that declares `Shape::CELLS`."
            ),
            Finding::WordNotDeclared {
                file,
                line,
                word,
                vocabularies,
            } => {
                writeln!(
                    f,
                    "{file}:{line}: compares against the word `{word}`, which no closed vocabulary \
                     the producer declares contains. A predicate over a word no producer emits is \
                     false for ever and reads, to anyone above it, like an answer. The producer's \
                     vocabularies:"
                )?;
                for (where_, words) in vocabularies {
                    writeln!(f, "    {where_} = [{}]", words.join(", "))?;
                }
                write!(
                    f,
                    "  WHAT IS MISSING: either the word on the producer (an `Enum` in its \
                     `Shape::CELLS`, then re-run tools/make-inventory.py) or this comparison."
                )
            }
            Finding::FieldNotDeclared { file, line, name } => write!(
                f,
                "{file}:{line}: reads `.get(\"{name}\")`, and no cell or field the producer \
                 declares on any shape is called `{name}`. A field the producer does not declare \
                 is one this read can only ever answer `None` for. WHAT IS MISSING: either the \
                 field on the producer (in its `Shape::CELLS`, then re-run \
                 tools/make-inventory.py) or this read."
            ),
            Finding::GeneratedMissing { why } => write!(
                f,
                "inventory.rs is not a generated inventory: {why}. It is the kit's seventh file \
                 and the one the consumer COMPILES against — without it the producer's surface \
                 is a JSON file a test reads and nothing more, which is the arrangement that \
                 let `thread.replace` and `thread.unpend` ship. WHAT IS MISSING: \
                 tools/make-inventory.py --rust-only, which regenerates it from the committed \
                 inventory.json, then commit it."
            ),
            Finding::GeneratedStale { rust, json } => write!(
                f,
                "inventory.rs was generated at `{rust}` and inventory.json was taken at \
                 `{json}`. One run of tools/make-inventory.py writes both from one payload, so \
                 two commits means one of them was hand-edited or one of them was not \
                 regenerated — and the consumer is compiling against a surface its own \
                 inventory does not claim. WHAT IS MISSING: tools/make-inventory.py --rust-only \
                 if the Rust is behind, or a revert if the Rust is ahead; the JSON is the \
                 artifact a reviewer saw land and it wins."
            ),
            Finding::GeneratedDisagrees {
                what,
                missing,
                extra,
            } => {
                writeln!(
                    f,
                    "inventory.rs and inventory.json disagree about {what}. One run of \
                     tools/make-inventory.py writes both from one payload, so a disagreement is \
                     a hand-edit — and a hand-edited inventory.rs is a consumer compiling \
                     against its own opinion of what the producer serves, which is the one \
                     thing docs/consumer-seam.md §7.2 says the seam cannot survive."
                )?;
                if !missing.is_empty() {
                    writeln!(
                        f,
                        "  in inventory.json and NOT in inventory.rs: {}",
                        missing.join(", ")
                    )?;
                }
                if !extra.is_empty() {
                    writeln!(
                        f,
                        "  in inventory.rs and NOT in inventory.json: {}",
                        extra.join(", ")
                    )?;
                }
                write!(
                    f,
                    "  WHAT IS MISSING: tools/make-inventory.py --rust-only, which regenerates \
                     inventory.rs from the committed inventory.json. If the JSON is the stale \
                     one, re-run tools/make-inventory.py --declared against the producer and \
                     commit both."
                )
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Split one file's text into what must match and what need not.
///
/// A marker is recognised by [`marker_open`] / [`marker_close`]: a `//` comment
/// alone on its line, at any indent, including inside a `json!` literal — which
/// is where the AUI's identity fence sits. See [`marker_open`] for why the
/// recognition is that strict and not merely "the literal appears".
pub fn parse(file: &str, text: &str) -> (Parsed, Vec<Finding>) {
    let mut out = Parsed::default();
    let mut findings = Vec::new();
    let mut open: Option<Fence> = None;

    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if marker_open(line) {
            let at = line.find(OPEN).unwrap_or(0);
            if let Some(o) = &open {
                findings.push(Finding::Nested {
                    file: file.to_string(),
                    line: n,
                    outer: o.id.clone(),
                });
                continue;
            }
            let rest = &line[at + OPEN.len()..];
            let Some(close) = rest.find(')') else {
                findings.push(Finding::Malformed {
                    file: file.to_string(),
                    line: n,
                    text: line.trim().to_string(),
                });
                continue;
            };
            let id = rest[..close].trim().to_string();
            if !is_id(&id) {
                findings.push(Finding::Malformed {
                    file: file.to_string(),
                    line: n,
                    text: line.trim().to_string(),
                });
                continue;
            }
            let reason = rest[close + 1..]
                .trim_start()
                .trim_start_matches(':')
                .trim()
                .to_string();
            if reason.is_empty() {
                findings.push(Finding::NoReason {
                    file: file.to_string(),
                    id: id.clone(),
                    line: n,
                });
            }
            if !ALLOWED.contains(&id.as_str()) {
                findings.push(Finding::IdNotAllowed {
                    file: file.to_string(),
                    id: id.clone(),
                    line: n,
                });
            }
            open = Some(Fence {
                id,
                reason,
                line: n,
                body: Vec::new(),
            });
            continue;
        }
        if marker_close(line) {
            match open.take() {
                Some(fence) => out.fences.push(fence),
                None => findings.push(Finding::Unopened {
                    file: file.to_string(),
                    line: n,
                }),
            }
            continue;
        }
        match &mut open {
            Some(fence) => fence.body.push(line.to_string()),
            None => out.outside.push((n, line.to_string())),
        }
    }

    if let Some(fence) = open {
        findings.push(Finding::Unclosed {
            file: file.to_string(),
            id: fence.id,
            line: fence.line,
        });
    }
    (out, findings)
}

fn is_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('D') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

// ─────────────────────────────────────────────────────────────────────────────
//  Comparing
// ─────────────────────────────────────────────────────────────────────────────

/// Diff ONE file. The three properties, each independently reddenable:
///
/// 1. every id in the copy is in [`ALLOWED`] (done in [`parse`]);
/// 2. the copy's fence ids, in order, are the reference's;
/// 3. the text outside the fences is byte-identical.
pub fn compare_file(file: &str, reference: &str, copy: &str) -> (Parsed, Vec<Finding>) {
    let (r, mut findings) = parse(file, reference);
    let (c, cf) = parse(file, copy);
    findings.extend(cf);

    if r.ids() != c.ids() {
        findings.push(Finding::FenceSequence {
            file: file.to_string(),
            reference: r.ids(),
            copy: c.ids(),
        });
    }

    // **FIRST DIFFERENCE ONLY, AND BY POSITION RATHER THAN BY A DIFF
    // ALGORITHM.** A real diff would report an insertion as one hunk; this
    // reports it as a mismatch at the line where the two texts part company,
    // which is the line a reader has to look at either way. No alignment means
    // no chance of the tool deciding two texts are "equivalent".
    let mut i = 0usize;
    loop {
        match (r.outside.get(i), c.outside.get(i)) {
            (None, None) => break,
            (Some((rn, rl)), Some((cn, cl))) if rl == cl => {
                let _ = (rn, cn);
                i += 1;
            }
            (rr, cc) => {
                findings.push(Finding::OutsideDiffers {
                    file: file.to_string(),
                    reference_line: rr.map(|(n, _)| *n),
                    copy_line: cc.map(|(n, _)| *n),
                    reference: rr.map(|(_, l)| l.clone()),
                    copy: cc.map(|(_, l)| l.clone()),
                });
                break;
            }
        }
    }
    (c, findings)
}

/// The budget, over the whole copy. Separate from [`compare_file`] because it
/// is a property of the COPY and not of any one file, which is also why a face
/// cannot buy room by spreading a divergence over three files.
pub fn budget(copy: &[(String, Parsed)]) -> Option<Finding> {
    let mut per_fence = Vec::new();
    let mut counted = 0usize;
    for (file, p) in copy {
        for fence in &p.fences {
            let n = fence.code_lines();
            counted += n;
            per_fence.push((file.clone(), fence.id.clone(), n));
        }
    }
    if counted >= BUDGET {
        return Some(Finding::BudgetExceeded {
            counted,
            budget: BUDGET,
            per_fence,
        });
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────────
//  The stray-fence guard, and the thing it must not witness
// ─────────────────────────────────────────────────────────────────────────────

/// **THE ONE FILE A TREE WALK MUST SKIP, AND IT IS EXACTLY ONE.**
///
/// This is the self-witnessing case, named. A source-scanning guard that reads
/// its own source finds the pattern it is looking for and either passes or
/// fails on itself; either way it is not measuring what it claims to.
///
/// [`marker_open`]'s strictness removes most of the problem structurally —
/// this engine's own source, its CLI, and the `README.md` all discuss markers
/// in prose or in string literals and none of them matches. What is LEFT is
/// `tests/forcings.rs`, which embeds whole fenced FIXTURES as multi-line string
/// literals, so its raw lines really are markers. One file, for a stated
/// reason.
///
/// The exclusion is proved NON-VACUOUS by
/// `tests/forcings.rs::the_stray_fence_exclusion_is_not_vacuous`, which asserts
/// that every excluded path exists AND that the STRICT detector finds a marker
/// in it. An exclusion list with a path on it that needs no excluding is an
/// exclusion that has stopped being a measurement — which is how this list
/// shrank from four entries to one: three of them were only on it because the
/// detector was too loose, and `src/main.rs` was on it with no marker in it at
/// all.
pub const SELF_EXCLUDED: &[&str] = &["tools/seamdiff/tests/forcings.rs"];

/// Walk `root` for `SEAM-DIVERGE` markers in files that are not one of the seven.
///
/// A fence in a seventh file is not a divergence: nothing diffs that file, so
/// the fence buys silence instead of permission. `probe/` is skipped as a
/// directory because it is a separate crate with its own job.
pub fn stray_fences(root: &std::path::Path) -> Vec<Finding> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    out
}

fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<Finding>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if path.is_dir() {
            if rel == "probe" || rel == "target" || rel.ends_with("/target") {
                continue;
            }
            walk(root, &path, out);
            continue;
        }
        if SELF_EXCLUDED.contains(&rel.as_str()) {
            continue;
        }
        if FILES.contains(&rel.as_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if marker_open(line) {
                out.push(Finding::StrayFence {
                    path: rel.clone(),
                    line: i + 1,
                });
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  The inventory guard — `docs/consumer-seam.md` §7.2
// ─────────────────────────────────────────────────────────────────────────────

/// The producer's method templates, shape contracts and cell manifests, as
/// `inventory.json` carries them.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub source_commit: String,
    pub methods: Vec<String>,
    pub shapes: Vec<InvShape>,
}

/// One shape's entry: its contract, and — since the producer declares its cells
/// (`shapewire::cell`) — its manifest.
#[derive(Debug, Clone, Default)]
pub struct InvShape {
    pub name: String,
    pub version: u64,
    pub serves: Vec<u64>,
    /// **`None` IS "THIS INVENTORY WAS TAKEN BEFORE THE PRODUCER DECLARED ITS
    /// CELLS", AND IS NOT AN EMPTY MANIFEST.** The guard reports it as a finding
    /// of its own ([`Finding::ManifestUnstated`]) rather than passing every read
    /// or failing every read: an inventory with no manifest cannot check a read,
    /// and an unchecked read is the hole the manifest exists to close.
    pub cells: Option<Vec<InvCell>>,
    pub shared_cells: Option<Vec<InvCell>>,
}

/// One declared cell: its name, the closed words of its enum leaf (empty if the
/// leaf is not an enum), and its struct fields one level deep.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InvCell {
    pub name: String,
    pub words: Vec<String>,
    pub fields: Vec<InvField>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InvField {
    pub name: String,
    pub words: Vec<String>,
}

impl InvShape {
    /// Every cell name at both levels, for a finding to quote.
    fn declares(&self, shared: bool) -> Option<Vec<String>> {
        let list = if shared { &self.shared_cells } else { &self.cells };
        list.as_ref().map(|c| c.iter().map(|x| x.name.clone()).collect())
    }
}

/// Right-to-left template match, the same one `shapewire::server::route` does:
/// equal segment counts, and a segment that is `{…}` in the template matches
/// anything. The direction matters because `threads/{id}/send` and
/// `threads/create` have the same length and only the fixed tail tells them
/// apart.
pub fn routes(templates: &[String], path: &str) -> bool {
    let p: Vec<&str> = path.split('/').collect();
    templates.iter().any(|t| {
        let t: Vec<&str> = t.split('/').collect();
        t.len() == p.len()
            && t.iter()
                .rev()
                .zip(p.iter().rev())
                .all(|(a, b)| (a.starts_with('{') && a.ends_with('}')) || a == b)
    })
}

/// **THE CONTRACT VERSION A COPY STATES, PER SHAPE** — rule 2.6 is per shape,
/// so the seam's `mod.rs` carries a table (`SHAPE_CONTRACT_VERSIONS`) and a
/// default (`SHAPE_CONTRACT_DEFAULT`) for a shape with no row. Read out of the
/// copy's TEXT by [`scan_contracts`]. Until R2.S8 this was one integer for every
/// shape, which could only be right while every shape sat at one version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contracts {
    pub default: u64,
    pub per_shape: Vec<(String, u64)>,
}

impl Contracts {
    /// Every shape at one version — the old single-integer form, kept for
    /// fixtures that are about something else.
    pub fn uniform(v: u64) -> Self {
        Self { default: v, per_shape: Vec::new() }
    }

    /// The version stated for `shape`.
    pub fn of(&self, shape: &str) -> u64 {
        self.per_shape
            .iter()
            .find(|(s, _)| s == shape)
            .map(|(_, v)| *v)
            .unwrap_or(self.default)
    }
}

/// Read `SHAPE_CONTRACT_VERSIONS` and `SHAPE_CONTRACT_DEFAULT` out of a copy's
/// `mod.rs`. `None` when either is missing or a row does not parse — the
/// caller treats that as version 0 for every shape, which no producer serves,
/// so an unreadable table is RED rather than vacuously green.
pub fn scan_contracts(mod_rs: &str) -> Option<Contracts> {
    let default = scan_const(mod_rs, "SHAPE_CONTRACT_DEFAULT")?
        .replace('_', "")
        .parse()
        .ok()?;
    let start = mod_rs.find("pub const SHAPE_CONTRACT_VERSIONS:")?;
    let body = &mod_rs[start..];
    let open = body.find("= &[")? + 4;
    let close = body[open..].find("];")? + open;
    let mut per_shape = Vec::new();
    for line in body[open..close].lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        let inner = t.trim_end_matches(',').trim().strip_prefix('(')?.strip_suffix(')')?;
        let (name, v) = inner.split_once(',')?;
        let name = name.trim().strip_prefix('"')?.strip_suffix('"')?;
        per_shape.push((name.to_string(), v.trim().replace('_', "").parse().ok()?));
    }
    Some(Contracts { default, per_shape })
}

/// **THE TABLE A COPY STATES, OR VERSION 0 FOR EVERY SHAPE IF IT CANNOT BE
/// READ.** No producer serves 0, so an unreadable or missing table is a finding
/// per shape — RED — and never a silent default that happens to match.
pub fn contracts_of(mod_rs: &str) -> Contracts {
    scan_contracts(mod_rs).unwrap_or(Contracts::uniform(0))
}

/// Check one copy's declared surface against the producer's.
///
/// `templates` is the copy's own `commands::methods::TEMPLATES`, `shapes` its
/// `held.rs` shape consts, and `declared_stamp`/`contracts` its `mod.rs`
/// constants — all read out of the copy's TEXT by
/// [`scan_templates`]/[`scan_shapes`]/[`scan_const`]/[`scan_contracts`], so the
/// guard is checking what the copy actually says rather than what it was told.
///
/// **Every shape is checked at ITS OWN version**: the book's shapes, and every
/// row of the contract table even if the book does not hold it, because a row
/// is a statement the copy sends the day that shape is held.
pub fn check_inventory(
    inv: &Inventory,
    templates: &[String],
    shapes: &[String],
    contracts: &Contracts,
    declared_stamp: &str,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in templates {
        if !routes(&inv.methods, &t.replace("{id}", "x")) {
            out.push(Finding::MethodNotRouted {
                template: t.clone(),
            });
        }
    }
    let mut checked: Vec<String> = shapes.to_vec();
    for (s, _) in &contracts.per_shape {
        if !checked.contains(s) {
            checked.push(s.clone());
        }
    }
    for s in &checked {
        let version = contracts.of(s);
        match inv.shapes.iter().find(|x| &x.name == s) {
            None => out.push(Finding::ShapeNotServed { shape: s.clone() }),
            Some(x) if !x.serves.contains(&version) => out.push(Finding::ContractNotServed {
                shape: s.clone(),
                version,
                serves: x.serves.clone(),
            }),
            Some(_) => {}
        }
    }
    let short = inv.source_commit.chars().take(7).collect::<String>();
    if !declared_stamp.contains(&short) {
        out.push(Finding::StampDisagrees {
            declared: declared_stamp.to_string(),
            inventory: inv.source_commit.clone(),
        });
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE GENERATED INVENTORY — the one check the COMPILER cannot do
// ─────────────────────────────────────────────────────────────────────────────
//
// `seam/inventory.rs` is generated beside `inventory.json`, in the same run, by
// `tools/make-inventory.py`. The consumer's COMPILER now routes every method it
// sends, every cell it reads, every field it gets and every word it compares
// against — those were this guard's findings and they are build errors now.
//
// What the compiler cannot see is whether the Rust it compiled against is the
// same surface as the JSON a reviewer read. Two files written in one run agree by
// construction; one of them HAND-EDITED does not, and a hand-edited
// `inventory.rs` is a consumer compiling against its own opinion of what the
// producer serves — the one thing §7.2 says the seam cannot survive. So that is
// what this checks, and it is the honest division: the build checks the reads
// against the surface, the test checks the surface against the surface.

/// The generated module's surface, read out of its TEXT — never by `use`-ing it,
/// because this engine has no dependencies and is `include!`d into a face's own
/// test.
///
/// **THE SLICES AND THE TOKENS ARE READ SEPARATELY, AND BOTH ARE CHECKED.** That
/// is not redundancy, it is the whole guard: a hand-edit that adds a `Method` or a
/// `Carried` const WITHOUT touching the slice beside it compiles, and a face can
/// then name it. Checking only the slices would miss exactly the edit somebody
/// would make to resurrect a dead verb.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Generated {
    pub source_commit: String,
    /// The `METHODS: &[&str]` slice.
    pub methods: Vec<String>,
    /// The template of every `Method::__served("…")` const — what a consumer can
    /// actually name.
    pub method_tokens: Vec<String>,
    pub shapes: Vec<GenShape>,
}

/// One shape module of the generated inventory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenShape {
    pub name: String,
    /// The `CELLS: &[&str]` slice.
    pub cells: Vec<String>,
    /// The `SHARED: &[&str]` slice.
    pub shared: Vec<String>,
    /// Every `Carried::__declared(SHAPE, "name", <shared>)` const, as
    /// `(name, shared)` — what a consumer can actually name.
    pub tokens: Vec<(String, bool)>,
}

/// Read `inventory.rs`. `Err` when the file is not a generated inventory at all —
/// which is a finding the caller reports rather than a reason to pass.
pub fn parse_generated(text: &str) -> Result<Generated, String> {
    let mut g = Generated::default();
    g.source_commit = scan_const(text, "SOURCE_COMMIT")
        .ok_or("inventory.rs declares no SOURCE_COMMIT")?;
    g.methods = scan_templates(text, "METHODS");
    if g.methods.is_empty() {
        return Err("inventory.rs declares no METHODS, or an empty one".into());
    }
    g.method_tokens = scan_minted(text, "Method::__served(");
    if g.method_tokens.is_empty() {
        return Err("inventory.rs mints no `Method` token, so nothing in it can be sent".into());
    }
    // Each shape is `pub mod <ident> {` with `SHAPE`, `CELLS` and `SHARED` inside.
    // Found by the module header rather than by the SHAPES list, so a module the
    // list forgot is still read and a list entry with no module is still missed —
    // both of which this guard then reports.
    let mut from = 0;
    while let Some(at) = text[from..].find("\npub mod ") {
        let start = from + at + 1;
        let after = &text[start + "pub mod ".len()..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        from = start + 1;
        if name == "methods" {
            continue;
        }
        // The module body ends at the next top-level `pub mod` or at the end.
        let body_start = start;
        let body_end = text[body_start + 1..]
            .find("\npub mod ")
            .map(|e| body_start + 1 + e)
            .unwrap_or(text.len());
        let body = &text[body_start..body_end];
        let Some(shape) = scan_const(body, "SHAPE") else {
            return Err(format!(
                "inventory.rs's module `{name}` declares no SHAPE const, so the guard cannot \
                 say which shape its cells belong to"
            ));
        };
        g.shapes.push(GenShape {
            name: shape,
            cells: scan_templates(body, "CELLS"),
            shared: scan_templates(body, "SHARED"),
            tokens: scan_carried(body),
        });
    }
    if g.shapes.is_empty() {
        return Err("inventory.rs declares no shape modules".into());
    }
    g.shapes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(g)
}

/// Every first string literal after each occurrence of `open` — the mint call's
/// argument. Used for `Method::__served(` and nothing else.
fn scan_minted(text: &str, open: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(open) {
        let start = from + at + open.len();
        from = start;
        let rest = &text[start..];
        let end = rest.find(')').unwrap_or(rest.len());
        if let Some(lit) = string_literals(&rest[..end]).into_iter().next() {
            out.push(lit);
        }
    }
    out
}

/// Every `Carried::__declared(SHAPE, "name", <bool>)` in one shape module's body.
fn scan_carried(body: &str) -> Vec<(String, bool)> {
    let open = "Carried::__declared(";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = body[from..].find(open) {
        let start = from + at + open.len();
        from = start;
        let rest = &body[start..];
        let end = rest.find(')').unwrap_or(rest.len());
        let args = &rest[..end];
        let Some(name) = string_literals(args).into_iter().next() else {
            continue;
        };
        out.push((name, args.contains("true")));
    }
    out
}

/// **IS THE GENERATED RUST THE SAME SURFACE AS THE JSON?**
///
/// Every disagreement is reported with BOTH SIDES named, because the fix depends
/// on which one is stale: a Rust file behind the JSON wants
/// `tools/make-inventory.py --rust-only`, and a Rust file AHEAD of the JSON was
/// hand-edited and wants reverting.
pub fn check_generated(inv: &Inventory, g: &Generated) -> Vec<Finding> {
    let mut out = Vec::new();
    if g.source_commit != inv.source_commit {
        out.push(Finding::GeneratedStale {
            rust: g.source_commit.clone(),
            json: inv.source_commit.clone(),
        });
    }
    for (what, rust) in [
        ("the method list", &g.methods),
        ("the METHOD TOKENS — the consts a consumer can actually name", &g.method_tokens),
    ] {
        let (missing, extra) = diff(&inv.methods, rust);
        if !missing.is_empty() || !extra.is_empty() {
            out.push(Finding::GeneratedDisagrees {
                what: what.to_string(),
                missing,
                extra,
            });
        }
    }
    let json_shapes: Vec<String> = inv.shapes.iter().map(|s| s.name.clone()).collect();
    let rust_shapes: Vec<String> = g.shapes.iter().map(|s| s.name.clone()).collect();
    let (missing, extra) = diff(&json_shapes, &rust_shapes);
    if !missing.is_empty() || !extra.is_empty() {
        out.push(Finding::GeneratedDisagrees {
            what: "the shape list".to_string(),
            missing,
            extra,
        });
        // Comparing manifests against a shape set that does not match would
        // report every shape twice; the set disagreement is the finding.
        return out;
    }
    for s in &inv.shapes {
        let Some(g) = g.shapes.iter().find(|x| x.name == s.name) else {
            continue;
        };
        let tokens = |shared: bool| -> Vec<String> {
            g.tokens
                .iter()
                .filter(|(_, sh)| *sh == shared)
                .map(|(n, _)| n.clone())
                .collect()
        };
        for (level, json, rust) in [
            ("per-key cells", s.cells.as_ref(), g.cells.clone()),
            ("shape-level cells", s.shared_cells.as_ref(), g.shared.clone()),
            (
                "per-key CELL TOKENS — the consts a consumer can actually name",
                s.cells.as_ref(),
                tokens(false),
            ),
            (
                "shape-level CELL TOKENS — the consts a consumer can actually name",
                s.shared_cells.as_ref(),
                tokens(true),
            ),
        ] {
            let Some(json) = json else { continue };
            let json: Vec<String> = json.iter().map(|c| c.name.clone()).collect();
            let (missing, extra) = diff(&json, &rust);
            if !missing.is_empty() || !extra.is_empty() {
                out.push(Finding::GeneratedDisagrees {
                    what: format!("`{}`'s {level}", s.name),
                    missing,
                    extra,
                });
            }
        }
    }
    out
}

/// `(in json and not in rust, in rust and not in json)` — sorted, deduplicated.
fn diff(json: &[String], rust: &[String]) -> (Vec<String>, Vec<String>) {
    let mut missing: Vec<String> = json
        .iter()
        .filter(|x| !rust.contains(x))
        .cloned()
        .collect();
    let mut extra: Vec<String> = rust
        .iter()
        .filter(|x| !json.contains(x))
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    extra.sort();
    extra.dedup();
    (missing, extra)
}

/// The string literals of a `&[&str]` array named `name`, out of Rust text.
pub fn scan_templates(text: &str, name: &str) -> Vec<String> {
    let Some(at) = text.find(&format!("{name}: &[&str] = &[")) else {
        return Vec::new();
    };
    let rest = &text[at..];
    let end = rest.find("];").map(|e| e + at).unwrap_or(text.len());
    string_literals(&text[at..end])
}

/// Every `pub const NAME: &str = "…";` value in `text` whose literal looks like
/// a shape name — which is to say, whatever the copy's `held.rs` declares. Read
/// off the text rather than from a hardcoded list so that a face that adds a
/// shape is checked on the shape it added.
pub fn scan_shapes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with("pub const ") || !t.contains(": &str = \"") {
            continue;
        }
        // The shape consts are the ones whose value is a shape path. `FLEET`
        // and `LINK` are KEYS, not shapes, and are excluded by the one thing
        // that distinguishes them: a key is never the name of a shape the
        // producer serves, so the inventory check would reject them. The
        // discriminator is the declared doc, so take the value and let the
        // caller filter against the inventory's own list of names.
        if let Some(v) = string_literals(t).into_iter().next() {
            out.push(v);
        }
    }
    out
}

/// A `pub const NAME: <ty> = <value>;` value, as text.
pub fn scan_const(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with(&format!("pub const {name}:")) {
            let v = t.split_once('=')?.1.trim().trim_end_matches(';').trim();
            return Some(v.trim_matches('"').to_string());
        }
    }
    None
}

fn string_literals(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '"' {
            let mut j = i + 1;
            let mut lit = String::new();
            while j < b.len() && b[j] != '"' {
                lit.push(b[j]);
                j += 1;
            }
            out.push(lit);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
//  The cell guard — `shapewire::cell`, routed the way methods are
// ─────────────────────────────────────────────────────────────────────────────

/// One cell read `view.rs` makes, as the scanner finds it: the shape CONST it
/// names (resolved against `held.rs` by the caller), the cell literal beside it,
/// whether it is a shape-level read, and the line.
#[derive(Debug, Clone, PartialEq)]
pub struct CellRead {
    pub shape_const: String,
    pub cell: String,
    pub shared: bool,
    pub line: usize,
}

/// A string literal with the line it sits on.
#[derive(Debug, Clone, PartialEq)]
pub struct Lit {
    pub text: String,
    pub line: usize,
}

/// **EVERY CELL READ IN `view.rs`**, by its three call shapes:
///
/// ```text
/// cell(h, SHAPE, <key>, "name", …)      per-key, with a reader
/// items(h, SHAPE, <key>, "name")         per-key, an edge cell's items
/// shared_cell(h, SHAPE, "name", …)       shape-level
/// ```
///
/// The shape is an IDENT (a `held.rs` const) and the cell a LITERAL, which is
/// the discipline `view.rs` keeps so that this scanner can see every read: a
/// read spelled any other way is one the guard does not check, and the
/// reference's own doc on `cell` says so. The scanner finds the call by its
/// `(h, ` opening, which no definition shares (`fn cell<T>(h: &Held` has a
/// colon), takes the ident after the comma and the first string literal
/// before the call's closing paren.
pub fn scan_cell_reads(text: &str) -> Vec<CellRead> {
    let mut out = Vec::new();
    for (needle, shared) in [("cell(h, ", false), ("items(h, ", false), ("shared_cell(h, ", true)] {
        let mut from = 0;
        while let Some(at) = text[from..].find(needle) {
            let start = from + at;
            // `shared_cell(h, ` contains `cell(h, `; a `cell(h, ` match that is
            // really the tail of `shared_cell(h, ` is the other needle's.
            if !shared && needle == "cell(h, " && start >= 7 && &text[start - 7..start] == "shared_" {
                from = start + needle.len();
                continue;
            }
            let args = &text[start + needle.len()..];
            let ident: String = args
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            // The first literal inside the call's parens.
            let mut depth = 1i32;
            let mut lit: Option<String> = None;
            let mut chars = args.char_indices().peekable();
            while let Some((_, c)) = chars.next() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    '"' => {
                        let mut l = String::new();
                        for (_, d) in chars.by_ref() {
                            if d == '"' {
                                break;
                            }
                            l.push(d);
                        }
                        lit = Some(l);
                        break;
                    }
                    _ => {}
                }
            }
            let line = text[..start].matches('\n').count() + 1;
            if let Some(cell) = lit {
                if !ident.is_empty() {
                    out.push(CellRead {
                        shape_const: ident,
                        cell,
                        shared,
                        line,
                    });
                }
            }
            from = start + needle.len();
        }
    }
    out.sort_by_key(|r| r.line);
    out
}

/// **EVERY `pub const NAME: &str = "value";` IN `held.rs`**, name to value — the
/// table a [`CellRead::shape_const`] is resolved through. Keys (`FLEET`, `LINK`)
/// are in it too; a read names a SHAPE const by construction, and one that named
/// a key would be [`Finding::ShapeNotServed`] at the inventory.
pub fn scan_shape_consts(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with("pub const ") || !t.contains(": &str = \"") {
            continue;
        }
        let name = t["pub const ".len()..].split(':').next().unwrap_or("").trim().to_string();
        if let Some(v) = string_literals(t).into_iter().next() {
            out.push((name, v));
        }
    }
    out
}

/// **EVERY WORD `view.rs` COMPARES A VALUE AGAINST**: a literal preceded by `==`
/// or `== Some(`, or any literal among the alternatives of a `matches!(…)`.
/// Those are the two spellings the reference uses for a vocabulary test, and a
/// word compared any other way is one this guard does not see.
pub fn scan_words(text: &str) -> Vec<Lit> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    // `==` and `== Some(`
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // The whole literal, then past its closing quote — ONE skip, so the
            // literal after a comparison is not swallowed with it.
            let Some(end) = text[i + 1..].find('"') else { break };
            let before = text[..i].trim_end();
            let before = before.strip_suffix("Some(").unwrap_or(before).trim_end();
            if before.ends_with("==") {
                out.push(Lit {
                    text: text[i + 1..i + 1 + end].to_string(),
                    line: text[..i].matches('\n').count() + 1,
                });
            }
            i = i + 1 + end + 1;
            continue;
        }
        i += 1;
    }
    // `matches!(expr, "a" | "b")`
    let mut from = 0;
    while let Some(at) = text[from..].find("matches!(") {
        let start = from + at + "matches!(".len();
        let mut depth = 1i32;
        let mut end = start;
        for (k, c) in text[start..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + k;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &text[start..end];
        if let Some(comma) = inner.find(',') {
            let pats = &inner[comma + 1..];
            let line = text[..start].matches('\n').count() + 1;
            for w in string_literals(pats) {
                out.push(Lit { text: w, line });
            }
        }
        from = end.max(start);
    }
    out.sort_by_key(|l| l.line);
    out
}

/// **EVERY `.get("name")` IN `view.rs`** — a field or cell read off a `Value`.
pub fn scan_field_reads(text: &str) -> Vec<Lit> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(".get(\"") {
        let start = from + at + ".get(\"".len();
        if let Some(end) = text[start..].find('"') {
            out.push(Lit {
                text: text[start..start + end].to_string(),
                line: text[..start].matches('\n').count() + 1,
            });
            from = start + end;
        } else {
            break;
        }
    }
    out
}

/// **ROUTE EVERY CELL READ, EVERY COMPARED WORD AND EVERY FIELD READ AGAINST THE
/// PRODUCER'S MANIFEST.** The cell half of [`check_inventory`], and the same
/// shape of check: what the copy's TEXT says, against what the producer states.
///
/// `file` is the name findings quote (`view.rs`).
pub fn check_cells(
    inv: &Inventory,
    file: &str,
    reads: &[CellRead],
    consts: &[(String, String)],
    words: &[Lit],
    fields: &[Lit],
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut unstated: Vec<String> = Vec::new();
    for r in reads {
        let Some((_, shape)) = consts.iter().find(|(n, _)| *n == r.shape_const) else {
            out.push(Finding::ShapeConstUnknown {
                file: file.to_string(),
                line: r.line,
                name: r.shape_const.clone(),
            });
            continue;
        };
        let Some(s) = inv.shapes.iter().find(|s| &s.name == shape) else {
            // The shape itself is `check_inventory`'s finding.
            continue;
        };
        let Some(declares) = s.declares(r.shared) else {
            if !unstated.contains(shape) {
                unstated.push(shape.clone());
                out.push(Finding::ManifestUnstated {
                    shape: shape.clone(),
                });
            }
            continue;
        };
        if !declares.contains(&r.cell) {
            out.push(Finding::CellNotDeclared {
                file: file.to_string(),
                line: r.line,
                shape: shape.clone(),
                cell: r.cell.clone(),
                shared: r.shared,
                declares,
            });
        }
    }
    // The vocabularies: every enum leaf on every cell and field of every shape.
    let mut vocab: Vec<(String, Vec<String>)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for s in &inv.shapes {
        for (level, list) in [("", &s.cells), ("shared.", &s.shared_cells)] {
            let Some(list) = list else { continue };
            for c in list {
                names.push(c.name.clone());
                if !c.words.is_empty() {
                    vocab.push((format!("{}.{level}{}", s.name, c.name), c.words.clone()));
                }
                for f in &c.fields {
                    names.push(f.name.clone());
                    if !f.words.is_empty() {
                        vocab.push((format!("{}.{level}{}.{}", s.name, c.name, f.name), f.words.clone()));
                    }
                }
            }
        }
    }
    if vocab.is_empty() && names.is_empty() {
        // No manifest anywhere: every shape a read named was reported above;
        // words and fields cannot be judged and are not silently passed.
        return out;
    }
    for w in words {
        if !vocab.iter().any(|(_, v)| v.contains(&w.text)) {
            out.push(Finding::WordNotDeclared {
                file: file.to_string(),
                line: w.line,
                word: w.text.clone(),
                vocabularies: vocab.clone(),
            });
        }
    }
    for f in fields {
        if !names.contains(&f.text) {
            out.push(Finding::FieldNotDeclared {
                file: file.to_string(),
                line: f.line,
                name: f.text.clone(),
            });
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
//  tiny_json — just enough to read inventory.json, and no dependency
// ─────────────────────────────────────────────────────────────────────────────

/// A JSON value, as far as this guard needs one. Hand-rolled because this engine
/// is `include!`-ed by each face's own test and must not oblige a face to add a
/// dev-dependency to compile its own guard. It is a complete reader of the JSON
/// `make-inventory.py` writes (objects, arrays, strings with escapes, numbers,
/// the three literals), and it refuses anything else rather than reading past
/// it: the first version of this reader sliced the text by `{` and `}` and could
/// not have read a manifest nested inside a shape.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    fn strings(&self) -> Vec<String> {
        self.as_arr()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }
}

/// Parse one JSON document. `Err` names the byte offset and what was expected.
pub fn parse_json(text: &str) -> Result<Json, String> {
    let b = text.as_bytes();
    let mut i = 0;
    let v = json_value(b, &mut i)?;
    json_ws(b, &mut i);
    if i != b.len() {
        return Err(format!("trailing characters at byte {i}"));
    }
    Ok(v)
}

fn json_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\n' | b'\r' | b'\t') {
        *i += 1;
    }
}

fn json_value(b: &[u8], i: &mut usize) -> Result<Json, String> {
    json_ws(b, i);
    let Some(&c) = b.get(*i) else {
        return Err(format!("unexpected end of input at byte {i}"));
    };
    match c {
        b'{' => {
            *i += 1;
            let mut kv = Vec::new();
            json_ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Ok(Json::Obj(kv));
            }
            loop {
                json_ws(b, i);
                let Json::Str(k) = json_value(b, i)? else {
                    return Err(format!("expected a string key at byte {i}"));
                };
                json_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(format!("expected `:` at byte {i}"));
                }
                *i += 1;
                let v = json_value(b, i)?;
                kv.push((k, v));
                json_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => {
                        *i += 1;
                        return Ok(Json::Obj(kv));
                    }
                    _ => return Err(format!("expected `,` or `}}` at byte {i}")),
                }
            }
        }
        b'[' => {
            *i += 1;
            let mut items = Vec::new();
            json_ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Ok(Json::Arr(items));
            }
            loop {
                items.push(json_value(b, i)?);
                json_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b']') => {
                        *i += 1;
                        return Ok(Json::Arr(items));
                    }
                    _ => return Err(format!("expected `,` or `]` at byte {i}")),
                }
            }
        }
        b'"' => {
            *i += 1;
            let mut out = String::new();
            loop {
                let Some(&c) = b.get(*i) else {
                    return Err("unterminated string".into());
                };
                *i += 1;
                match c {
                    b'"' => return Ok(Json::Str(out)),
                    b'\\' => {
                        let Some(&e) = b.get(*i) else {
                            return Err("unterminated escape".into());
                        };
                        *i += 1;
                        match e {
                            b'"' => out.push('"'),
                            b'\\' => out.push('\\'),
                            b'/' => out.push('/'),
                            b'b' => out.push('\u{8}'),
                            b'f' => out.push('\u{c}'),
                            b'n' => out.push('\n'),
                            b'r' => out.push('\r'),
                            b't' => out.push('\t'),
                            b'u' => {
                                let hex = std::str::from_utf8(b.get(*i..*i + 4).ok_or("short \\u escape")?)
                                    .map_err(|e| e.to_string())?;
                                let cp = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                                *i += 4;
                                // A surrogate pair is two escapes; join them.
                                let ch = if (0xD800..0xDC00).contains(&cp) {
                                    let lo = b
                                        .get(*i..*i + 6)
                                        .and_then(|t| std::str::from_utf8(t).ok())
                                        .filter(|t| t.starts_with("\\u"))
                                        .and_then(|t| u32::from_str_radix(&t[2..], 16).ok())
                                        .ok_or("a lone high surrogate")?;
                                    *i += 6;
                                    0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    cp
                                };
                                out.push(char::from_u32(ch).ok_or("an invalid code point")?);
                            }
                            other => return Err(format!("unknown escape `\\{}`", other as char)),
                        }
                    }
                    _ => {
                        // Copy the UTF-8 sequence this byte starts, whole.
                        let len = match c {
                            0x00..=0x7F => 1,
                            0xC0..=0xDF => 2,
                            0xE0..=0xEF => 3,
                            _ => 4,
                        };
                        let s = std::str::from_utf8(&b[*i - 1..*i - 1 + len]).map_err(|e| e.to_string())?;
                        out.push_str(s);
                        *i += len - 1;
                    }
                }
            }
        }
        b't' if b[*i..].starts_with(b"true") => {
            *i += 4;
            Ok(Json::Bool(true))
        }
        b'f' if b[*i..].starts_with(b"false") => {
            *i += 5;
            Ok(Json::Bool(false))
        }
        b'n' if b[*i..].starts_with(b"null") => {
            *i += 4;
            Ok(Json::Null)
        }
        b'-' | b'0'..=b'9' => {
            let start = *i;
            while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *i += 1;
            }
            std::str::from_utf8(&b[start..*i])
                .ok()
                .and_then(|t| t.parse::<f64>().ok())
                .map(Json::Num)
                .ok_or_else(|| format!("a malformed number at byte {start}"))
        }
        other => Err(format!("unexpected `{}` at byte {i}", other as char)),
    }
}

fn inv_cells(v: Option<&Json>) -> Option<Vec<InvCell>> {
    let list = v?.as_arr()?;
    Some(
        list.iter()
            .filter_map(|c| {
                Some(InvCell {
                    name: c.get("name")?.as_str()?.to_string(),
                    words: c.get("words").map(Json::strings).unwrap_or_default(),
                    fields: c
                        .get("fields")
                        .and_then(Json::as_arr)
                        .map(|fs| {
                            fs.iter()
                                .filter_map(|f| {
                                    Some(InvField {
                                        name: f.get("name")?.as_str()?.to_string(),
                                        words: f.get("words").map(Json::strings).unwrap_or_default(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect(),
    )
}

/// Parse `inventory.json`. It reads exactly the keys the guard needs and says
/// so loudly if a required one is absent, rather than passing on an empty read.
/// `cells`/`shared_cells` are OPTIONAL per shape and their absence is kept as
/// absence — see [`InvShape::cells`].
pub fn parse_inventory(text: &str) -> Result<Inventory, String> {
    let doc = parse_json(text).map_err(|e| format!("inventory.json does not parse: {e}"))?;
    let mut inv = Inventory::default();
    inv.source_commit = doc
        .get("source_commit")
        .and_then(Json::as_str)
        .ok_or("inventory.json has no `source_commit`")?
        .to_string();
    inv.methods = doc.get("methods").ok_or("inventory.json has no `methods`")?.strings();
    let shapes = doc
        .get("shapes")
        .and_then(Json::as_arr)
        .ok_or("inventory.json has no `shapes`")?;
    for s in shapes {
        let Some(name) = s.get("name").and_then(Json::as_str) else {
            continue;
        };
        inv.shapes.push(InvShape {
            name: name.to_string(),
            version: s.get("version").and_then(Json::as_u64).unwrap_or(0),
            serves: s
                .get("serves")
                .and_then(Json::as_arr)
                .map(|a| a.iter().filter_map(Json::as_u64).collect())
                .unwrap_or_default(),
            cells: inv_cells(s.get("cells")),
            shared_cells: inv_cells(s.get("shared_cells")),
        });
    }
    if inv.methods.is_empty() {
        return Err("inventory.json's `methods` is empty".into());
    }
    if inv.shapes.is_empty() {
        return Err("inventory.json's `shapes` is empty".into());
    }
    Ok(inv)
}

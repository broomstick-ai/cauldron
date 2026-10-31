# `lib/shapewire/seam/` — the seam reference kit

`docs/consumer-seam.md` §7.1 and §R3 specify this directory: the five seam
files are **authored here first**, vendored into a consumer repo, and the second
copy is written **from** the reference — so *"the second copy's first commit is
a diff of zero by construction."*

```
mod.rs link.rs held.rs view.rs commands.rs ledger.rs   THE REFERENCE
inventory.json                     the producer's surface, as the producer states it
conformance/*.jsonl                golden transcripts + their locked answers
tests/conformance.rs               replays them through the real Held::fold
tests/fold_mutations.rs            and proves that lock bites
tools/seamdiff/                    THE REFERENCE DIFF — the drift detector
tools/make-inventory.py            writes inventory.json
tools/record-transcript.py         records a transcript off the live producer
tools/shapes-list.py               one read-only `shapes/list`, no dependencies
tools/vendor-chassis.sh            the ONE way any of it moves into a consumer repo
probe/                             the re-subscribe identity proof, against production
```

**The reference is data for a test, never a `use`.** Nothing links it. It
compiles here, as its own detached crate, because an unverified reference hands
its defects to every copy at once.

## Can a drifted copy be detected?

Yes, and here is the mechanism and its reds.

```
cargo build --manifest-path tools/seamdiff/Cargo.toml
seamdiff --copy <face>/src/seam [--inventory inventory.json] [--strays]
```

Five properties, each an independently reddenable finding:

| property | what it refuses |
|---|---|
| outside-fence identity | one character of difference outside a fence |
| sanctioned ids | a fence id not in `D1…D6` (`docs/consumer-seam.md` §5's table) |
| fence sequence | a fence the reference declares and the copy dropped, **and** a fence the copy invented |
| the budget | more than **60 CODE lines** inside fences, across the whole copy |
| no stray fences | a `SEAM-DIVERGE` marker in a seventh file, where nothing diffs |

### The headline, proved

A second copy was written from the reference mechanically — copy the six files,
substitute the two fence bodies, change nothing else — and:

```
$ seamdiff --copy <second copy>
seamdiff: ok
  the copy is the reference outside its fences                      exit 0
```

Then one line of drift in `view.rs`, the file the design says carries **zero**
divergences:

```
$ seamdiff --copy <drifted>
seamdiff: 1 finding(s)
  view.rs: the text outside the fences differs.
  reference:246: pub fn running_count(h: &Held) -> Fact<usize> {
  copy:246: pub fn running_count(h: &Held) -> Fact<usize> { // the terminal face wanted a comment
                                                                     exit 1
```

And the **same** drift, moved inside the `D2` fence, is silent — because that is
the one difference §5's table exists to permit:

```
$ seamdiff --copy <drifted inside the fence>
seamdiff: ok                                                        exit 0
```

### Move the reference and both copies go red

`docs/consumer-seam.md` §7.1's direction of pressure. The reference grew one
function in `view.rs`:

```
$ seamdiff --reference <moved> --copy <second copy>       exit 1
$ seamdiff --reference <moved> --copy apps/aui/src/seam   exit 1
$ seamdiff --reference <moved> --copy <copy that followed> exit 0
```

### The budget bites

61 code lines inside a legal `D2` fence, everything else byte-identical:

```
seamdiff: 1 finding(s)
  the divergence budget is 60 code lines and this copy spends 75:
  link.rs  D2  61
  held.rs  D3  14
  (code lines only — markers, blanks and comments are free)
```

**The budget counts code, not comments**, for a reason: a divergence has to be allowed to explain itself — `D1`'s explanation
cites where it was decided — and charging the explanation against the code
budget creates pressure to delete the explanation, which is the opposite of
what a fence is for.

### The forcings are opposed

`tools/seamdiff/tests/forcings.rs`, 28 tests. The pair that matters:

* a diff that **permits everything** reddens `a_diff_that_permits_everything_is_caught`;
* a diff that **permits nothing** reddens `a_diff_that_permits_nothing_is_caught`.

So the tool cannot pass by being laxer and cannot pass by being stricter.

### The guard does not witness itself

Two self-witnessing defects were found by this file's own tests on their first
run, and both are closed:

1. The reference's `mod.rs` documents the fence syntax in prose, and the first
   detector — *the literal anywhere on the line* — matched its own
   documentation. Fixed **structurally**: a marker is a `//` comment ALONE on
   its line, so a doc comment (`//!`, `///`) can name the mechanism and the
   parser does not see it. No exclusion list to keep up to date.
2. `SELF_EXCLUDED` carried four paths, one of which (`src/main.rs`) contains no
   marker at all. It now carries **one**: `tools/seamdiff/tests/forcings.rs`,
   which embeds whole fenced fixtures as string literals.

And the exclusion is proved non-vacuous rather than asserted:
`the_stray_fence_exclusion_is_not_vacuous` requires every excluded path to
exist, to contain a marker **under the strict detector**, and — the
counter-factual — requires an unexcluded walk of `tools/` to find exactly the
excluded set and nothing else. An exclusion that protects nothing fails.

## The transcripts

`tests/conformance.rs` replays every `conformance/*.jsonl` through the real
`Held::fold` and compares each `view::*` answer byte for byte.

**Two kinds, and each says which it is.** `"source": "live producer"` —
recorded off the running daemon by `tools/record-transcript.py`, which sends
exactly three method names and no command verb. `"source": "hand-written"` —
the edge cases a read-only recorder cannot ask for: a key vanishing, a parent
re-init under a child that has not, a death export, an `event.kind` from a newer
producer, a cell-state word from the future, and a promise re-offered across a
reconnect. Every hand-written frame names the producer path it is modelled on.

Answers are rendered with `{:?}`, which is not laziness: `Fact<T>` is
three-valued and the locked string has to say WHICH of the three it was, or a
copy that turned a `NotHydrated` into a `Known(0)` would agree.

**The expectations are blessed** (`BLESS=1 cargo test`), so a transcript is a
regression lock rather than an independent oracle — the independent oracle is
one layer down, `tests/mirror_oracle.rs`, differing the Rust `Mirror` against
the Python `ShapeMirror`. What the seam needs on top of that is a statement that
two COPIES answer the same thing.

### The lock bites

`tests/fold_mutations.rs` replays each transcript with the frames transformed by
one deliberate defect — all of them defects a real consumer committed, each
named in `docs/consumer-seam.md` §8 — and requires a disagreement at the
position the answer is locked at:

| mutation | the defect it models |
|---|---|
| an export that merges instead of replacing | the conjunction `resubscribe_identity.rs` was written for |
| a transition folded as a row | **W6** — `shapes.py` appending transitions into `row[cell]` |
| a `gone` dropped | *removed* collapsed into *not loaded yet* |
| a frame lost on an unknown word | what `from_value::<CellState>` does unaided |
| a frame's `promises` ignored | rule 2.4 — the fact arrives on the frame, not the reply |
| `parent_epoch` stripped | `ParentMovedOn` collapsed into *looks fine* |
| the log window reversed | rule 2.2 — the order IS the meaning |

Opposed by `the_identity_transform_disagrees_with_nothing`, so a harness that
always disagrees fails.

**And the real `Held::fold` was mutated too**, by hand, twice:

```
# `if !receipt.subscribes.is_empty()` -> `if false && …`
4 transcript(s) disagree with the reference:
     want: Some(Complete { subscribe_id: 27 })   got: Some(Asked)
     want: Some(Complete { subscribe_id: 1 })    got: Some(Asked)
     … 9 expectations across 4 transcripts

# `self.promises.settle(*id)` -> `let _ = id;`
2 transcript(s) disagree with the reference:
  edges.jsonl:11: settled(7)              want: Visible  got: Pending
  promise_reconnect.jsonl:16: settled(7)  want: Visible  got: Pending
```

A mutation held permanently behind a feature flag would be a second `fold`
living in the reference, which is the one thing the reference may not have.

## The inventory guard

`seamdiff --inventory inventory.json` checks the copy's own text — its
`commands::methods::TEMPLATES`, its `held.rs` shape consts, its
`SHAPE_CONTRACT_VERSIONS` table (one contract version per shape) and its `SHAPE_VERSION` — against the producer's
surface. Forced red, all four findings at once:

```
`seamdiff.fixture.unrouted-alpha` does not route against the producer's inventory (WS_METHODS u CHASSIS_METHODS). The daemon answers `unknown_method`.
`seamdiff.fixture.unrouted-beta` does not route against the producer's inventory …
the book names shape `threads/projects`, which the producer does not serve
`threads` serves contract versions [1] and the seam declares 2; rule 2.6 refuses that link
SHAPE_VERSION says `shapewire@deadbee` and inventory.json was taken at `6445362…`
```

**Those first two used to be `thread.replace` and `thread.unpend`, and this
guard is what found them.** They were spelled in the AUI, the daemon answered
`unknown method` to both, and nothing was checking. They are now DELETED from
every face, with the `pending` status word their two Options rows were gated on
— a word no producer on this wire has ever sent — so they are no longer
available as fixtures, and quoting them here would re-seed names the deletion
removed.

The substitutes are unroutable BY CONSTRUCTION rather than merely absent today:
every real name on this wire is a slash path under a resource family
(`threads/…`, `accounts/…`, `shapes/…`, `daemon/…`), and `seamdiff.fixture` is
not a resource any producer could grow. Two of them, not one, because one
unrouted method cannot tell "the check fires" from "the check fires once and
stops".

## The cell guard — `shapewire::cell`, routed the way methods are

`seamdiff --inventory` also routes every CELL `view.rs` reads, every WORD it
compares a value against and every FIELD it `.get`s against the producer's cell
manifest (`Shape::CELLS` / `Shape::SHARED`, published on `shapes.list` and
carried in `inventory.json` as `cells` / `shared_cells`). This is the guard that
would have caught `pending`: a cell no producer declared, read for months as
"not yet". Forced red on a copy of the reference with one read of `pending`
added:

```
view.rs:619: reads per-key cell `pending` of `threads`, which the producer does not declare. A cell the
producer does not declare is a cell no export will ever carry: the mirror answers `NotHydrated` for the
life of the process, which reads as "not yet" and means "never". `threads` declares: header, settings,
account, status, status_transitions, queued, last_message_id, seen_seq, unread, newest, body, compaction,
liveness, app. WHAT IS MISSING: either the cell on the producer (`Shape::CELLS`, then re-run
tools/make-inventory.py) or this read.

view.rs:622: compares against the word `pending`, which no closed vocabulary the producer declares
contains. A predicate over a word no producer emits is false for ever and reads, to anyone above it, like
an answer. The producer's vocabularies:
    threads.status.state = [active, parked, retrying, queued, idle]
    …
  WHAT IS MISSING: either the word on the producer (an `Enum` in its `Shape::CELLS`, then re-run
  tools/make-inventory.py) or this comparison.
```

Three call shapes are scanned and `view.rs` keeps every read in one of them —
`cell(h, SHAPE, key, "name", …)`, `items(h, SHAPE, key, "name")`,
`shared_cell(h, SHAPE, "name", …)` — with the shape an ident from `held.rs` and
the cell a literal; a read spelled any other way is one the guard does not see,
and the reference's own doc on `cell` says so. An inventory taken before the
producer declared its cells is its own finding (`ManifestUnstated`), never a
pass. The forcings are in `tools/seamdiff/tests/forcings.rs` under "The cell
guard", each with a positive control; the one that reads the REAL reference
asserts the scanners saw at least sixteen reads and named ones before it asserts
the empty finding list.

**The inventory is taken OFFLINE now** — `make-inventory.py --declared` runs
`cargo run -p threads --bin shapes-manifest`, which reads every shape's consts
through the same `ShapeInfo::declared_by` that `Hub::register` seeds a task from.
No daemon, no guess, a reviewable diff.

**And it REFUSES A DIRTY SURFACE**, the way `vendor-chassis.sh` refuses a dirty
chassis and for the same stated reason: it stamps `source_commit = HEAD` into
both `inventory.json` and `inventory.rs`, and a stamp that names a commit the
files are not at is a stamp that lies. The scope is the inputs that DECIDE the
surface and deliberately not the repo — `lib/shapewire/src`,
`lib/threads/src/server.rs`, `lib/threads/src/shapes` — so a dirty lock
file or a dirty doc cannot block a regeneration, while a dirty shape declaration
does. `--rust-only`'s one input is the committed `inventory.json`, so that file
is its scope instead. The generator's own OUTPUTS are out of scope on purpose:
their dirt is the run's result, not an input, and counting them would make a
successful run refuse the next one.

## `probe/` — the re-subscribe identity proof, against production

Rule 2.5: *"Recovery is re-subscribe. There is no re-emit verb, no replay, no
cursor."* Every deletion the seam is for is downstream of that being true in
practice. `probe/` measures it against the live daemon over two separate
connections, read-only, and `FORCE_RED=1` is its forcing.

Measured 2026-10-07 (pid 51642, boot `5b54892d…`, seven shapes served):
a 4,000-row window over a 175,858-event tail — 2.3% of the ledger — came back
**identical on every axis** from a connection that had never seen the first
one. Truncation is stated, never silent: `complete = false` with a
`covers_since`, which is rule 2.8's honesty requirement.

## The divergence table — two fences, not eight

`docs/consumer-seam.md` §5 enumerated eight forced divergences. Six turned out
not to be divergences in the seam; `mod.rs` carries the table with each one's
disposition, and so does §5 itself. The short version:

* **D1** (who may start the daemon) — a HOOK, `link::Supervisor`. Measured at
  ~110 lines of fenced body once the `/health` probe, cold-start wait, grace
  re-probe and detached spawn are counted, against a whole-seam budget of 60.
  The ruling is unchanged and lives at the call site. The seam now links no HTTP
  client at all.
* **D2** (the face's two words) — **FENCED**, and it absorbed three other
  declared divergences: the identity literal, two env names and the correlate
  prefix are one fact said four times.
* **D3** (the standing book) — **FENCED**, narrowed to the narration channel
  list.
* **D4** (the verb set) — a verb a face never calls is unused vocabulary, not
  policy.
* **D5** (how a face reaches the seam) — `Seam::start` answers a handle; where a
  face PUTS it is above the seam.
* **D6** (the event adapter) — a four-line forwarder in the face.
* **D7** (the ledger's buffer policy) — **dissolved by `threads/log`**: the
  window is the producer's 4,000, published as `retained_items`, so there is no
  `OPEN_CAP` and no `TAIL_CAP` left to differ.
* **D8** (the project index) — never in the seam.

Reference spend: **16 code lines of 60** (`D2` 2, `D3` 14).

## `ledger.rs` — the interim was not built

The design specified `ledger.rs` as the seam's one interim component, to be
deleted at migration step M7 when a log shape landed. **`threads/log` landed and
is in production, so the interim never ships** and this file is the
`threads/log` mirror. The argument is in `ledger.rs`'s own header; the short
form is that a file whose header says *delete me* is a file somebody preserves,
that a hand-rolled tailer folds legacy frames outside the mirror and so is the
one component the golden transcripts could not reach, and that it was the only
thing keeping a cursor in the seam.

## Running the kit

```
# the reference compiles, and its goldens hold
cargo test --manifest-path lib/shapewire/seam/Cargo.toml

# the drift guard, and its 28 forcings
cargo test --manifest-path lib/shapewire/seam/tools/seamdiff/Cargo.toml

# the inventory, when the producer's surface moves (needs a live daemon)
python3 lib/shapewire/seam/tools/make-inventory.py

# a new transcript off the live producer
python3 lib/shapewire/seam/tools/record-transcript.py <name> <shape>[:<key>][@<retain>] …
BLESS=1 cargo test --manifest-path lib/shapewire/seam/Cargo.toml   # then review the diff

# into a consumer repo
lib/shapewire/seam/tools/vendor-chassis.sh --dest <repo>/libs/shapewire
```

## What is still owed

* **The two copies are not yet diffing clean.** The reference is in this repo;
  each face's `tests/seam_reference.rs` and `tests/seam_conformance.rs` live in
  the consumer repo and are not written. `vendor-chassis.sh` prints what they
  must do.
* **The AUI's copy has a residue**, enumerated in R2.S0's report: an AUI-only
  module reached from `link.rs`, AUI-named env vars, a `D1` fence the reference
  no longer has, and prose that names one face in files both faces share.
* ~~One chassis finding is LOCKED rather than fixed: `Mirror::key_state`
  answers `Held` for a key whose keyed export carried no row.~~ **FIXED
  2026-10-08**: `KeyState::Absent` / `Holding::Absent`. `absent_key.jsonl` went
  red on `holding`, `key_state` and `mirror_keys` and was re-blessed; that
  diff is the record.
* **The faces diverge from this reference until R2.S9 (a re-vendor).** Both
  faces vendor the chassis at `9235e8e`. Since then the reference has gained
  `threads/non-idle` in its book and `view::running` reads it (R2.S8); the
  contract version is stated PER SHAPE (`mod.rs`'s `SHAPE_CONTRACT_VERSIONS`,
  `accounts` = 2, sent by `link.rs` per subscribe, checked per shape by
  `seamdiff --inventory`); and the inventory carries `threads::queue` and
  `threads_non_idle`. A face's `view.rs` copied from here does not compile
  against its vendored `inventory.rs` (`E0432 threads_non_idle`, `E0433
  threads::queue`), and a face still sending one `version: 1` for every shape
  is refused `accounts` by a producer at `accounts` v2. The TUI also reads
  `row.get("threads")` off `account_row` (`apps/tui/src/mirror.rs:856`), a
  literal no compiler sees; R2.S9 must move it to `view::running`.

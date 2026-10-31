#!/usr/bin/env bash
# `prove-surface-gate.sh` — **A GUARD THAT HAS NEVER BEEN SEEN RED IS A
# DECORATION.**
#
# The sibling of `prove-cell-gate.sh`, for the other half of the same idea.
# `declare_cells!` made a PRODUCER's manifest a compile-time obligation where it is
# written; `shapewire::surface` plus the generated `seam/inventory.rs` make the
# same surface a compile-time obligation in the CONSUMER. Nothing in a test suite
# can witness a compile failure, so this script forces each gate red and asserts
# that rustc refused AND said the sentence the gate owes.
#
#   ./prove-surface-gate.sh        # exit 0 iff every red is red and every green is green
#
# ## Two probes, and the first one is the real kit
#
# **PART A mutates a COPY OF THE REFERENCE SEAM** — all seven files, its own
# `Cargo.toml` and `Cargo.lock`, staged into a temp dir with the `shapewire` path
# dep rewritten absolute. Nothing in the repository is touched. That matters more
# than convenience: these reds are the messages a FACE will actually see, from the
# actual `Link::request` signature and the actual `view::cell` reads, rather than
# from a probe that reproduces them and could drift.
#
# **PART B is a small probe crate** for the two `Command` gates, which need the
# `command` feature and no socket at all.
#
# Serially and alone, like `check-features.sh`: a concurrent cargo on the same
# target dir makes this report false failures.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
seam="$here/seam"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail=0

# ─────────────────────────────────────────────────────────────────────────────
#  PART A — the real kit, mutated in a copy
# ─────────────────────────────────────────────────────────────────────────────

stage() {
  rm -rf "$tmp/seam"
  mkdir -p "$tmp/seam"
  cp "$seam"/*.rs "$tmp/seam/"
  cp "$seam"/Cargo.toml "$tmp/seam/"
  cp "$seam"/Cargo.lock "$tmp/seam/" 2>/dev/null || true
  cp "$seam"/inventory.json "$tmp/seam/"
  # The reference is a crate whose only path dep is the chassis one directory up.
  # In the copy that directory is the temp dir, so the dep is rewritten absolute
  # and the chassis under test is THIS checkout.
  python3 - "$tmp/seam/Cargo.toml" "$here" <<'PY'
import sys
p, crate = sys.argv[1], sys.argv[2]
s = open(p).read()
new = s.replace('path = ".."', f'path = "{crate}"')
if new == s:
    sys.exit("prove-surface-gate.sh: could not rewrite the chassis path dep")
open(p, 'w').write(new)
PY
  # `tests/` is deliberately NOT staged: this script proves COMPILE failures of the
  # library, and a test that scans the library's text would redden on a mutation
  # for its own reasons and muddy the evidence.
}

# `mutate <file> <python expression over `s`>` — one textual mutation in the copy.
mutate() {
  python3 - "$tmp/seam/$1" "$2" "$3" <<'PY'
import sys
p, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p).read()
if s.count(old) != 1:
    sys.exit(f"prove-surface-gate.sh: the mutation target appears {s.count(old)} "
             f"times in {p}, so the forcing would not be the one described:\n  {old!r}")
open(p, 'w').write(s.replace(old, new))
PY
}

# `expect_red_in_seam <label> <file> <old> <new> <substring>...`
expect_red_in_seam() {
  local label="$1" file="$2" old="$3" new="$4"; shift 4
  stage
  if ! mutate "$file" "$old" "$new"; then
    printf "  BROKEN  %s: the mutation could not be applied\n" "$label"; fail=1; return
  fi
  local out
  out="$(cd "$tmp/seam" && cargo check -q --lib 2>&1)"
  if [ $? -eq 0 ]; then
    printf "  BROKEN  %s: the build went GREEN on the mutated kit\n" "$label"; fail=1; return
  fi
  local want
  for want in "$@"; do
    if ! printf '%s' "$out" | grep -qF -- "$want"; then
      printf "  BROKEN  %s: rustc refused but did not say %s\n%s\n" "$label" "$want" "$out"
      fail=1; return
    fi
  done
  printf "  ok      red: %s\n" "$label"
  printf '%s\n' "$out" | grep -E "^error\[E[0-9]+\]" | head -3 | sed 's/^/            /'
}

echo "PART A — the reference kit, mutated in a copy"

# THE POSITIVE CONTROL FIRST. Every red below is meaningful only because the
# unmutated kit compiles against the unmutated inventory.
stage
if (cd "$tmp/seam" && cargo check -q --lib 2>/dev/null); then
  echo "  ok      green: the reference kit compiles against the generated inventory (the positive control)"
else
  echo "  BROKEN  the unmutated kit does not compile — every red below is meaningless"
  (cd "$tmp/seam" && cargo check --lib 2>&1 | tail -20)
  fail=1
fi

# ── 1. A NAME WITH NO DOOR ──────────────────────────────────────────────────
expect_red_in_seam "a method the producer does not serve has no const to name" \
  commands.rs \
  'pub const CANCEL: Method<1> = served::THREADS_CANCEL;' \
  'pub const CANCEL: Method<1> = served::THREADS_UNPEND;' \
  "error[E0425]" \
  "cannot find value \`THREADS_UNPEND\` in module \`served\`"

# ── 2. AND IT CANNOT BE SMUGGLED PAST AS A STRING ───────────────────────────
expect_red_in_seam "a method name as a string literal is refused, by name" \
  commands.rs \
  'let payload = s.link.request(methods::cancel(thread), json!({})).await?;' \
  'let payload = s.link.request("threads/{thread}/unpend", json!({})).await?;' \
  "error[E0277]" \
  "a method name cannot be written here; it is minted from the producer's surface" \
  "WHAT IS MISSING: the method's const" \
  "there is no raw door"

# ── 3. THE ARITY OF A TOKEN IS CHECKED AGAINST ITS TEMPLATE ─────────────────
expect_red_in_seam "a method token whose arity is not its template's" \
  inventory.rs \
  'pub const THREADS_SEND: Method<1> = Method::__served("threads/{thread_id}/send");' \
  'pub const THREADS_SEND: Method<0> = Method::__served("threads/{thread_id}/send");' \
  "error[E0080]" \
  "this method token's arity is not its template's"

# ── 4. THE TWO SPELLINGS OF THE METHOD SET STAY ONE FACT ────────────────────
expect_red_in_seam "a method token whose template is not in TEMPLATES" \
  commands.rs \
  '        "threads/{thread_id}/cancel",' \
  '' \
  "error[E0080]" \
  "the \`cancel\` method token's template is not in TEMPLATES" \
  "WHAT IS MISSING: this template in TEMPLATES"

expect_red_in_seam "a TEMPLATES entry with no method token beside it" \
  commands.rs \
  '        "threads/{thread_id}/cancel",' \
  '        "threads/{thread_id}/cancel",
        "threads/{thread_id}/unpend",' \
  "error[E0080]" \
  "TEMPLATES has an entry with no method token beside it"

# ── 5. A DOOR WITH NO CARGO — THE CELL LAYER, WHICH IS `pending` ────────────
expect_red_in_seam "a cell the producer does not declare has no const to name" \
  view.rs \
  'cell(h, inventory::threads::STATUS, id, |v| {' \
  'cell(h, inventory::threads::PENDING, id, |v| {' \
  "error[E0425]" \
  "cannot find value \`PENDING\` in module \`inventory::threads\`"

expect_red_in_seam "a cell name as a string literal does not type-check (E0308: the kit-internal case)" \
  view.rs \
  'cell(h, inventory::threads::STATUS, id, |v| {' \
  'cell(h, "pending", id, |v| {' \
  "error[E0308]" \
  "expected \`Carried\`, found \`&str\`"

# ── 6. A DOOR WITH NO CARGO — THE FIELD AND WORD LAYERS ────────────────────
expect_red_in_seam "a field no cell on that shape declares" \
  view.rs \
  'v.get(inventory::threads::status::EXECUTING)' \
  'v.get(inventory::threads::status::PENDING)' \
  "error[E0425]" \
  "cannot find value \`PENDING\` in module \`inventory::threads::status\`"

# A word is compared in a PATTERN, so rustc answers E0531 rather than E0425 — and
# the message is the better of the two for this purpose: it says *constant*, and it
# names the module, which is why the path is spelled in full at the comparison
# rather than behind an alias.
expect_red_in_seam "a word the producer's closed vocabulary does not contain" \
  view.rs \
  'inventory::threads::status::state::RETRYING' \
  'inventory::threads::status::state::PENDING' \
  "error[E0531]" \
  "cannot find unit struct, unit variant or constant \`PENDING\` in module \`inventory::threads::status::state\`"

# ─────────────────────────────────────────────────────────────────────────────
#  PART B — the two `Command` gates
# ─────────────────────────────────────────────────────────────────────────────

echo "PART B — an ack with no event"

probe="$tmp/cmd"
write_cmd_probe() {
  mkdir -p "$probe/src"
  cat > "$probe/Cargo.toml" <<TOML
[package]
name = "surface-gate-probe"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
shapewire = { path = "$here", default-features = false, features = ["command"] }
TOML
  cat > "$probe/src/lib.rs" <<RS
use shapewire::command::{Command, NeverVisible, Resolves};

/// A fact SOME shape claims to emit — the legal case.
pub struct AFactSomethingEmits;
pub struct AShape;
impl Resolves<AFactSomethingEmits> for AShape {}

/// A fact NOTHING claims to emit, which is the hole.
pub struct AFactNothingEmits;

$1
RS
}

expect_red_cmd() {
  local label="$1"; shift
  local body="$1"; shift
  write_cmd_probe "$body"
  local out
  out="$(cd "$probe" && cargo check -q 2>&1)"
  if [ $? -eq 0 ]; then
    printf "  BROKEN  %s: the build went GREEN\n" "$label"; fail=1; return
  fi
  local want
  for want in "$@"; do
    if ! printf '%s' "$out" | grep -qF -- "$want"; then
      printf "  BROKEN  %s: rustc refused but did not say %s\n%s\n" "$label" "$want" "$out"
      fail=1; return
    fi
  done
  printf "  ok      red: %s\n" "$label"
  printf '%s\n' "$out" | grep -E "^error\[E[0-9]+\]" | head -3 | sed 's/^/            /'
}

# THE POSITIVE CONTROL FIRST, and it carries BOTH legal shapes — a command a shape
# resolves, and a command that declares it resolves to nothing. A gate that
# refused the second would make `NeverVisible` a trap rather than an alternative.
write_cmd_probe 'pub struct Ordinary;
impl Command for Ordinary {
    const NAME: &str = "a/b";
    type Resolves = AFactSomethingEmits;
    type ResolvedBy = AShape;
}
pub struct ResolvesToNothing;
impl Command for ResolvesToNothing {
    const NAME: &str = "a/c";
    type Resolves = NeverVisible;
    type ResolvedBy = NeverVisible;
}'
if (cd "$probe" && cargo check -q 2>/dev/null); then
  echo "  ok      green: a command a shape resolves AND one that declares NeverVisible both compile (the positive control)"
else
  echo "  BROKEN  the well-formed commands do not compile — every red below is meaningless"
  (cd "$probe" && cargo check 2>&1 | tail -20)
  fail=1
fi

expect_red_cmd "a command that declares no ResolvedBy at all" \
  'pub struct Undeclared;
impl Command for Undeclared {
    const NAME: &str = "a/b";
    type Resolves = AFactSomethingEmits;
}' \
  "error[E0046]" \
  "not all trait items implemented, missing: \`ResolvedBy\`"

expect_red_cmd "a command whose fact no shape on this producer resolves" \
  'pub struct Unresolvable;
impl Command for Unresolvable {
    const NAME: &str = "a/b";
    type Resolves = AFactNothingEmits;
    type ResolvedBy = AShape;
}' \
  "error[E0277]" \
  "no shape on this producer resolves this command's fact, so its promise can never become visible" \
  "WHAT IS MISSING: either \`impl shapewire::command::Resolves<TheFact> for TheShape\`" \
  "type Resolves = shapewire::command::NeverVisible;"

# And the OTHER direction of the same gate: naming `NeverVisible` as the resolving
# type while pointing `ResolvedBy` at an ordinary shape is also refused, so the
# declared alternative cannot be half-written.
expect_red_cmd "a command that says NeverVisible but points ResolvedBy elsewhere" \
  'pub struct HalfWritten;
impl Command for HalfWritten {
    const NAME: &str = "a/b";
    type Resolves = NeverVisible;
    type ResolvedBy = AShape;
}' \
  "error[E0277]" \
  "no shape on this producer resolves this command's fact"

exit $fail

#!/usr/bin/env bash
# `prove-cell-gate.sh` — **A GUARD THAT HAS NEVER BEEN SEEN RED IS A DECORATION.**
#
# `shapewire::declare_cells!` refuses a malformed cell manifest at COMPILE time,
# with a message that names the cell (and the field) and says what is wrong.
# Nothing in the test suite can witness a compile failure, so this script does
# what `check-features.sh` does for the mirror floor: it builds a throwaway probe
# crate against this checkout of `shapewire`, once with a well-formed manifest
# (the POSITIVE CONTROL — a gate that refuses everything is not a gate) and once
# per rule with a manifest that breaks exactly that rule, and asserts that rustc
# refused AND said the sentence the rule owes.
#
#   ./prove-cell-gate.sh            # exit 0 iff every red is red and the green is green
#
# Serially and alone, like `check-features.sh`: a concurrent cargo on the same
# target dir makes this report false failures.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
probe="$(mktemp -d)"
trap 'rm -rf "$probe"' EXIT
fail=0

# A probe crate that declares one manifest, from a body this script supplies.
write_probe() {
  mkdir -p "$probe/src"
  cat > "$probe/Cargo.toml" <<TOML
[package]
name = "cell-gate-probe"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
shapewire = { path = "$here", default-features = false }
TOML
  cat > "$probe/src/lib.rs" <<RS
use shapewire::operation::{
    DType::{Enum, Scalar as Sc, Struct, Opaque},
    Presence::{Ceiling, Optional, Required},
    Scalar::{Bool, Int, Str},
};
#[allow(unused_imports)]
use shapewire::cell::Cells;
shapewire::declare_cells! {
    pub PROBE = [
$1
    ]
}
RS
}

# `expect_red <label> <manifest body> <substring>...`
expect_red() {
  local label="$1"; shift
  local body="$1"; shift
  write_probe "$body"
  local out
  out="$(cd "$probe" && cargo check -q 2>&1)"
  if [ $? -eq 0 ]; then
    printf "  BROKEN  %s: the build went GREEN on a malformed manifest\n" "$label"; fail=1; return
  fi
  for want in "$@"; do
    if ! printf '%s' "$out" | grep -qF -- "$want"; then
      printf "  BROKEN  %s: rustc refused but did not say %s\n%s\n" "$label" "$want" "$out"; fail=1; return
    fi
  done
  printf "  ok      red: %s\n" "$label"
  printf '%s\n' "$out" | grep -E "^error\[E0080\]" | head -1 | sed 's/^/            /'
}

# THE POSITIVE CONTROL FIRST. Every red below is meaningful only because this is green.
write_probe '        ("status", Struct("StateLevel"), Required, [
            ("state", Enum(&["active", "idle"]), Required),
            ("executing", Sc(Bool), Required),
            ("detail", Sc(Str), Optional),
        ]),
        ("queued", Sc(Int), Required, []),
        ("app", Opaque, Required, []),'
if (cd "$probe" && cargo check -q 2>/dev/null); then
  echo "  ok      green: a well-formed manifest compiles (the positive control)"
else
  echo "  BROKEN  the well-formed manifest does not compile — every red below is meaningless"; fail=1
fi

expect_red "two cells of one name" \
  '        ("status", Sc(Str), Required, []),
        ("status", Sc(Int), Required, []),' \
  "error[E0080]" \
  "cell \`status\` is declared more than once in this manifest. One name, one cell: a row has one slot per name and JSON keeps the last, so the second declaration describes nothing a consumer could read"

expect_red "an empty enum" \
  '        ("state", Enum(&[]), Required, []),' \
  "cell \`state\`: its dtype has an empty subtype or an empty enum — a declaration that says nothing while looking like it says something. Name the struct, ref, delta or cursor; list the enum's words"

expect_red "a ceiling on an emitted cell" \
  '        ("queued", Sc(Int), Ceiling, []),' \
  "cell \`queued\` is declared \`Ceiling\`. A ceiling belongs to a parameter a SUBSCRIBER sets (rule 2.8); a cell is EMITTED, so it is \`Required\` (never null once known) or \`Optional\` (may be null or absent)"

expect_red "a struct with no fields" \
  '        ("status", Struct("StateLevel"), Required, []),' \
  "cell \`status\`: a \`Struct\` leaf lists its fields and any other leaf lists none. A struct with no fields is a declaration no consumer can be checked against — list what it carries, or declare the cell \`Opaque\`, which is the positive declaration that nothing may read inside it"

expect_red "a scalar with fields" \
  '        ("queued", Sc(Int), Required, [("n", Sc(Int), Required)]),' \
  "cell \`queued\`: a \`Struct\` leaf lists its fields and any other leaf lists none"

expect_red "two fields of one name" \
  '        ("status", Struct("StateLevel"), Required, [
            ("state", Sc(Str), Required),
            ("state", Sc(Str), Required),
        ]),' \
  "cell \`status\`, field \`state\` is declared more than once; one name, one field"

expect_red "a ceiling on a field" \
  '        ("status", Struct("StateLevel"), Required, [("state", Sc(Str), Ceiling)]),' \
  "cell \`status\`, field \`state\` is declared \`Ceiling\`; an emitted field is \`Required\` or \`Optional\`"

expect_red "a field with an empty struct subtype" \
  '        ("status", Struct("StateLevel"), Required, [("inner", Struct(""), Required)]),' \
  "cell \`status\`, field \`inner\`: its dtype has an empty subtype or an empty enum"

expect_red "a nameless cell" \
  '        ("", Sc(Int), Required, []),' \
  "a cell with no name is declared in this manifest; every cell is read by its name, so a nameless one can never be read"

# AND THE MISSING DECLARATION ITSELF: a shape with no `CELLS` does not compile,
# and rustc names what is missing. This is the check `Shape::CELLS` being a
# required const buys — no default, so no silent empty manifest.
mkdir -p "$probe/src"
cat > "$probe/src/lib.rs" <<'RS'
use shapewire::fold::{Outbox, Root};
use shapewire::hub::{Kind, Shape, Tracking, Tx};
use shapewire::wire::{Stamped, Timestamp};
use serde_json::Value;
#[derive(Clone)] struct I;
impl Kind for I { fn kind(&self) -> &'static str { "i" } }
#[derive(Default)] struct R;
impl Root for R {
    fn keys(&self) -> Vec<String> { vec![] }
    fn row(&self, _: &str) -> Option<Value> { None }
    fn cells(&self, _: &str) -> Option<Value> { None }
    fn shared(&self) -> Value { Value::Null }
    fn shared_cells(&self) -> Value { Value::Null }
    fn drain(&mut self) -> Outbox { Outbox::default() }
    fn mark_dead(&mut self, _: &str) {}
}
struct Undeclared;
impl Shape<I> for Undeclared {
    const NAME: &'static str = "undeclared";
    const TRACKING: Tracking = Tracking::Lazy;
    type Producer = ();
    type Root = R;
    fn root(&self) -> R { R }
    fn hydrates(&mut self, _: &(), _: &mut R, _: Timestamp) {}
    fn folds(&mut self, _: &Stamped<I>, _: &mut Tx<'_, R>) {}
}
RS
cat > "$probe/Cargo.toml" <<TOML
[package]
name = "cell-gate-probe"
version = "0.0.0"
edition = "2021"
[workspace]
[dependencies]
shapewire = { path = "$here", features = ["hub"] }
serde_json = "1"
TOML
out="$(cd "$probe" && cargo check -q 2>&1)"
if [ $? -eq 0 ]; then
  echo "  BROKEN  a shape that declares no CELLS compiled"; fail=1
elif printf '%s' "$out" | grep -q 'not all trait items implemented, missing: `CELLS`, `SHARED`'; then
  echo "  ok      red: a shape that declares no manifest does not compile, and rustc names \`CELLS\`, \`SHARED\`"
  printf '%s\n' "$out" | grep -E "^error\[E0046\]" | head -1 | sed 's/^/            /'
else
  echo "  BROKEN  the undeclared shape failed for another reason:"; echo "$out"; fail=1
fi

exit $fail

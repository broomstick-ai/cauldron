#!/usr/bin/env python3
"""Write `seam/inventory.json` AND `seam/inventory.rs` — the producer's surface.

**TWO FILES, ONE RUN, BY CONSTRUCTION.** `inventory.json` is what `seamdiff`
reads; `inventory.rs` is what the CONSUMER'S COMPILER reads. They are written in
the same pass from the same payload, so they cannot disagree unless one of them
is hand-edited — and `seamdiff --inventory` asserts they do not, naming the field
that moved.

**AND THE STAMP, IN THE SAME RUN (R4.S13).** Every run also rewrites
`SHAPE_VERSION` in `seam/mod.rs` and in every in-repo face's
`apps/*/src/seam/mod.rs` to the inventory's commit, and copies `inventory.rs`
into each face — so a re-take leaves `apps/tui/tests/seam_reference.rs` green
with no hand edit. See `stamp_faces`. A re-take whose SURFACE did not move keeps
the committed inventory as it is (ledger 314), so it produces no diff at all.

`inventory.rs` is the half that makes the surface a BUILD INPUT rather than a
test fixture. It carries one `shapewire::surface::Method` const per method the
producer serves and one `shapewire::surface::Carried` const per cell it declares,
so a method with no door and a cell with no cargo have no name a consumer can
write: the failure is rustc's own `E0425`, at the call site, in the consumer's
own build. That is the difference between how `thread.replace` was found (a tool
somebody ran, months late) and how the next one will be.


`docs/consumer-seam.md` §7.2: *"each app's `methods.rs` is parsed and every name
is asserted to route against `inventory.json`'s `WS_METHODS ∪ CHASSIS_METHODS`
by the same right-to-left template match `shapewire::server::route` uses; every
shape in `Book::default()` is asserted to be in `inventory.json`'s shape list"*.
This is where that file comes from.

    ./make-inventory.py --declared                          # offline, from the consts
    ./make-inventory.py [--port 3653]                       # off a running daemon
    ./make-inventory.py --shapes-from <shapes-list.json>    # from a saved payload

It is a COMMITTED ARTIFACT of this repo, like `tests/oracle/expected.json`, and
`vendor-chassis.sh` copies it verbatim rather than rebuilding it — so a vendor
run needs no daemon, and the file a consumer checks itself against is one a
reviewer saw land in a diff.

## Two sources, and neither is a guess

**The methods are SCRAPED from the source**, `shapewire::server::CHASSIS_METHODS`
and `threads::server::WS_METHODS`, both of which are pinned against their own
dispatch arms by `lib/threads/tests/chassis_api.rs`. Deterministic, offline, and
loud if either const moves.

**The shape contracts and cell manifests come off the DECLARATION, read by the
producer's own code — never scraped and never guessed.** `Shape::VERSION`,
`Shape::SERVES`, every ceiling and, since 2026-10-08, `Shape::CELLS` and
`Shape::SHARED` are per-shape consts with trait defaults, so a shape that
declares neither version const still serves a contract (`accounts` is that case)
and no regex could tell. Two honest readers exist and `--declared` is the one
to use:

* `--declared` runs `cargo run -q -p threads --bin shapes-manifest`, which
  prints every registered shape through `ShapeInfo::declared_by` — the SAME
  function `Hub::register` seeds a task's published info from. No daemon, no
  network; a reviewer regenerates and reads the diff.
* no flag asks a RUNNING daemon for `shapes/list`, which is the same values
  plus runtime state. It needs the daemon to be of the commit being
  inventoried, which a signed deploy image usually is not.

An inventory that guessed would make the guards agree with whoever wrote this
script instead of with the producer, which is worse than having no check. So
there is no fallback: if the declaration cannot be read, this exits non-zero.

## It refuses a dirty tree, the way `vendor-chassis.sh` does

`vendor-chassis.sh` will not vendor while the chassis has uncommitted changes:
*"A stamp that names a commit the files are not at is a stamp that lies, and the
whole point of the stamp is that a consumer can check out what it names."* That
applies here with more force, because this is where the stamp is WRITTEN —
`source_commit` is `git rev-parse HEAD`, copied into `inventory.json` and into
`inventory.rs`'s `SOURCE_COMMIT`, which is the half a consumer COMPILES against.
A generated seam file taken from uncommitted source is a seam file whose
provenance nobody can check out, which defeats the point of generating it.

See `dirt_scope` for what "dirty" means here, and for why it is deliberately not
the repo.
"""

import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SEAM = os.path.dirname(HERE)
CRATE = os.path.dirname(SEAM)
REPO = os.path.dirname(os.path.dirname(CRATE))


# ─────────────────────────────────────────────────────────────────────────────
# THE DIRTY-TREE REFUSAL — same guard as `vendor-chassis.sh`, scoped on purpose
# ─────────────────────────────────────────────────────────────────────────────
#
# **WHAT "DIRTY" MEANS HERE, AND WHY IT IS NOT THE REPO.**
#
# The stamp this script writes claims one thing: *this is the producer's surface
# at commit X*. So the only dirt that can make that claim false is dirt in the
# files that DECIDE the surface. Everything else in this repo — `.mind/`, the
# docs, the other crates, the tests, including this crate's own tests — can be
# as dirty as it likes without the stamp becoming a lie.
#
# That distinction is not pedantry, it is a bug this repo already shipped once.
# `lib/threads/build.rs` had a dirt check that was REPO-WIDE while its comment
# claimed otherwise, and it duly flagged the tree dirty over
# `.mind/v5-pools/interface-layer/.pen.lock` — a workspace lock file with no
# bearing on any artifact. A guard that cries wolf on a lock file is a guard
# people learn to pass `--force` to. It was fixed by scoping dirt to the closure
# that actually compiles in, and this is the same fix made the first time.
#
# The three entries below are exactly the inputs, and each one is read by name
# somewhere in this file or in `vendor-chassis.sh`:
#
#   lib/shapewire/src          `src/server.rs`'s `CHASSIS_METHODS`, scraped by
#                              `scrape()` below; and `src/hub.rs`, which
#                              `vendor-chassis.sh`'s `shape_sources` names as a
#                              thing that can move a shape's contract.
#   lib/threads/src/server.rs  `WS_METHODS`, scraped by `scrape()` below.
#   lib/threads/src/shapes     the shape DECLARATIONS — every `Shape::VERSION`,
#                              `SERVES`, ceiling, `CELLS` and `SHARED` that
#                              `shapes-manifest` reads through
#                              `ShapeInfo::declared_by`. The other half of
#                              `vendor-chassis.sh`'s `shape_sources`.
#
# **THE OUTPUTS ARE DELIBERATELY NOT IN SCOPE.** `seam/inventory.json` and
# `seam/inventory.rs` are what this script WRITES. Their being dirty is this
# script's own result, not an input to the stamp — and scoping them in would
# mean a successful run makes the next run refuse, which is a guard that fires
# on itself rather than on anything about provenance.
#
# `--rust-only` is the one case with a different input set: it reads nothing from
# the producer at all, only the committed `seam/inventory.json`, so that file is
# its scope and the producer sources are not.
DIRT_SCOPE = [
    "libs/shapewire/src",
    "services/threads/src/server.rs",
    "services/threads/src/shapes",
]

# `--rust-only`'s sole input: the committed JSON it re-emits as Rust.
DIRT_SCOPE_RUST_ONLY = ["libs/shapewire/seam/inventory.json"]


def dirt_scope(paths, reading):
    """Refuse while any of `paths` has uncommitted changes.

    `git status --porcelain -- <pathspec>` and exit 1, which is
    `vendor-chassis.sh`'s guard verbatim rather than a second dialect of it.
    Untracked files count, exactly as they do there: a shape source that is not
    in the index yet is still a surface that HEAD does not name.
    """
    out = subprocess.run(["git", "-C", REPO, "status", "--porcelain", "--"] + list(paths),
                         capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit("make-inventory.py: `git status` failed, so this script cannot tell "
                 "whether the surface it is about to stamp is the surface at HEAD. "
                 "That is a refusal, not a warning: " + (out.stderr.strip() or "?"))
    dirty = [ln for ln in out.stdout.splitlines() if ln.strip()]
    if not dirty:
        return
    print(f"make-inventory.py: {reading} has uncommitted changes.", file=sys.stderr)
    print("  A stamp that names a commit the files are not at is a stamp that lies,",
          file=sys.stderr)
    print("  and `inventory.rs` is a BUILD INPUT of every consumer — the whole point",
          file=sys.stderr)
    print("  of the stamp is that a consumer can check out what it names. Commit",
          file=sys.stderr)
    print("  first. `vendor-chassis.sh` refuses the same way, for the same reason.",
          file=sys.stderr)
    print("  In scope (and nothing else in this repo is):", file=sys.stderr)
    for p in paths:
        print(f"    {p}", file=sys.stderr)
    print("  Dirty:", file=sys.stderr)
    for ln in dirty:
        print(f"    {ln}", file=sys.stderr)
    sys.exit(1)


def scrape(path, name):
    body = open(path).read()
    m = re.search(r"pub const " + name + r": &\[&str\] = &\[(.*?)\n\];", body, re.S)
    if not m:
        sys.exit(f"make-inventory.py: {name} not found in {path}. It moved or was "
                 f"renamed; an empty method list would pass everything, so this "
                 f"is a failure rather than a default.")
    out = re.findall(r'"([^"]+)"', m.group(1))
    if not out:
        sys.exit(f"make-inventory.py: {name} in {path} is empty")
    return out


# ─────────────────────────────────────────────────────────────────────────────
# `inventory.rs` — THE SAME SURFACE, AS THE CONSUMER'S BUILD INPUT
# ─────────────────────────────────────────────────────────────────────────────
#
# Everything below turns the `doc` dict this script already builds into one
# generated Rust module. It invents nothing: every name it emits came out of
# `CHASSIS_METHODS`, `WS_METHODS` or `ShapeInfo::declared_by`, so it CANNOT emit a
# const for a method the producer does not dispatch or a cell it does not declare.
# That is what makes `Method::__served` / `Carried::__declared` safe to expose as
# hidden doors: the honesty is in the input, not in a promise this script makes.
#
# **IT REFUSES RATHER THAN MANGLES.** A name that would collide with another
# const, with a reserved item in the module, or with a Rust keyword exits
# non-zero. Rule 2.8 — declare, refuse, never clamp — applies to a code generator
# exactly as it applies to a ceiling: a generator that quietly renamed a const
# would hand the consumer a surface the producer did not state.

KEYWORDS = {
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
    "super", "trait", "true", "type", "unsafe", "use", "where", "while", "async",
    "await", "abstract", "become", "box", "do", "final", "macro", "override",
    "priv", "try", "typeof", "unsized", "virtual", "yield", "union",
}

# Items every shape module defines itself. A cell of one of these names would be
# a second definition, so it is refused rather than renamed.
RESERVED_IN_SHAPE = {"SHAPE", "CELLS", "SHARED"}


def ident(s):
    """A Rust identifier from a wire name: non-alphanumerics become `_`."""
    out = "".join(c if (c.isalnum() or c == "_") else "_" for c in s)
    if not out or out[0].isdigit():
        out = "_" + out
    return out


def mod_ident(s, what):
    i = ident(s)
    if i in KEYWORDS:
        sys.exit(f"make-inventory.py: {what} `{s}` is a Rust keyword and cannot be a "
                 f"module name. The generator refuses rather than renaming it: a "
                 f"renamed module is a name the producer did not state.")
    return i


def const_ident(s):
    return ident(s).upper()


def method_const(template):
    """`threads/{thread_id}/send` -> `THREADS_SEND`.

    The placeholder segments are DROPPED rather than spelled, because a
    placeholder's name is the producer's parameter name and not part of what the
    method is called. Uniqueness is asserted by the caller.
    """
    kept = [s for s in template.split("/") if not (s.startswith("{") and s.endswith("}"))]
    return "_".join(const_ident(s) for s in kept)


def holes(template):
    return sum(1 for s in template.split("/")
               if len(s) >= 3 and s.startswith("{") and s.endswith("}"))


def rust_list(names):
    return ", ".join('"%s"' % n for n in names)


def emit_cell_module(out, cell, indent):
    """Field-name consts and closed vocabularies for one cell, one level deep.

    Only emitted when there is something to emit. A cell with neither fields nor
    words is a scalar, a ref or an opaque, and a module with nothing in it would
    be a place a reader goes to find out nothing.
    """
    words = cell.get("words") or []
    fields = cell.get("fields") or []
    if not words and not fields:
        return
    pad = " " * indent
    name = mod_ident(cell["name"], "cell")
    out.append(f'{pad}/// Cell `{cell["name"]}` — `{cell["dtype"]}`.')
    if fields:
        out.append(f'{pad}/// Its declared fields: {", ".join(f["name"] for f in fields)}.')
    out.append(f"{pad}pub mod {name} {{")
    if words:
        out.append(f'{pad}    /// The closed vocabulary this cell\'s value is one of.')
        out.append(f'{pad}    pub const WORDS: &[&str] = &[{rust_list(words)}];')
        seen = {}
        for w in words:
            c = const_ident(w)
            if c in seen:
                sys.exit(f"make-inventory.py: cell `{cell['name']}` declares the words "
                         f"`{seen[c]}` and `{w}`, which spell one Rust const `{c}`. "
                         f"The generator refuses rather than renaming one.")
            seen[c] = w
            out.append(f'{pad}    pub const {c}: &str = "{w}";')
    if fields:
        out.append(f'{pad}    /// Every field the producer declares on this cell.')
        out.append(f'{pad}    pub const FIELDS: &[&str] = &[{rust_list(f["name"] for f in fields)}];')
        seen = {"WORDS": "<the cell's own vocabulary>", "FIELDS": "<the field list>"}
        for f in fields:
            c = const_ident(f["name"])
            if c in seen:
                sys.exit(f"make-inventory.py: cell `{cell['name']}` declares fields "
                         f"`{seen[c]}` and `{f['name']}`, which spell one Rust const "
                         f"`{c}`. The generator refuses rather than renaming one.")
            seen[c] = f["name"]
            out.append(f'{pad}    pub const {c}: &str = "{f["name"]}";')
        for f in fields:
            fw = f.get("words") or []
            if not fw:
                continue
            fmod = mod_ident(f["name"], "field")
            out.append(f'{pad}    /// Field `{f["name"]}` — the closed vocabulary it is one of.')
            out.append(f"{pad}    pub mod {fmod} {{")
            out.append(f'{pad}        pub const WORDS: &[&str] = &[{rust_list(fw)}];')
            seen_w = {"WORDS": "<the field's vocabulary>"}
            for w in fw:
                c = const_ident(w)
                if c in seen_w:
                    sys.exit(f"make-inventory.py: cell `{cell['name']}`, field "
                             f"`{f['name']}` declares the words `{seen_w[c]}` and `{w}`, "
                             f"which spell one Rust const `{c}`.")
                seen_w[c] = w
                out.append(f'{pad}        pub const {c}: &str = "{w}";')
            out.append(f"{pad}    }}")
    out.append(f"{pad}}}")


def emit_rust(doc):
    """The whole generated module, as text."""
    short = doc["source_commit"][:7]
    out = []
    out.append("//! `inventory.rs` — **THE PRODUCER'S SURFACE, AS THIS CONSUMER'S BUILD INPUT.**")
    out.append("//!")
    out.append("//! **GENERATED BY `tools/make-inventory.py`, BESIDE `inventory.json`, IN ONE RUN.**")
    out.append("//! Hand-editing it is the one thing the seam cannot survive: it is the producer's")
    out.append("//! surface as the PRODUCER stated it, and a consumer that edited it would be")
    out.append("//! compiling against its own opinion. `tools/seamdiff --inventory` asserts this")
    out.append("//! file and `inventory.json` are the same surface at the same commit, and names")
    out.append("//! the field that moved when they are not.")
    out.append("//!")
    out.append(f'//! Read from `{doc["source_repo"]}` at **`{short}`**')
    out.append(f'//! ({doc["source_commit"]}), via `{doc["shapes_read_from"]}`.')
    out.append("//!")
    out.append("//! ## What is here, and what a missing name looks like")
    out.append("//!")
    out.append("//! * [`methods`] — one [`Method`] const per method the producer dispatches.")
    out.append("//! * one module per shape — one [`Carried`] const per cell it declares, plus the")
    out.append("//!   field names and closed vocabularies of each cell, one level deep.")
    out.append("//!")
    out.append("//! A name that is not here is a name the producer does not serve, and the")
    out.append("//! failure is rustc's own — `error[E0425]: cannot find value ... in module ...`")
    out.append("//! — in the consumer's build, at the line that depends on it. The list of what")
    out.append("//! there IS is this file, and the slices `METHODS`, `CELLS` and `SHARED` carry")
    out.append("//! the same lists for a reader that wants them at runtime.")
    out.append("//!")
    out.append("//! **TIME SKEW IS NOT COVERED AND CANNOT BE.** This is the surface at the commit")
    out.append("//! above; the producer may have moved since. `vendor-chassis.sh` refuses a bump")
    out.append("//! whose producer surface moved, and rule 2.6 refuses a stale CONTRACT at link.")
    out.append("//! Nothing in a consumer's build can see the producer's current surface.")
    out.append("")
    out.append("// Most of this module is vocabulary a given face does not use. A surface is not")
    out.append("// dead code for being larger than one consumer's appetite.")
    out.append("#![allow(dead_code)]")
    out.append("")
    out.append("use shapewire::surface::{Carried, Method};")
    out.append("")
    out.append("/// The repository the surface was read from.")
    out.append(f'pub const SOURCE_REPO: &str = "{doc["source_repo"]}";')
    out.append("")
    out.append("/// **THE COMMIT THIS SURFACE IS.** `mod.rs`'s `SHAPE_VERSION` carries the short")
    out.append("/// form of the same commit, and `seamdiff --inventory` asserts all three agree:")
    out.append("/// this const, `inventory.json`'s `source_commit`, and that stamp.")
    out.append(f'pub const SOURCE_COMMIT: &str = "{doc["source_commit"]}";')
    out.append("")
    out.append("/// The short form, as `SHAPE_VERSION` spells it.")
    out.append(f'pub const SOURCE_COMMIT_SHORT: &str = "{short}";')
    out.append("")
    out.append("/// **EVERY METHOD THE PRODUCER DISPATCHES**, as templates, in its own spelling —")
    out.append("/// `CHASSIS_METHODS` u `WS_METHODS`. The machine-readable twin of [`methods`].")
    out.append(f"pub const METHODS: &[&str] = &[")
    for m in doc["methods"]:
        out.append(f'    "{m}",')
    out.append("];")
    out.append("")
    out.append("/// **EVERY SHAPE THE PRODUCER SERVES.** One module below carries each one's cells.")
    out.append(f'pub const SHAPES: &[&str] = &[{rust_list(s["name"] for s in doc["shapes"])}];')
    out.append("")

    # ── the methods ──────────────────────────────────────────────────────────
    out.append("/// **ONE CONST PER METHOD, SEALED.** A name that is not in here cannot be")
    out.append("/// requested: `Link::request` takes a `Dialed`, and `Method::at` is the only")
    out.append("/// thing that mints one. The two dead verbs `thread.replace` and `thread.unpend`")
    out.append("/// are not here because the producer never dispatched them, which is the whole")
    out.append("/// mechanism.")
    out.append("pub mod methods {")
    out.append("    use super::Method;")
    out.append("")
    seen = {}
    for m in doc["methods"]:
        c = method_const(m)
        if c in seen:
            sys.exit(f"make-inventory.py: `{seen[c]}` and `{m}` spell one Rust const "
                     f"`{c}`. The generator refuses rather than renaming one: a renamed "
                     f"const is a method name the producer did not state. Give the two "
                     f"methods distinguishable tails on the producer.")
        seen[c] = m
        out.append(f'    pub const {c}: Method<{holes(m)}> = Method::__served("{m}");')
    out.append("}")
    out.append("")

    # ── the shapes ───────────────────────────────────────────────────────────
    for s in doc["shapes"]:
        mod = mod_ident(s["name"], "shape")
        per = s["cells"]
        shared = s["shared_cells"]
        out.append(f'/// **SHAPE `{s["name"]}`** — contract v{s["version"]}, serves {s["serves"]}.')
        out.append("///")
        out.append(f'/// Per-key cells: {", ".join(c["name"] for c in per) or "none"}.')
        out.append("///")
        out.append(f'/// Shape-level cells: {", ".join(c["name"] for c in shared) or "none"}.')
        out.append("///")
        out.append("/// A cell that is not named above is a cell no export will ever carry.")
        out.append(f"pub mod {mod} {{")
        out.append("    use super::Carried;")
        out.append("")
        out.append(f'    /// The shape\'s name on the wire. `held.rs`\'s shape const is this one.')
        out.append(f'    pub const SHAPE: &str = "{s["name"]}";')
        out.append("")
        out.append("    /// Every per-key cell the producer declares (`Shape::CELLS`).")
        out.append(f'    pub const CELLS: &[&str] = &[{rust_list(c["name"] for c in per)}];')
        out.append("")
        out.append("    /// Every shape-level cell the producer declares (`Shape::SHARED`).")
        out.append(f'    pub const SHARED: &[&str] = &[{rust_list(c["name"] for c in shared)}];')
        out.append("")
        names = dict((n, "<reserved by this module>") for n in RESERVED_IN_SHAPE)
        for level, cells, is_shared in (("per-key", per, "false"), ("shape-level", shared, "true")):
            if not cells:
                continue
            out.append(f"    // ── the {level} cells ──")
            for c in cells:
                k = const_ident(c["name"])
                if k in names:
                    sys.exit(f"make-inventory.py: shape `{s['name']}` declares `{names[k]}` "
                             f"and `{c['name']}`, which spell one Rust const `{k}`. The "
                             f"generator refuses rather than renaming one.")
                names[k] = c["name"]
                out.append(f'    /// `{c["name"]}` — `{c["dtype"]}`, '
                           f'{"required" if c["required"] else "optional"}.')
                out.append(f'    pub const {k}: Carried = '
                           f'Carried::__declared(SHAPE, "{c["name"]}", {is_shared});')
            out.append("")
        for cells in (per, shared):
            for c in cells:
                emit_cell_module(out, c, 4)
        out.append("}")
        out.append("")
    return "\n".join(out) + "\n"


def write_rust(doc):
    path = os.path.join(SEAM, "inventory.rs")
    with open(path, "w") as fh:
        fh.write(emit_rust(doc))
    return path


# ─────────────────────────────────────────────────────────────────────────────
# THE STAMP — `SHAPE_VERSION` follows the inventory IN THE SAME RUN (R4.S13)
# ─────────────────────────────────────────────────────────────────────────────
#
# `apps/tui/tests/seam_reference.rs` holds three things together: the face's
# `seam/` is the reference outside its fences (so the face's `inventory.rs` must
# be the reference's, byte for byte), and the face's `SHAPE_VERSION` must name
# `inventory.json`'s `source_commit`. Before R4.S13 this script wrote only the
# two inventory files, so EVERY re-take left `seam_reference` red until somebody
# copied `inventory.rs` into the face and re-stamped two `mod.rs` files by hand
# (R3.S2.10, R4.S9 and R4.S12 all did; ledger 313). That is a step a reviewer
# has to remember and a stamp somebody types, which is what generating it was
# meant to end.
#
# So the same run now does the vendor bump's one-line rewrite —
# `vendor-chassis.sh`'s regex, verbatim — on the reference `seam/mod.rs` and on
# every in-repo face copy (`apps/*/src/seam/mod.rs`), and copies `inventory.rs`
# into each face. The stamp is the INVENTORY's commit, never HEAD, for the reason
# `vendor-chassis.sh` gives at length: `SHAPE_VERSION` names the surface, and
# the surface is the inventory.

STAMP_RE = re.compile(r'(pub const SHAPE_VERSION: &str = ")[^"]*(";)')


def faces():
    """Every in-repo face that carries a vendored seam, as repo-relative dirs.

    Discovered rather than listed, so a second face is stamped the day it
    lands. An empty result is a refusal: a stamp that reached no face would
    leave `seam_reference` exactly as red as before and say nothing.
    """
    apps = os.path.join(REPO, "apps")
    found = sorted(
        os.path.join("apps", a, "src", "seam")
        for a in (os.listdir(apps) if os.path.isdir(apps) else [])
        if os.path.isfile(os.path.join(apps, a, "src", "seam", "mod.rs"))
    )
    if not found:
        sys.exit("make-inventory.py: no apps/*/src/seam/mod.rs found under "
                 f"{REPO}; the stamp would reach no face.")
    return found


def stamp(path, short):
    """Rewrite the one `SHAPE_VERSION` line in `path`. Exactly one, or refuse."""
    s = open(path).read()
    new, n = STAMP_RE.subn(rf'\g<1>shapewire@{short}\g<2>', s)
    if n != 1:
        sys.exit(f"make-inventory.py: expected one SHAPE_VERSION in {path}, found {n}")
    if new != s:
        with open(path, "w") as fh:
            fh.write(new)
    return new != s


def stamp_faces(doc):
    """`SHAPE_VERSION` in the reference and every face; `inventory.rs` into each face."""
    short = doc["source_commit"][:7]
    gen = open(os.path.join(SEAM, "inventory.rs")).read()
    moved = []
    if stamp(os.path.join(SEAM, "mod.rs"), short):
        moved.append("libs/shapewire/seam/mod.rs")
    for face in faces():
        if stamp(os.path.join(REPO, face, "mod.rs"), short):
            moved.append(f"{face}/mod.rs")
        dst = os.path.join(REPO, face, "inventory.rs")
        if not os.path.isfile(dst) or open(dst).read() != gen:
            with open(dst, "w") as fh:
                fh.write(gen)
            moved.append(f"{face}/inventory.rs")
    print(f"SHAPE_VERSION = shapewire@{short}; "
          + (f"rewrote {', '.join(moved)}" if moved else "every face already carried it"))


# The fields of `inventory.json` that say WHEN and HOW it was taken, as opposed
# to WHAT the producer serves. Ledger 314: a re-take on a commit whose surface
# did not move used to rewrite only these, a stamp-only diff each deploy merge
# had to revert by hand. Now a re-take whose SURFACE is unchanged keeps the
# committed inventory (and its commit) as it is.
PROVENANCE = ("source_commit", "source_committed_at", "shapes_read_from", "daemon_boot_id")


def surface_of(doc):
    return {k: v for k, v in doc.items() if k not in PROVENANCE}


def main():
    port, shapes_from, declared, rust_only = "3653", None, False, False
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--port":
            port = args[i + 1]; i += 2
        elif args[i] == "--shapes-from":
            shapes_from = args[i + 1]; i += 2
        elif args[i] == "--declared":
            declared = True; i += 1
        elif args[i] == "--rust-only":
            rust_only = True; i += 1
        else:
            sys.exit(f"make-inventory.py: unknown argument `{args[i]}`")

    # `--rust-only` REGENERATES `inventory.rs` FROM THE COMMITTED `inventory.json`
    # and touches nothing else. It is the operator's own phrasing of the decision
    # — *"generate `inventory.rs` from `inventory.json`"* — and it is the path a
    # REVIEWER uses: no daemon, no producer build, and the diff it produces is a
    # diff against the JSON a reviewer already saw land.
    if rust_only:
        if declared or shapes_from:
            sys.exit("make-inventory.py: --rust-only reads the committed inventory.json and "
                     "takes no source flag; pass it alone.")
        # Its only input is the committed JSON, so that file is its whole scope:
        # regenerating `inventory.rs` from an uncommitted `inventory.json` is the
        # same unknown provenance by a shorter route.
        dirt_scope(DIRT_SCOPE_RUST_ONLY, "the inventory this would re-emit as Rust")
        path = os.path.join(SEAM, "inventory.json")
        doc = json.load(open(path))
        rs = write_rust(doc)
        print(f"{rs}: {len(doc['methods'])} methods, {len(doc['shapes'])} shapes, "
              f"at {doc['source_commit'][:7]} (from inventory.json)")
        stamp_faces(doc)
        return

    # BEFORE the expensive read and before anything is written: a surface that is
    # not at HEAD cannot honestly be stamped with HEAD, so there is nothing to be
    # gained by building first and refusing afterwards.
    dirt_scope(DIRT_SCOPE, "the producer surface this inventory is taken from")

    if declared:
        out = subprocess.run(
            ["cargo", "run", "-q", "-p", "threads", "--bin", "shapes-manifest"],
            cwd=REPO, capture_output=True, text=True,
        )
        if out.returncode != 0:
            sys.exit(out.stderr.strip() or "make-inventory.py: shapes-manifest failed")
        payload = json.loads(out.stdout)
        read_from = "declared:cargo run -p threads --bin shapes-manifest"
    elif shapes_from:
        payload = json.load(open(shapes_from))
        read_from = f"file:{os.path.basename(shapes_from)}"
    else:
        out = subprocess.run([sys.executable, os.path.join(HERE, "shapes-list.py"), port],
                             capture_output=True, text=True)
        if out.returncode != 0:
            sys.exit(out.stderr.strip() or "make-inventory.py: shapes-list.py failed")
        payload = json.loads(out.stdout)
        read_from = f"daemon:127.0.0.1:{port}"

    commit = subprocess.run(["git", "-C", REPO, "rev-parse", "HEAD"],
                            capture_output=True, text=True, check=True).stdout.strip()
    at = subprocess.run(["git", "-C", REPO, "show", "-s", "--format=%cI", "HEAD"],
                        capture_output=True, text=True, check=True).stdout.strip()

    chassis = scrape(os.path.join(CRATE, "src", "server.rs"), "CHASSIS_METHODS")
    ws = scrape(os.path.join(REPO, "services", "threads", "src", "server.rs"), "WS_METHODS")

    shapes = []
    for s in payload.get("shapes", []):
        c = s.get("contract") or {}
        shapes.append({
            "name": s["name"],
            "version": c.get("version", 0),
            "serves": c.get("serves", []),
            # The ceilings, carried so `Mirror::check` has something to check
            # against before the first subscribe of a cold start — and so a
            # ceiling that MOVED shows up as a diff in this file.
            "max": s.get("max", {}),
            "parent": s.get("parent"),
            "tracking": s.get("tracking"),
            # THE CELL MANIFEST (`shapewire::cell`): what the shape carries, per
            # key and shape-level, so `seamdiff --inventory` can route every
            # `view::*` read and every compared word against it. A producer that
            # publishes none makes this exit non-zero rather than writing an
            # inventory the cell guard would have to treat as unstated.
            "cells": s["cells"],
            "shared_cells": s["shared_cells"],
        })
    if not shapes:
        sys.exit("make-inventory.py: `shapes/list` named no shapes")
    shapes.sort(key=lambda x: x["name"])

    doc = {
        "_": "GENERATED BY tools/make-inventory.py. Hand-editing this file is the "
             "one thing docs/consumer-seam.md §7.2 cannot survive: it is the "
             "producer's surface as the producer stated it, and a consumer that "
             "edited it would be checking itself against its own opinion.",
        "source_repo": "cauldron",
        "source_commit": commit,
        "source_committed_at": at,
        "shapes_read_from": read_from,
        "daemon_boot_id": payload.get("_boot_id"),
        "methods": sorted(set(chassis) | set(ws)),
        "chassis_methods": sorted(chassis),
        "ws_methods": sorted(ws),
        "shapes": shapes,
    }
    path = os.path.join(SEAM, "inventory.json")
    # LEDGER 314 — a re-take whose SURFACE did not move keeps the committed
    # inventory and its commit, so re-running this on a later commit is a no-op
    # rather than a stamp-only diff. Only when that commit is a cauldron commit
    # in HEAD's history: an inventory naming a commit nobody here can check out
    # is re-taken however unchanged its surface looks.
    committed = json.load(open(path)) if os.path.isfile(path) else None
    if (committed and committed.get("source_repo") == "cauldron"
            and surface_of(committed) == surface_of(doc)
            and subprocess.run(["git", "-C", REPO, "merge-base", "--is-ancestor",
                                committed["source_commit"], "HEAD"],
                               capture_output=True).returncode == 0):
        print(f"{path}: surface unchanged since {committed['source_commit'][:7]}; "
              f"kept as committed (nothing re-taken at {commit[:7]})")
        doc = committed
    else:
        with open(path, "w") as fh:
            json.dump(doc, fh, indent=2)
            fh.write("\n")
        print(f"{path}: {len(doc['methods'])} methods, {len(shapes)} shapes, "
              f"at {commit[:7]}")
    # THE SAME SURFACE, IN THE LANGUAGE IT WILL BE COMPILED IN, IN THE SAME RUN.
    # Two files written from one `doc` cannot disagree; one of them hand-edited
    # can, and `seamdiff --inventory` is what says so.
    rs = write_rust(doc)
    print(f"{rs}: the same surface as the consumer's build input")
    # AND THE STAMP, in the same run, so a re-take leaves `seam_reference` green
    # with no hand edit (ledger 313).
    stamp_faces(doc)


if __name__ == "__main__":
    main()

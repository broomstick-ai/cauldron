#!/usr/bin/env bash
# check-no-personal-data.sh — guards the PUBLIC ARTIFACT against the
# operator's personal data and against real-shaped secrets.
#
# What it scans (REL-4.S7h): not the source tree, but the exact bytes the
# publish step ships. The public repository is `git archive` output, built by
# the ONE shared definition `scripts/build-public-archive` (which
# check-public-tree.sh uses too). This guard builds that artifact for up to
# three commits and scans every member of each:
#   - HEAD                  — what ships if HEAD is published;
#   - the index, committed  — what the next `git commit` would ship;
#   - the working tree      — tracked files as they sit on disk (`add -u`).
# The index and working-tree commits are written to a SCRATCH object store
# (the repository's own objects are only read, as alternates): nothing is
# added to the repository, its index or its refs. They are real COMMITS, not
# trees, because `git archive` expands `export-subst` only for a commit.
#
# For each archive member:
#   - every PATH component (file and directory names) — never exemptible;
#   - SYMLINK and hard-link targets — never exemptible;
#   - the CONTENT, line by line, as raw bytes (binary included) and, where a
#     line is not ASCII, as NFKC-normalised UTF-8 with format characters
#     (zero-width joiners and the like) removed;
#   - UTF-16 / UTF-32 views, by BOM, or by heuristic for any content with a
#     NUL byte (both byte orders, every alignment);
#   - compressed and container members, recursively: gzip, bzip2, xz, zip
#     (member names too), tar (member names and link targets too), and any
#     zlib stream embedded in a binary (PNG text chunks, PDF streams).
#     REL-4.S7i: a member that does not decode to its end (bad CRC or check,
#     truncated, corrupt) has what DID decode scanned, and is then
#     UNSCANNABLE unless DECODE_ALLOW names it by key with a reason; nesting
#     past 8 levels is UNSCANNABLE for every container kind. Both fail;
#     REL-4.S7j: after a failed stream the rest is searched for the next
#     stream header (and, for bzip2, every block by its bit-aligned magic),
#     so an allow entry excuses a failure and never hides a later member; a
#     `BZh` blob with a damaged block magic is a stream that fails, not text;
#   - (an embedded zlib stream is found by a 2-byte header that occurs by
#     chance in ordinary binaries, so a candidate that does not decode is
#     scanned for what it yields and not failed);
#   - pax headers.
# The members are read straight from the tar stream, not extracted to disk:
# extraction onto a case-insensitive or normalising file system (this
# machine's APFS) merges `a/Note` and `a/note` into one file and would lose a
# member that ships.
# Also: any path the archived commit gives the `export-subst` attribute
# (from any `.gitattributes`, shipped or export-ignored, or info/attributes),
# and any shipped `.gitattributes` that sets it, FAILS unless that path is in
# EXPORT_SUBST_ALLOW below. The content scan already sees what
# `$Format:...$` expands to (it scans the archive); this rule refuses the
# mechanism outright.
#
# Secrets are ALSO refused anywhere in the tracked tree (working tree, index,
# HEAD; `git grep -a`), shipped or export-ignored: a real key in a private
# note is still a key in the repository's history.
#
# The personal patterns are NOT in this file (REL-4.S7h, N3): the published
# guard must not state the identifiers it protects. They live in
# `scripts/personal-patterns.private`, which is export-ignored and listed in
# check-public-tree's internal paths, so it never ships. The guard reads it
# when present. When it is ABSENT from a tree that has export-ignored paths
# (the operator's tree; a public archive has none, by construction) the guard
# fails closed, exit 2. Every `sample` line in that file must match its own
# kind, or the guard exits 2: a pattern set that cannot find its own sample
# cannot be trusted to find anything.
#
# Exemptions: a personal hit in CONTENT is exempt only if a LINE KEY below
# matches it, counted per occurrence (a key listed N times exempts N identical
# lines of that file per archive). A key is
#   sha256( <archive path> NUL <the whole line, raw bytes> )
# NUL cannot occur in a git path, so the boundary between the path and the
# line is unambiguous (REL-4.S7g-I N2: the old `<file>|<line>` key let a line
# move into a file named `<file>|`). Never exemptible, key or not: a hit of
# kind `home` or `email`, any path-name hit, any link target, any secret, any
# export-subst use. The SECRET patterns have no exemption of any kind.
#
# Exit: 0 clean · 1 a hit (each printed, personal matches masked as
# <personal>, secrets as their kind only) · 2 the check could not measure
# (no pattern file in the operator's tree, a sample that does not match, an
# archive that would not build or read, a scan error).
#
# HISTORY: REL-4.S2 pattern set; REL-4.S7f per-line keys replaced whole-file
# exemptions; REL-4.S7g scanned worktree+index+HEAD with `git grep -a`;
# REL-4.S7h scans the archive itself, which closes the class the
# REL-4.S7ef-I and REL-4.S7g-I inspections found case by case: a tree scan
# cannot see file names, publish-time `export-subst` expansion, or encoded
# and compressed content the way the shipped artifact carries them.
# REL-4.S7i (REL-4.S7h-I N1-N3): undecodable members and over-deep nesting
# fail closed; the artifact is built from the commit alone (no local
# attribute source can change it, see build-public-archive).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
top="$(pwd)"

# Line keys: sha256(<path>\0<line>) then the path, for the reader. Hex cannot
# spell a personal pattern. A failing line prints its key; adding it here is a
# reviewable one-line diff per line.
LINE_KEYS='
# The public byline (REL-4.S1/S7): NOTICE copyright, README developer link.
# REL-4.S3 launchd label / bundle-id literals and their tests.
# REL-3.S1 soup scrubber and its test, which quote the handle they scrub.
# Only lines that SHIP need a key: export-ignored files are not in the
# archive, so they are not scanned at all (REL-4.S7h dropped their 173 keys).
5c69bc1d35df82d44a1aeeaf293c2c139356fc54e833420739b5149339052459 NOTICE
23b0ed359cd4908864f9e0ed2d42b49638cb559eca1cb1f2cedd8433fad0d677 README.md
dccfabddc479757b10c39e62f2b688b2ffe11462eeb5c7a1417c20559b3de84f cli/src/jobs.rs
a726bacab905db29f2c2b640079af8755ae880198650157502bcda5622352cbd cli/src/launchd.rs
30ecb7672bac32e77494ed1bea0075fbb8db5f01bf06a023b35bd8be7c0975cc cli/src/launchd.rs
e2b836b2b837272e9b1923d904917b3774d1d2b90769642906c4e80155dff72f cli/src/launchd.rs
55808751b13650d5a9b3a25c571410fec682e8d0c053ff76480c375b1e8fabab cli/src/launchd.rs
60a60b60effb7a3fd225e7865de48910b52824cc98977ade6510b995d334b758 cli/src/launchd.rs
9888823b87583aa36d5288e5e99e10a0b74668608806880373a745a833894730 libs/soup/src/scrub.rs
4e29c7cb545f943995d63c66304c6fa39bd9377be26daeeae6654f5115954fb5 libs/soup/tests/it/scrub.rs
9349fd0c42e4477818d763876c9f45fcc4b76e2007bbe90ef9fd5bb4775eeec8 scripts/install_conformance_test.sh
55f80f9cee2d644b4c8c3f65ff81d97da61cfdf9c7266d796412db947c9545d8 scripts/install_conformance_test.sh
8918859532e6a862deda0070f99071f9fbc2292a286264a6c8898ab12749dead scripts/install_conformance_test.sh
1cd20af9f4edd55114b76fb41b017e56aa6c1ec7730878a76eb813b67fd6a16b services/soup-backup.toml
b9412c983935f9233c323430c6d390efed6ebd38961b948ebbe6add5dfa833b1 services/soup-backup.toml
'

# Paths allowed to carry `export-subst` (exact archive paths). Empty: nothing
# in this repository needs it.
EXPORT_SUBST_ALLOW='
'

# Compressed or container members the guard may fail to decode to their end
# (REL-4.S7i, N1). Everything such a member yields is still scanned; without
# an entry here it is UNSCANNABLE and the guard fails. One entry per line:
#   <decode-key> TAB <member, as printed> TAB <why it cannot decode>
# The key is sha256(<member path> NUL <the container's bytes>), printed on
# the UNSCANNABLE line, so an entry stops applying when the bytes change. A
# reason is required. Empty: every container in this repository decodes.
DECODE_ALLOW='
'

SECRET_PATTERN='sk-ant-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{20,}|AKIA[A-Z0-9]{12,}|BEGIN [A-Z ]*PRIVATE KEY'
PATTERNS_FILE="$top/scripts/personal-patterns.private"

# Every deliberate exit goes through finish(). Any other exit (a `set -e` or
# `set -u` abort) is reported as 2, never as its own status: bash 3.2 (this
# machine's /bin/bash) exits 0 from a `set -u` abort when an EXIT trap is
# set, which would read as "clean".
FINISHED=
finish() { FINISHED=1; exit "$1"; }
TMP=$(mktemp -d "${TMPDIR:-/tmp}/check-no-personal-data.XXXXXX")
trap 'st=$?; rm -rf "$TMP"; if [ -z "$FINISHED" ]; then echo "check-no-personal-data: FAIL: aborted (status $st); not a clean scan" >&2; exit 2; fi' EXIT

die2() { echo "check-no-personal-data: FAIL: $*" >&2; finish 2; }

# The non-public marker: a tracked path that is export-ignored. A public
# archive has none (export-ignore is exactly what keeps paths out of it).
private=0
if git ls-files -z | git check-attr -z --stdin export-ignore > "$TMP/ei" 2> "$TMP/ei.err"; then
  if tr '\0' '\n' < "$TMP/ei" | paste - - - | awk -F'\t' '$3=="set"{f=1} END{exit !f}'; then private=1; fi
else
  die2 "git check-attr export-ignore errored: $(head -c 200 "$TMP/ei.err")"
fi
if [ ! -f "$PATTERNS_FILE" ]; then
  if (( private )); then
    die2 "scripts/personal-patterns.private is missing from a private tree (it has export-ignored paths); refusing to report clean"
  fi
  PATTERNS_FILE=""
  echo "check-no-personal-data: note: no personal-pattern file (public tree); scanning secrets and structure only" >&2
fi

# Scratch object store: the index and working-tree commits go here, never
# into the repository.
objects="$(git rev-parse --path-format=absolute --git-path objects)"
real_index="$(git rev-parse --path-format=absolute --git-path index)"
mkdir -p "$TMP/obj"
export GIT_OBJECT_DIRECTORY="$TMP/obj"
export GIT_ALTERNATE_OBJECT_DIRECTORIES="$objects"
if ! git var GIT_AUTHOR_IDENT > /dev/null 2>&1; then
  export GIT_AUTHOR_NAME=check GIT_AUTHOR_EMAIL=check@localhost
fi
if ! git var GIT_COMMITTER_IDENT > /dev/null 2>&1; then
  export GIT_COMMITTER_NAME=check GIT_COMMITTER_EMAIL=check@localhost
fi

HAVE_HEAD=0
git rev-parse -q --verify 'HEAD^{commit}' > /dev/null && HAVE_HEAD=1
parent=()
(( HAVE_HEAD )) && parent=(-p HEAD)

: > "$TMP/sources"
add_source() {  # <label> <commit>
  local label=$1 commit=$2
  if [ -z "$(git ls-tree "$commit")" ]; then   # an empty tree ships nothing
    echo "check-no-personal-data: note: $label has an empty tree; nothing in it ships" >&2
    return 0
  fi
  "$top/scripts/build-public-archive" "$commit" "$TMP/$label.tar" > /dev/null \
    || die2 "build-public-archive $label ($commit) failed"
  # export-subst as the archived commit resolves it, for every path in it.
  git ls-tree -r -z --name-only "$commit" \
    | git check-attr -z --stdin --source="$commit" export-subst > "$TMP/$label.subst" \
    || die2 "git check-attr export-subst on $label errored"
  printf '%s\t%s\n' "$label" "$commit" >> "$TMP/sources"
}

if (( HAVE_HEAD )); then add_source HEAD "$(git rev-parse HEAD)"; fi

if [ -f "$real_index" ]; then
  cp "$real_index" "$TMP/index"
  t_index=$(GIT_INDEX_FILE="$TMP/index" git write-tree) || die2 "write-tree (index) failed"
  c_index=$(git commit-tree ${parent[@]+"${parent[@]}"} -m 'check-no-personal-data: index' "$t_index") || die2 "commit-tree (index) failed"
  add_source index "$c_index"
  cp "$real_index" "$TMP/wt-index"
  GIT_INDEX_FILE="$TMP/wt-index" git add -u -- . || die2 "staging the working tree into a scratch index failed"
  t_wt=$(GIT_INDEX_FILE="$TMP/wt-index" git write-tree) || die2 "write-tree (worktree) failed"
  c_wt=$(git commit-tree ${parent[@]+"${parent[@]}"} -m 'check-no-personal-data: worktree' "$t_wt") || die2 "commit-tree (worktree) failed"
  add_source worktree "$c_wt"
fi
[ -s "$TMP/sources" ] || die2 "nothing to scan: no HEAD and no index"

# Secrets are refused in the private TREE too, not only in what ships: a real
# key in an export-ignored note is still a key in the repository's history.
secret_tree=0
for where in worktree --cached HEAD; do
  [ "$where" = HEAD ] && (( ! HAVE_HEAD )) && continue
  args=(); [ "$where" = worktree ] || args=("$where")
  rc=0
  git grep -a -E -l -e "$SECRET_PATTERN" ${args[@]+"${args[@]}"} -- . > "$TMP/st" || rc=$?
  (( rc > 1 )) && die2 "git grep for secrets ($where) exited $rc; an errored scan is not a clean scan"
  while IFS= read -r f; do
    echo "SECRET-SHAPED: ${f#HEAD:}: a real-shaped key in the tree (<redacted>)  (in: tree $where)"
    secret_tree=1
  done < "$TMP/st"
done

set +e
LINE_KEYS="$LINE_KEYS" EXPORT_SUBST_ALLOW="$EXPORT_SUBST_ALLOW" DECODE_ALLOW="$DECODE_ALLOW" SECRET_PATTERN="$SECRET_PATTERN" \
PATTERNS_FILE="$PATTERNS_FILE" python3 - "$TMP" <<'PY'
import os, re, io, sys, tarfile, zipfile, zlib, bz2, lzma, hashlib, unicodedata

tmp = sys.argv[1]
def die2(msg):
    print("check-no-personal-data: FAIL: " + msg, file=sys.stderr); sys.exit(2)

# ---- patterns ---------------------------------------------------------
PK = []          # (kind, compiled)
samples = []
pf = os.environ.get("PATTERNS_FILE", "")
if pf:
    try:
        for raw in open(pf, encoding="utf-8"):
            ln = raw.rstrip("\n")
            if not ln or ln.startswith("#"): continue
            f = ln.split("\t")
            if f[0] == "personal" and len(f) == 3: PK.append((f[1], re.compile(f[2], re.I)))
            elif f[0] == "sample" and len(f) == 3: samples.append((f[1], f[2]))
            else: die2("unparseable line in the personal-pattern file")
    except (OSError, re.error) as e:
        die2("cannot load the personal-pattern file: %s" % type(e).__name__)
    if not PK: die2("the personal-pattern file has no `personal` lines")
    kinds = {k for k, _ in PK}
    for k in kinds:
        if not any(sk == k for sk, _ in samples): die2("kind %r has no sample line" % k)
    for k, s in samples:
        if not any(pk == k and rx.search(s) for pk, rx in PK):
            die2("a %r sample does not match its own patterns; the pattern set cannot be trusted" % k)
ANYP = re.compile("|".join("(?:%s)" % rx.pattern for _, rx in PK), re.I) if PK else None
# Email patterns can only match a line with an `@`, so the gate runs them on
# those lines alone (a leading `[...]+` makes a whole-blob search quadratic).
for k, rx in PK:
    if k == "email" and "@" not in rx.pattern: die2("an email pattern without a literal @")
_at = [rx.pattern for k, rx in PK if k == "email"]; _no = [rx.pattern for k, rx in PK if k != "email"]
ATP = re.compile("|".join("(?:%s)" % p for p in _at), re.I) if _at else None
NOAT = re.compile("|".join("(?:%s)" % p for p in _no), re.I) if _no else None
SX = re.compile(os.environ["SECRET_PATTERN"])
NEVER_EXEMPT = {"home", "email"}

allowed = {}
for ln in os.environ.get("LINE_KEYS", "").split("\n"):
    m = re.match(r"^([0-9a-f]{64}) ", ln)
    if m: allowed[m.group(1)] = allowed.get(m.group(1), 0) + 1
subst_allow = {l.strip() for l in os.environ.get("EXPORT_SUBST_ALLOW", "").split("\n") if l.strip()}
decode_allow, decode_used = {}, set()
for ln in os.environ.get("DECODE_ALLOW", "").split("\n"):
    if not ln.strip() or ln.lstrip().startswith("#"): continue
    f = ln.split("\t")
    if len(f) != 3 or not re.fullmatch(r"[0-9a-f]{64}", f[0]) or not f[2].strip():
        die2("a DECODE_ALLOW entry is not <key> TAB <member> TAB <reason> (a reason is required)")
    decode_allow[f[0]] = f[2].strip()

def _cf_class():
    cps = [i for i in range(0x80, 0x110000) if unicodedata.category(chr(i)) == "Cf"]
    out, a = [], cps[0]
    for prev, cur in zip(cps, cps[1:] + [None]):
        if cur != prev + 1:
            out.append(re.escape(chr(a)) + ("-" + re.escape(chr(prev)) if prev != a else "")); a = cur
    return re.compile("[%s]" % "".join(out))
CF = []   # format characters (zero-width joiners, bidi marks, ...), built on first use
def norm(u):
    if u.isascii(): return u
    if not CF: CF.append(_cf_class())
    return CF[0].sub("", unicodedata.normalize("NFKC", u))

def mask(s):
    s = SX.sub("<redacted>", s)
    if ANYP: s = ANYP.sub("<personal>", s)
    s = re.sub(r"[^\x20-\x7e]", "?", s)
    return s[:200] + ("..." if len(s) > 200 else "")

def kind_of_secret(s):
    for needle, k in (("sk-ant-", "anthropic-api-key"), ("ghp_", "github-token"),
                      ("AKIA", "aws-access-key-id"), ("PRIVATE KEY", "pem-private-key")):
        if needle in s: return k
    return "secret"

def pkinds(*texts):
    return {k for k, rx in PK for t in texts if rx.search(t)}

# ---- findings ---------------------------------------------------------
hits = {}; order = []
def note(ident, msg, src):
    if ident not in hits: hits[ident] = {"msg": msg, "src": set()}; order.append(ident)
    hits[ident]["src"].add(src)

# Scanning a blob records EVENTS; replay() turns them into hits for one
# archive. Identical members (same path, same bytes) in the HEAD, index and
# working-tree archives are scanned once and replayed per archive.
EV = []
def emit(ident, msg, src=None):
    EV.append(("N", ident, msg))
def replay(events, src, seen):
    for ev in events:
        if ev[0] == "N":
            note(ev[1], ev[2], src)
        else:
            _, path, n, key, ks, shown = ev
            seen[key] = seen.get(key, 0) + 1
            if (ks & NEVER_EXEMPT) or seen[key] > allowed.get(key, 0):
                why = "  [kind %s is never exempt]" % ",".join(sorted(ks & NEVER_EXEMPT)) if ks & NEVER_EXEMPT else "  [line-key %s]" % key
                note(("P", path, n, key), "PERSONAL-DATA: %s:%d: %s%s" % (mask(path), n, shown, why), src)

def check_string(where, what, s, src):
    """A path or a link target: any hit fails, no key can exempt it."""
    vs = [s, norm(s)]
    if pkinds(*vs):
        emit(("P", where, what), "PERSONAL-DATA: %s: %s %s  [%ss are never exempt]" % (mask(where), what, mask(vs[-1]), what), src)
    if any(SX.search(v) for v in vs):
        emit(("S", where, what), "SECRET-SHAPED: %s: %s <redacted kind=%s>" % (mask(where), what, kind_of_secret(s)), src)

LIMIT_CHILD = 256 << 20
budget = [2 << 30]
def unscannable(where, why, src):
    emit(("U", where), "UNSCANNABLE: %s: %s (fails closed)" % (mask(where), why), src)

def scan_lines(path, text_lines, src, seen):
    """text_lines: iterable of (lineno, raw_bytes_for_key, [view strings])."""
    for n, keyb, vs in text_lines:
        ks = pkinds(*vs)
        if ks:
            key = hashlib.sha256(path.encode("utf-8", "surrogateescape") + b"\0" + keyb).hexdigest()
            EV.append(("L", path, n, key, ks, mask(vs[-1])))
        if any(SX.search(v) for v in vs):
            emit(("S", path, n), "SECRET-SHAPED: %s:%d: <redacted kind=%s>" % (mask(path), n, kind_of_secret(" ".join(vs))), src)

def maybe_hit(s):
    """Cheap gate before the line-by-line pass; never narrower than it."""
    if SX.search(s) or (NOAT is not None and NOAT.search(s)): return True
    if ATP is not None and "@" in s:
        return bool(ATP.search("\n".join(l for l in s.split("\n") if "@" in l)))
    return False

def scan_text(path, data, src, seen):
    raw = data.decode("latin-1")
    nonascii = not data.isascii()
    # Only lines with a non-ASCII byte can normalise into something new.
    u = norm(b"\n".join(l for l in data.split(b"\n") if not l.isascii()).decode("utf-8", "replace")) if nonascii else ""
    if not maybe_hit(raw) and not (nonascii and maybe_hit(u)): return
    def gen():
        for i, lb in enumerate(data.split(b"\n"), 1):
            vs = [lb.decode("latin-1")]
            if not lb.isascii(): vs.append(norm(lb.decode("utf-8", "replace")))
            yield i, lb, vs
    scan_lines(path, gen(), src, seen)

def scan_decoded(path, text, src, seen):
    t = norm(text)
    if not maybe_hit(t): return
    def gen():
        for i, l in enumerate(t.split("\n"), 1):
            yield i, l.encode("utf-8", "surrogatepass"), [l]
    scan_lines(path, gen(), src, seen)

# A text-range code unit: printable ASCII, tab, CR or LF. (A whole file of
# UTF-16/32, look-alikes included, is decoded in full by the parity test.)
UTF_RUNS = [
    ("utf-16-le", re.compile(rb"(?:[\x09\x0a\x0d\x20-\x7e]\x00){4,}")),
    ("utf-16-be", re.compile(rb"(?:\x00[\x09\x0a\x0d\x20-\x7e]){4,}")),
    ("utf-32-le", re.compile(rb"(?:[\x09\x0a\x0d\x20-\x7e]\x00\x00\x00){4,}")),
    ("utf-32-be", re.compile(rb"(?:\x00\x00\x00[\x09\x0a\x0d\x20-\x7e]){4,}")),
]
BOMS = [(b"\xff\xfe\x00\x00", "utf-32-le", 4), (b"\x00\x00\xfe\xff", "utf-32-be", 4),
        (b"\xff\xfe", "utf-16-le", 2), (b"\xfe\xff", "utf-16-be", 2)]

# Decoders (REL-4.S7i, N1). Each returns (recovered bytes, error or None).
# The input is fed in chunks, then a byte at a time from a failing chunk, so
# everything a decoder produced before it failed is kept: a bad CRC, a bad xz
# check or a truncated stream still yields the content `gzip -dc` /
# `xz -dc` would print, and that content is scanned. A member the guard could not decode to its end is then UNSCANNABLE
# (fails closed) unless DECODE_ALLOW names it, by key, with a reason.
CH = 4096
DECODE_ERRORS = (zlib.error, OSError, EOFError, ValueError, lzma.LZMAError)
def _run(mk, data, zl, upto):
    """One pass: CH-byte input chunks up to offset `upto`, single bytes after
    it. Returns (out, error, start of the failing chunk, bytes after the
    stream)."""
    d = mk(); out = bytearray(); pos = start = 0
    while not d.eof:
        if zl and d.unconsumed_tail:
            chunk = d.unconsumed_tail
        elif not zl and not d.needs_input:
            chunk = b""
        else:
            if pos >= len(data): break
            n = CH if pos + CH <= upto else 1
            chunk = data[pos:pos + n]; start = pos; pos += n
        try:
            out += d.decompress(chunk, LIMIT_CHILD + 1 - len(out))
        except DECODE_ERRORS as e:
            return out, "%s: %s" % (type(e).__name__, str(e)[:80]), start, b""
        if len(out) > LIMIT_CHILD: return out, "limit", None, b""
    if not d.eof: return out, "truncated (the stream does not end)", None, b""
    return out, None, None, d.unused_data + data[pos:]

def _feed(mk, data, zl):
    """Decode one stream. A decoder that raises discards the output of the
    call that raised (a bad CRC is reported in the same call as the last
    bytes), so on an error the stream is decoded again, a byte at a time
    from the failing chunk: everything before the bad byte is kept."""
    out, err, start, rest = _run(mk, data, zl, len(data))
    if err and start is not None:
        out2 = _run(mk, data, zl, start)[0]
        if len(out2) > len(out): out = out2
    return bytes(out), err, rest

# Bit-aligned bzip2 block and end-of-stream magics (REL-4.S7j, F3).
BZ_BLOCK = 0x314159265359
BZ_EOS = 0x177245385090
MAX_RESYNC = 256

def bit_find(data, magic48):
    """Every bit offset of a 48-bit magic in `data`. bzip2 blocks are not
    byte-aligned, so a byte search cannot find them: for each of the 8 bit
    shifts the 5 bytes the magic fully covers are searched, then all 48 bits
    are verified."""
    hits = []; data = data + b"\0"; mask = (1 << 48) - 1
    for s in range(8):
        mid = (magic48 << (8 - s)).to_bytes(7, "big")[1:6]
        i = data.find(mid, 1)
        while i != -1 and len(hits) <= MAX_RESYNC:
            b0 = i - 1
            if b0 + 7 <= len(data) and (int.from_bytes(data[b0:b0 + 7], "big") >> (8 - s)) & mask == magic48:
                hits.append(b0 * 8 + s)
            i = data.find(mid, i + 1)
    return sorted(hits)

def bz2_recover(seg):
    """bzip2recover in miniature (REL-4.S7j, F3): every block found by its
    magic is rewrapped as a one-block stream (header, the block's bits, the
    end-of-stream magic, the block CRC as the stream CRC) and decoded best
    effort. Returns (recovered bytes, False if there were more blocks than
    MAX_RESYNC, i.e. content may be skipped)."""
    blocks = bit_find(seg, BZ_BLOCK)
    stops = sorted(set(blocks + bit_find(seg, BZ_EOS)))
    out = bytearray(); nbits = len(seg) * 8
    for b in blocks[:MAX_RESYNC]:
        e = next((x for x in stops if x > b), nbits); n = e - b
        if n < 80: continue
        chunk = seg[b // 8:(e + 7) // 8]; cb = len(chunk) * 8; off = b % 8
        bits = (int.from_bytes(chunk, "big") >> (cb - off - n)) & ((1 << n) - 1)
        crc = (bits >> (n - 80)) & 0xFFFFFFFF
        v = (((bits << 48) | BZ_EOS) << 32) | crc; t = n + 80; pad = (-t) % 8
        out += _feed(bz2.BZ2Decompressor, b"BZh9" + (v << pad).to_bytes((t + pad) // 8, "big"), False)[0]
        if len(out) > LIMIT_CHILD: break
    return bytes(out), len(blocks) <= MAX_RESYNC

def decode_multi(data, magic, mk, zl, sync, recover=None):
    """Concatenated streams (gzip members, bz2 / xz streams). Returns
    (recovered bytes, first error or None, complete). REL-4.S7j (F1): after
    a stream that fails, or bytes after a stream that are not another stream,
    the rest is searched for the next stream header and decoding resumes
    there, so a later member is always scanned: DECODE_ALLOW may excuse a
    decode FAILURE, never skip content. Only a stream that starts where the
    previous one ended (or at offset 0) can set the error; a header found by
    search may occur by chance, so it is decoded best effort and what it
    yields is scanned. `recover` salvages what a failed stream still holds
    (bzip2 blocks). complete is False when more than MAX_RESYNC headers had
    to be tried: the caller fails that closed, and no allow entry excuses
    it. Bytes that are no stream at all are left to the raw scan, as
    `gzip -dc` ignores trailing garbage; xz stream padding is NUL bytes."""
    out = bytearray(); first = None; pos = 0; tries = 0; in_seq = True
    while pos < len(data):
        o, err, rest = _feed(mk, data[pos:], zl)
        out += o
        if err == "limit" or len(out) > LIMIT_CHILD: return bytes(out), "limit", True
        if err:
            if in_seq and first is None: first = err
            nxt = sync.search(data, pos + 1)
            if recover:
                r, ok = recover(data[pos:nxt.start() if nxt else len(data)])
                out += r
                if not ok: return bytes(out), first or err, False
                if len(out) > LIMIT_CHILD: return bytes(out), "limit", True
        else:
            end = len(data) - len(rest)
            if magic(rest):
                pos = end; in_seq = True; continue
            nxt = sync.search(data, end)
        if not nxt: break
        tries += 1
        if tries > MAX_RESYNC: return bytes(out), first, False
        pos = nxt.start(); in_seq = False
    return bytes(out), first, True

def is_gz(b): return b[:2] == b"\x1f\x8b"
def is_bz2(b):
    """A full bzip2 header, or (REL-4.S7j, F3) `BZh<level>` with a damaged
    block magic when a block or end-of-stream magic sits anywhere in the
    blob: that is a bzip2 stream with a damaged header, and it must decode
    or fail, never be skipped. Plain text that starts with `BZh5` has no
    48-bit magic in it and is not a stream."""
    if not (b[:3] == b"BZh" and b[3:4] in b"123456789" and len(b) > 3): return False
    return b[4:10] in (b"1AY&SY", b"\x17rE8P\x90") or bool(bit_find(b, BZ_BLOCK) or bit_find(b, BZ_EOS))
def is_xz(b): return b[:6] == b"\xfd7zXZ\x00"
GZ_SYNC = re.compile(rb"\x1f\x8b\x08")
BZ_SYNC = re.compile(rb"BZh[1-9](?:1AY&SY|\x17rE8P\x90)")
XZ_SYNC = re.compile(re.escape(b"\xfd7zXZ\x00"))
DECODERS = [
    ("!gunzip", is_gz, lambda d: decode_multi(d, is_gz, lambda: zlib.decompressobj(31), True, GZ_SYNC)),
    ("!bunzip2", is_bz2, lambda d: decode_multi(d, is_bz2, bz2.BZ2Decompressor, False, BZ_SYNC, bz2_recover)),
    ("!unxz", is_xz, lambda d: decode_multi(d, is_xz, lambda: lzma.LZMADecompressor(lzma.FORMAT_XZ), False, XZ_SYNC)),
]

def inflate_embedded(data):
    """A zlib stream found by its 2-byte header anywhere in a blob. Best
    effort only: those two bytes occur by chance in ordinary binaries, so a
    candidate that does not decode is not a failure. What it does yield is
    scanned."""
    return _feed(lambda: zlib.decompressobj(15), data, True)[0][:LIMIT_CHILD + 1]

def undecodable(where, data, why, src):
    key = hashlib.sha256(where.encode("utf-8", "surrogateescape") + b"\0" + data).hexdigest()
    if key in decode_allow:
        decode_used.add(key); return
    unscannable(where, "does not fully decode (%s) [decode-key %s]" % (why, key), src)

def budgeted(where, out, src):
    """False (and UNSCANNABLE) when `out` breaks a size cap."""
    if len(out) > LIMIT_CHILD:
        unscannable(where, "expands past %d MiB" % (LIMIT_CHILD >> 20), src); return False
    budget[0] -= len(out)
    if budget[0] < 0:
        unscannable(where, "total expansion budget exhausted", src); return False
    return True

def zlib_candidates(data):
    for mt in re.finditer(rb"\x78[\x01\x5e\x9c\xda]", data):
        yield mt.start()

def scan_tar_members(prefix, tf, src, depth, seen):
    for m in tf:
        name = prefix + m.name
        check_string(name, "path", name, src)
        for k, v in (m.pax_headers or {}).items():
            check_string(name, "pax header", "%s=%s" % (k, v), src)
        if m.issym() or m.islnk():
            check_string(name, "link target", m.linkname, src)
        elif m.isfile():
            f = tf.extractfile(m)
            data = f.read() if f else b""
            scan_blob(name, data, src, depth, seen)

def scan_blob(path, data, src, depth, seen):
    scan_text(path, data, src, seen)
    for bom, enc, w in BOMS:
        if data.startswith(bom):
            body = data[len(bom):]; body = body[: len(body) - len(body) % w]
            scan_decoded(path + "#" + enc, body.decode(enc, "ignore"), src, seen)
            break
    if b"\x00" in data:
        # Heuristic, for content with no BOM: (a) every run of text-range
        # characters encoded as UTF-16/UTF-32 (either byte order, any
        # alignment; the `strings -el` idea), and (b) the whole file decoded,
        # when its NUL bytes sit at one parity the way UTF-16/32 text does.
        for enc, rx in UTF_RUNS:
            runs = [m.group(0).decode(enc, "ignore") for m in rx.finditer(data)]
            if runs: scan_decoded("%s#%s-runs" % (path, enc), "\n".join(runs), src, seen)
        sample = data[:65536]
        for enc, w in (("utf-16-le", 2), ("utf-16-be", 2), ("utf-32-le", 4), ("utf-32-be", 4)):
            for off in range(w):
                pick = sample[off + (w - 1 if enc.endswith("le") else 0)::w]
                if len(pick) >= 2 and pick.count(0) * 10 >= len(pick) * 6:
                    body = data[off:]; body = body[: len(body) - len(body) % w]
                    scan_decoded("%s#%s+%d" % (path, enc, off), body.decode(enc, "ignore"), src, seen)
    is_zip = b"PK\x03\x04" in data or b"PK\x05\x06" in data
    is_tar = len(data) > 262 and data[257:262] == b"ustar"
    if depth >= 8:
        # REL-4.S7i N2: past the depth cap EVERY container kind refuses,
        # never just returns. An embedded zlib candidate counts only if it
        # actually inflates to something.
        deep = [n for n, magic, _ in DECODERS if magic(data)]
        if is_zip: deep.append("zip")
        if is_tar: deep.append("tar")
        if not deep:
            for pos in zlib_candidates(data):
                if inflate_embedded(data[pos:]): deep.append("zlib@%d" % pos); break
        if deep:
            unscannable(path, "nested deeper than 8 levels (%s)" % ",".join(deep), src)
        return
    for name, magic, dec in DECODERS:
        if not magic(data): continue
        out, err, complete = dec(data)
        if err == "limit":
            unscannable(path + name, "expands past %d MiB" % (LIMIT_CHILD >> 20), src); continue
        if out and budgeted(path + name, out, src):
            scan_blob(path + name, out, src, depth + 1, seen)
        if not complete:   # content may be skipped: no allow entry excuses that
            unscannable(path + name, "more than %d stream headers or blocks to try" % MAX_RESYNC, src)
        if err:
            undecodable(path + name, data, err, src)
    # zlib streams embedded anywhere (PNG zTXt/iTXt/IDAT, PDF FlateDecode, ...)
    for pos in zlib_candidates(data):
        where = "%s!zlib@%d" % (path, pos)
        out = inflate_embedded(data[pos:])
        if out and budgeted(where, out, src):
            scan_blob(where, out, src, depth + 1, seen)
    if is_zip:
        try:
            zf = zipfile.ZipFile(io.BytesIO(data))
            infos = zf.infolist()
        except Exception as e:
            infos = None
            if data[:4] == b"PK\x03\x04":
                undecodable(path, data, "zip that does not open: %s" % type(e).__name__, src)
        if infos is not None:
            for zi in infos:
                name = path + "!" + zi.filename
                check_string(name, "path", name, src)
                if zi.file_size >= LIMIT_CHILD:
                    unscannable(name, "zip member past %d MiB" % (LIMIT_CHILD >> 20), src); continue
                content, err = bytearray(), None
                try:
                    with zf.open(zi) as fh:
                        # Recover first: ZipExtFile raises on a bad CRC in
                        # the same read() that returns the last bytes, which
                        # would lose them. The CRC is checked below instead.
                        fh._expected_crc = None
                        while True:
                            b = fh.read(CH)
                            if not b: break
                            content += b
                            if len(content) > LIMIT_CHILD: err = "limit"; break
                except Exception as e:
                    err = "%s: %s" % (type(e).__name__, str(e)[:80])
                content = bytes(content)
                if not err and zlib.crc32(content) & 0xffffffff != zi.CRC:
                    err = "bad CRC-32"
                if err == "limit":
                    unscannable(name, "zip member past %d MiB" % (LIMIT_CHILD >> 20), src); continue
                budget[0] -= len(content)
                if budget[0] < 0:
                    unscannable(name, "total expansion budget exhausted", src); return
                if (zi.external_attr >> 16) & 0o170000 == 0o120000:
                    check_string(name, "link target", content.decode("utf-8", "replace"), src)
                elif content:
                    scan_blob(name, content, src, depth + 1, seen)
                if err:
                    undecodable(name, data, err, src)
    if is_tar:
        try:
            tf = tarfile.open(fileobj=io.BytesIO(data), mode="r:")
            scan_tar_members(path + "!", tf, src, depth + 1, seen)
            # tarfile stops SILENTLY at a corrupt header after the first
            # member; a tar that ends cleanly has only NUL blocks left.
            if data[tf.offset:].strip(b"\0"):
                undecodable(path, data, "tar with bytes after its last readable member", src)
        except tarfile.TarError as e:
            undecodable(path, data, "tar that does not read: %s" % type(e).__name__, src)

# ---- the archives -----------------------------------------------------
sources = [l.rstrip("\n").split("\t") for l in open(os.path.join(tmp, "sources"))]
CACHE = {}
for label, commit in sources:
    tpath = os.path.join(tmp, label + ".tar")
    seen = {}
    try:
        with tarfile.open(tpath, mode="r:") as tf:
            members = 0
            for m in tf:
                members += 1
                EV = []
                check_string(m.name, "path", m.name, label)
                for k, v in (m.pax_headers or {}).items():
                    check_string(m.name, "pax header", "%s=%s" % (k, v), label)
                if m.issym() or m.islnk():
                    check_string(m.name, "link target", m.linkname, label)
                replay(EV, label, seen)
                if m.isfile():
                    f = tf.extractfile(m)
                    data = f.read() if f else b""
                    if os.path.basename(m.name) == ".gitattributes":
                        for i, l in enumerate(data.decode("utf-8", "replace").split("\n"), 1):
                            toks = l.split()
                            if toks and not toks[0].startswith("#") and any(t == "export-subst" for t in toks[1:]):
                                # allowed only if the line names an allow-listed path
                                pat = toks[0].lstrip("/")
                                prefix = m.name[: -len(".gitattributes")]
                                if (prefix + pat) not in subst_allow:
                                    note(("E", m.name, i), "EXPORT-SUBST: %s:%d sets export-subst (not in EXPORT_SUBST_ALLOW)" % (mask(m.name), i), label)
                    ck = (m.name, hashlib.sha256(data).digest())
                    if ck not in CACHE:
                        EV = []
                        scan_blob(m.name, data, label, 0, seen)
                        CACHE[ck] = EV
                    replay(CACHE[ck], label, seen)
            EV = []
            for k, v in (tf.pax_headers or {}).items():
                check_string("<pax global header>", "pax header", "%s=%s" % (k, v), label)
            replay(EV, label, seen)
    except (tarfile.TarError, OSError) as e:
        die2("cannot read the %s archive: %s" % (label, type(e).__name__))
    if members == 0:
        die2("the %s archive is empty; nothing measured" % label)
    rec = open(os.path.join(tmp, label + ".subst"), "rb").read().split(b"\0")
    for i in range(0, len(rec) - 2, 3):
        p, val = rec[i].decode("utf-8", "surrogateescape"), rec[i + 2]
        if val not in (b"unspecified", b"unset") and p not in subst_allow:
            note(("E", p), "EXPORT-SUBST: %s has the export-subst attribute (not in EXPORT_SUBST_ALLOW)" % mask(p), label)

for k in sorted(set(decode_allow) - decode_used):
    print("check-no-personal-data: note: DECODE_ALLOW entry %s matched no member" % k, file=sys.stderr)
for ident in order:
    print("%s  (in: %s)" % (hits[ident]["msg"], ",".join(sorted(hits[ident]["src"]))))
sys.exit(1 if order else 0)
PY
prc=$?
set -e
if (( secret_tree )) && (( prc == 0 )); then prc=1; fi

if (( prc == 1 )); then
  echo "check-no-personal-data: hit(s) in the public artifact (see above)" >&2
  finish 1
elif (( prc != 0 )); then
  echo "check-no-personal-data: FAIL: the scanner exited $prc" >&2
  finish 2
fi
echo "check-no-personal-data: clean ($(cut -f1 "$TMP/sources" | paste -sd, -) archives scanned)"
finish 0

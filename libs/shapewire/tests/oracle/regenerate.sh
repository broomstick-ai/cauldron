#!/usr/bin/env bash
# Regenerate `expected.json` — the committed transcript of what the REFERENCE
# mirror produces for each of `mirror_oracle.rs`'s scenarios.
#
# The transcript exists so the differential still asserts something in a
# checkout without `lib/threads` (a released `shapewire` has no producer in
# its tree). When the Python IS present the test runs it live AND compares the
# transcript to it, so this script is only needed when a scenario is added or
# when `shapes.py` legitimately changes.
#
# It writes the scenarios out of the Rust test itself, so the two cannot drift:
# `cargo test -p shapewire --test mirror_oracle -- --ignored dump_scenarios`
# prints them.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
app="$here/../../../../services/threads/scripts/threads-app"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-}"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

cd "$here/../../../.."
cargo test -q -p shapewire --test mirror_oracle -- --ignored --nocapture dump_scenarios \
  | sed -n '/^{/,/^}$/p' > "$scratch/scenarios.json"

python3 - "$app" "$here/drive_shapes_py.py" "$scratch" "$here/expected.json" <<'PY'
import json, subprocess, sys
app, driver, scratch, out = sys.argv[1:5]
scenarios = json.load(open(f"{scratch}/scenarios.json"))
result = {}
for name, s in sorted(scenarios.items()):
    path = f"{scratch}/{name}.json"
    json.dump(s, open(path, "w"))
    got = subprocess.run([sys.executable, driver, app, path],
                         capture_output=True, check=True)
    result[name] = json.loads(got.stdout)
json.dump(result, open(out, "w"), sort_keys=True, indent=1)
print(f"wrote {out}: {len(result)} scenarios")
PY

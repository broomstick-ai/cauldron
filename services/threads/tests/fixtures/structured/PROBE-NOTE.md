# R11.J1 probe note: `claude --json-schema` on 2.1.280

Captured 2026-10-20 with `claude --version` = `2.1.280 (Claude Code)`. There
was one claude session at a time, run in a scratch cwd. These are the
fixtures for step R11.J1 of the structured-output design; the `§` numbers
below refer to that design note, which is not part of this repository. Every
`.jsonl` file is the verbatim stdout of one run. `tests/it/structured_fixtures.rs`
replays them.

## argv

```
env -u ANTHROPIC_API_KEY -u CLAUDE_API_KEY [...] \
  claude -p [--resume <sid>] --output-format stream-json --verbose \
         --model haiku --json-schema '<schema>' '<prompt>'
```

`ANTHROPIC_API_KEY` is unset, as `src/spawn.rs` does. With it set, the run
fails with `"Credit balance is too low"` (`api_error_status: 400`) and never
reaches the model. That capture is not kept.

The daemon also passes `--include-partial-messages` and its settings flags.
They were left out here because they only add `stream_event` lines. They do
not change the `result` line or the tool_use and tool_result blocks.

## Captures

| file | schema / prompt | what came back |
|---|---|---|
| `first-turn-answers.jsonl` | `{verdict∈{merge,block}, count:int}`, an easy question | one `StructuredOutput` call; `result.structured_output = {"verdict":"merge","count":5}`; `result.result` is the same object as compact text |
| `resume-turn-answers.jsonl` | `--resume` of the session above, with a **different** schema `{word, vowels}` | the tool is offered again; the value answers the new schema |
| `resume-turn-no-schema.jsonl` | `--resume` of the same session, **no** `--json-schema` | the tool is not in `init.tools`; no call; the result has **no `structured_output` key** (absent, not null). The schema does not carry over to later turns. |
| `refuses-tool.jsonl` | the prompt tells the model never to call the tool | one injected user line `[structured-output-enforce] You MUST call the StructuredOutput tool…`; the model refuses again; **`subtype:"success"`, `is_error:false`, no `structured_output`** |
| `invalid-then-fixed.jsonl` | `count` has `minimum:10`; the model is told to send an invalid value first | the CLI answers the call with an `is_error` tool_result: `Output does not match required schema: /verdict: …, /count: must be >= 10`. The model corrects itself, and the result holds only the corrected value. |
| `invalid-retries-exhausted.jsonl` | the model is told to keep sending the invalid value | 5 calls. Four of them are unparseable JSON, which appear as `{"__unparsedToolInput":{raw,len}}`. One enforce re-prompt. Ends **`subtype:"error_max_structured_output_retries"`, `is_error:true`, `terminal_reason:"structured_output_retry_exhausted"`**, with no `result` and no `structured_output`. `errors[0]` = `Failed to provide valid structured output after 5 attempts — last StructuredOutput error: …`. The process exits **1**. |
| `bad-schema.stderr` | `--json-schema '{"type":"bogus"}'` | **stdout is empty**, exit **1**, and stderr reads `Error: --json-schema is not a valid JSON Schema: …`. The model is never reached. |

## What this settles (for J2–J4)

1. **"Success without value" vs `error_max_structured_output_retries`: both
   happen, and the trigger decides which one.**
   - A model that **never calls** the tool gets one enforce re-prompt. The
     turn then ends `success` with no value, which matches 0048's probe.
   - A model that **calls it with invalid input** gets refused each time. It
     ends `error_max_structured_output_retries` (`is_error:true`, exit 1)
     after 5 attempts.

   So J2 cannot treat `success` as meaning a value is present. It must also
   re-classify the error subtype as ok + unmet, as §3.3 says. Today
   `retry::classify_result` makes it `Fatal` (asserted in the test, which is
   expected to flip in J2).
2. **The CLI does validate the tool input against the schema.** It rejects
   the input and the model is told the errors. §3.3's "not known" is now
   answered yes. The daemon should still validate, as defence in depth and
   because `__unparsedToolInput` exists.
3. **The fallback "last `StructuredOutput` tool_use input" is not safe
   unless it is validated.** In the exhausted capture the last input is
   `{"verdict":"maybe","count":5}`, which is invalid. In the fixed capture the
   first input is invalid and the last one is valid. When `result` has
   `structured_output` (three captures), it equals the last call's input.
4. **The schema applies per process.** On a resume without the flag the tool
   is gone, and on a resume with a new schema the new schema applies (this
   supports §3.2).
5. **A malformed schema produces no stream at all.** The daemon would only
   see an exit-1 crash, so J3's pre-commit `bad_schema` check is the only
   place this failure gets a name.
6. The enforce re-prompt is a text-only `user` line. Today `stream.rs` keeps
   it as `Unknown`.

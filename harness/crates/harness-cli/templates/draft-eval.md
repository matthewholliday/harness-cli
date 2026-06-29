You are drafting **eval scripts** for the harness spec `{spec_name}`. Write
executable stub scripts to `evals/{spec_name}/` and then stop. Do not implement
or modify any product code. Do not modify anything under `.specs/`, `.harness/`,
or `src/`. Write only files under `evals/{spec_name}/`.

---

## What an eval is

An eval is the harness's **acceptance oracle**: a script that decides whether the
code satisfies a requirement. The defining property is independence —

- An eval interacts with the program the way a *user* would (its CLI, its HTTP
  API, its public surface). It **never** imports test helpers, mocks internals,
  or reads anything under `src/`.
- An eval asserts on observable *behaviour*, not on implementation structure. A
  completely different implementation that behaves correctly must still pass.
- An eval encodes the requirement's **intent**. It is not allowed to be "whatever
  makes the current code pass" — it is the definition of correct, written so that
  *incorrect* code fails.

You are producing **drafts**, not the final oracle. A human will review every
stub before it is trusted. Your job is to give them a strong, honest starting
point — not to guarantee passing scripts.

---

## Requirements to cover

{force_note}

Each requirement below has an `id`, the requirement `text`, and
`acceptance_criteria`. The acceptance criteria are prose; your task is to turn
them into executable checks.

```json
{requirements}
```

---

## Files to write

For each requirement you are covering, write
`evals/{spec_name}/<REQ-ID>-<slug>.sh`, e.g.
`evals/{spec_name}/REQ-001-add-task.sh`. The `<slug>` is a short kebab-case hint
from the requirement. The requirement ID **must** appear in the file (the
coverage gate matches on the ID), so keep it in the filename and repeat it in a
header comment.

Each script:

- starts with `#!/usr/bin/env sh` and `set -e`;
- is hermetic — create a temp working dir (`mktemp -d`), point the program at it
  via its real config/env mechanism, and clean up; never depend on machine state;
- exercises the program through its real interface only;
- signals pass with exit 0 and fail with a non-zero exit (a failed `grep`,
  `test`, or explicit `exit 1`).

### The honesty rule — leave the assertions to the human

Because you are drafting from prose and **must not read the implementation**, you
will not know the program's exact invocation, paths, or output format. Do not
invent them and do not write assertions that merely look plausible — a stub that
passes for the wrong reason is worse than one that fails loudly.

Instead, for anything you cannot ground in the requirement text:

- write the setup and the *shape* of the check;
- mark each unknown with a `# TODO:` comment naming exactly what the human must
  confirm (the command name, the flag, the expected output, the env var);
- end every stub that still contains a `# TODO` with `exit 1` so it FAILS until a
  human has filled it in. A draft must never pass while it is still a guess.

Example shape:

```sh
#!/usr/bin/env sh
# REQ-001 — <requirement text>
# DRAFT eval — a human must verify this encodes the requirement's intent.
set -e
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
# TODO: point the program at $WORK using its real config mechanism (env var? flag?)

# Acceptance criterion: "<criterion from the spec>"
# TODO: replace <invoke> with the real command and <expected> with the real output
# <invoke> add "Buy milk" | grep -q "<expected>"

echo "DRAFT: REQ-001 not yet verified by a human" >&2
exit 1   # remove once the TODOs above are real assertions
```

---

## After writing the files

Print a short summary:

- which requirement IDs you wrote stubs for (and which you skipped, and why);
- the list of `# TODO`s a human still needs to resolve;
- the reminder that these are drafts: each stub currently `exit 1`s until its
  TODOs are replaced with real behaviour-level assertions, and that
  `harness check {spec_name}` will confirm coverage once they exist.

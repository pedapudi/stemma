# Live language-service validation

The ordinary test suite is hermetic. Scripted proposals and deterministic
vectors exercise parser behavior without a network dependency. Live
validation is a separate, explicitly configured acceptance activity.

## Capability check

Provide the service base URL on the command line:

```sh
python3 tools/live_validation.py \
  --endpoint http://language-host:8080/v1
```

The command verifies catalog membership, an ordinary completion, and native
schema-constrained output. It prints only pass/fail stages; it never prints
the selected catalog identifier or response metadata. By default it uses the
first advertised entry; `--catalog-index` selects another entry by position.
The endpoint is not stored in the repository.

Use only synthetic, non-sensitive prompts. Treat the service response as
hostile input and do not use this command as a correctness oracle.

## Acceptance runs

The capability check is a prerequisite, not an acceptance score. Once the
parser runner is available, the live acceptance suite runs fixed trace and
database fixtures through the same proposal contract, deterministic syntax
validation, and bounded read-only execution used by the parser.

Report structured-response validity, syntax parse rate, first-pass validation,
single-repair success, grounding violations, execution accuracy, latency, and
normalized-tree stability. Store only an anonymous deployment fingerprint and
the run date. Live availability never gates the hermetic test suite.

Embedding evaluation is separate. Do not infer embedding support from a
successful language-service check.

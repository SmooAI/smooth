---
"@smooai/smooth": patch
---

C4: trust-but-verify on sidekick dispatch return

`DispatchResult` (the JSON the parent agent gets back from a successful
`send_sidekick` call) now carries two new fields:

- `verified_paths: Vec<String>` — file paths the sidekick named in its
  summary that the parent confirmed exist on the host filesystem at
  dispatch return time (either as absolute paths or relative to CWD).
- `unverified_paths: Vec<String>` — paths the parent couldn't verify;
  may have been renamed, moved, never existed, or be relative to a
  workspace the parent doesn't share.

Both fields are `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
so the JSON shape is unchanged for the common no-paths case (existing
parent-side parsers don't break).

Two new public free functions in `smooth-operator::cast::dispatch`:

- `extract_claimed_paths(text)` — scans free text for tokens that look
  like file paths (contain `/`, or end with a known code/config
  extension), strips trailing punctuation, deduplicates.
- `verify_paths(claimed)` — checks each claimed path against the host
  filesystem (`Path::exists()` as-given or under CWD), returning
  `(verified, unverified)`.

`DispatchSubagentTool::execute()` runs both after the sidekick returns,
so the parent's reasoning includes a structured trust-but-verify list
without requiring any extra plumbing on the parent side.

3 new unit tests (path extraction, dedup + prose-rejection,
verify_paths classification). Existing
`dispatch_result_serializes_to_expected_shape` extended to cover both
the empty-paths case (3 visible JSON fields) and the populated case.

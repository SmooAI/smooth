---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 2 iter-4c. `AwsStsBackend` wraps
`aws sts get-session-token` and `aws sts assume-role`.

Design decisions resolved in this iter:

* **session_token packaging**: added `session_token` (proto field 5)
  to `IssueCredentialResponse`. Additive — older clients ignore
  it. The alternative of JSON-packing into `secret` would have
  broken the Docker credential-helper-shaped contract the proto
  is built around.
* **scope_hint mapping**:
  * `Read` / `Unspecified` → `sts get-session-token`
  * `Write` with `SMOOTH_AWS_WRITE_ROLE_ARN` set →
    `sts assume-role --role-arn … --role-session-name smooth-<op>`
  * `Write` without the env var → falls back to
    `get-session-token` and logs a warning.
* **env var racing**: the role-ARN env is read once at
  construction (`AwsStsBackend::with_runner` / `::new`); tests
  override via `with_write_role_arn(...)` rather than mutating
  the process env, so parallel test runs can't race.

Domain `IssuedCredential` gains a `session_token: Option<String>`
field; existing backends (`GitHubBackend`, test fakes) set it to
`None`. The HostStubServer adapter threads it onto the wire,
defaulting to an empty string when `None`.

10 new tests: default-glob check; `Read`/`Unspecified` →
get-session-token; `Write` with role-arn → assume-role; `Write`
without role-arn → get-session-token fallback; STS CLI failure
→ Mint; malformed JSON → Mint; missing session_token → Mint;
RFC3339 expiration parses; garbage expiration → None.

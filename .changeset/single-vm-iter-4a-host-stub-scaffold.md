---
'@smooai/smooth': patch
---

Pearl th-893801 Phase 2 iter-4a. New `smooth-host-stub` crate
— the credential broker that runs on the macOS host and
bridges the single sandbox VM to host-resident CLIs.

The sandbox sees a UDS bind-mounted at
`/run/smooth/host.sock` and dials this server when an in-VM
tool needs a credential for a known server (GitHub, AWS, GCR,
ECR, …). The stub matches the `server_url` against registered
backends' globs, validates readiness, and shells the matched
backend out for a fresh credential.

Surface shipped in this iter:

* `Backend` trait + `BackendInfo` / `CredentialRequest` /
  `IssuedCredential` / `BackendError` domain types.
* `BackendRegistry` — registration order matters (first
  matching glob wins); routes `issue` by `server_url`.
* `glob_matches` — handles exact hostnames, `*.foo.com`
  subdomain wildcards, and falls back to full glob semantics.
* `HostStubServer` — tonic adapter mapping `IssueCredential`
  and `GetCredentialBackends` onto the registry. Backend
  errors map to the right gRPC `Status` codes
  (`NotFound` / `FailedPrecondition` / `InvalidArgument` /
  `Internal`).
* `serve_uds` — bind-and-spawn helper.
* `smooth-host-stub` binary that reads
  `SMOOTH_HOST_STUB_SOCKET` (default `/run/smooth/host.sock`)
  and serves an empty registry. Concrete backends (gh,
  aws-sts, gcloud, az-acr) land in follow-up iters once the
  shellout audits are reviewed.

15 new tests: glob matching across exact/subdomain/path
strips; registry routing including unknown-server,
not-ready, empty-URL, and overlap-resolution paths;
end-to-end gRPC round trips for issue/list/empty/unknown
over UDS; trait + enum coverage.

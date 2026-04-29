# Spec Review

**Date:** 2026-04-29
**Reviewer:** Context-separated agent (fresh session)
**Spec:** orbit/specs/2026-04-29-arcform-registry/spec.yaml
**Verdict:** APPROVE

---

## Review Depth

| Pass | Triggered by | Findings |
|------|-------------|----------|
| 1 — Structural scan | always | 0 (content signals present: security-adjacent tarball extraction; cross-system transport) |
| 2 — Assumption & failure | content signals | 4 |
| 3 — Adversarial | not triggered (no structural concerns surfaced in Pass 2) | — |

## Findings

### [LOW] Test-time seam for transport selection is not specified
**Category:** test-gap
**Pass:** 2
**Description:** The constraint says "tests use a fixture transport" and "no tests rely on... a system git binary." However, no AC declares the mechanism by which integration tests select FixtureTransport over GitTarballTransport when the entry point is the binary or a top-level run function. If integration tests are library-level (constructing the registry module directly), this is moot. If any integration test goes through the CLI surface, an injection seam (env var, feature flag, or `cfg(test)` builder) is needed.
**Evidence:** ac-04 defines the trait and both impls, but ac-07/ac-08/ac-09/ac-10 verifications say "Integration test loads a fixture index..." / "Integration test using a fixture entry..." without naming how the fixture transport is wired in at the binary boundary.
**Recommendation:** Add a one-line constraint clarifying "registry integration tests construct the registry orchestrator directly with a FixtureTransport; binary-level (`assert_cmd`) coverage is not required for v1." If binary-level coverage is intended, add a constraint declaring the seam (e.g. `$ARCFORM_REGISTRY_TRANSPORT=fixture:<path>`).

### [LOW] cache_root injection seam not declared in ac-03 API
**Category:** test-gap
**Pass:** 2
**Description:** ac-03 verification proposes "injecting a wrapper trait that returns None" as one of two alternative test strategies for the home-dir-missing branch, but the AC's declared signature `cache_root() -> Result<PathBuf>` has no parameter for such injection. The implementor will silently choose either child-process env-clearing or refactor the function to accept an injection seam (changing the surface implied by the AC).
**Evidence:** ac-03 description: `cache_root() -> Result<PathBuf>`. Verification: "or by injecting a wrapper trait that returns None."
**Recommendation:** Pick one approach. Either commit to child-process env-var clearing (Linux-only, simple) or declare the seam in the AC (e.g. `cache_root_with(home_provider: impl HomeProvider) -> Result<PathBuf>` with a default constructor `cache_root() = cache_root_with(SystemHomeProvider)`). Trivial spec edit; useful clarity.

### [LOW] --refresh + offline-grace interaction is ambiguous
**Category:** missing-requirement
**Pass:** 2
**Description:** ac-05 specifies "fetch failure with stale-but-present cache, log to stderr (under --verbose) and carry the cached copy forward." The TTL-triggered fetch path makes sense to fall back gracefully. However, the same rule appears to apply when the user explicitly passed `--refresh`, which is an active request for a fresh fetch. Silently falling back to stale cache after --refresh failure may surprise an end user who passed the flag specifically because they suspect the cache is stale.
**Evidence:** ac-05 description: "If --refresh OR (now - fetched > ttl): attempt transport.fetch_index(url)... On fetch failure with a stale-but-present cache... carry forward." No branch distinguishes the two triggers.
**Recommendation:** Decide whether `--refresh` errors hard on transport failure (user asked for fresh, deliver fresh-or-error) or shares the offline-grace behaviour with TTL refresh. Document in the AC. A one-line addition either way.

### [LOW] Cache root directory creation is implicit
**Category:** missing-requirement
**Pass:** 2
**Description:** On a fresh machine `~/.arcform/registry/` does not exist. The atomic-rename contract assumes a temp directory is a sibling of dest under cache root. No AC explicitly states that the registry implementation creates the cache root (and intermediate `<owner>/`, `<name>/` parents) before attempting the rename. First-fetch behaviour on a fresh machine is therefore implementation-defined.
**Evidence:** ac-04 atomic-write contract assumes a writable parent of `<dest>`; cache_root() in ac-03 only computes the path, not creates it. No AC declares "ensure cache directory tree exists before fetch."
**Recommendation:** Add a one-clause constraint: "the registry creates the cache root and any missing parent directories before fetch; failure to create surfaces as `RegistryCacheIo`." Or fold into ac-04's description of the atomic-write flow.

---

## Honest Assessment

This spec is implementation-ready. The four findings above are all low-severity pinch points — clarifications that will save the implementor a few minutes of guesswork rather than risks that would derail delivery. The spec has already absorbed substantial review feedback (a v1 review surfaced HIGH findings on tarball traversal and `--latest` semantics, both addressed in cycle 2 via ac-04b and the explicit `RegistryUnimplemented` reservation). Atomic-rename discipline is strong, the sister-work boundary is clean, and the AC verifications are concrete enough to write tests against directly. The biggest residual risk is the transport-injection seam (Finding 1) — if integration tests are library-level, the spec is fully ready; if they need binary-level coverage, the seam needs to land before the test fixtures are wired up. Worth confirming the test entry point with the implementor before code starts, but not a blocker.

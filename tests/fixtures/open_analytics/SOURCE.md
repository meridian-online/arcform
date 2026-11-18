# Vendored from `open-analytics`

Byte-identical copies of the four `arcform.yaml` manifests `open-analytics` ships,
plus the `models/*.sql` files each one's `sql:` steps reference, vendored here so
`all_open_analytics_manifests_load_and_pass_the_case_collision_gate` (in
`src/asset.rs`) is self-contained on CI, which checks out only this repo. Both
repos are public.

**Last synced from `open-analytics` commit `a8e0e10f90cc678d297d7cbf41cfab6e4ea4180f`**
(2026-11-18T11:24:31+10:00, the most recent commit to touch any of the vendored
paths as of the sync). The one exception is `edgar_gleif`'s GLEIF fetch: its
`arcform.yaml` fetch step and the `build/gleif_ra_sec.jsonl` read in `models/load.sql` are as at
`589a1963ec6f1d6d191dc3d514d7a35df41092c5`, where the fetch moved to `dlt@1`; the rest of
that Protocol is as at the commit above.

`all_open_analytics_manifests_have_not_drifted_from_their_vendored_checksums` (in
`src/asset.rs`) pins each file's SHA-256 so an accidental edit to a *vendored copy*
fails loudly. It cannot detect the other direction — `open-analytics` changing a
live manifest without anyone re-syncing here — because CI checks out one repo, not
two. Re-syncing is manual: diff `open-analytics`'s four `datasets/*/arcform.yaml`
and their `models/` directories against this one, copy over any changes, update
both the commit SHA above and the expected hashes in the Rust test, in the same
commit.

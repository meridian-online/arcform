# almanac

A tiny, fully offline protocol: three shell steps build a tide/moon almanac for
one port and a fourth prints it. Nothing here needs the network, a database, or
anything beyond a POSIX shell — `arc run` from this directory is the whole demo.

```
arc run
arc run --param port=hobart
```

## Why this example exists

The other examples lean on `sql:` files and quoted one-line commands. This one
is deliberately different: its steps are **inline `command: |` block scalars**,
including one with a blank line *inside* the scalar, plus flush comment headers,
a blank-separated section label, and trailing same-line comments.

Those are exactly the shapes a format-preserving editor has to respect, so this
spec doubles as the test corpus for the library's spec write path (see
`tests/edit_contract.rs`): edits are applied to it and the tests assert that
every byte the edit did not target survives, byte for byte, and that the edited
spec still runs under `arc`.

If you re-shape this file, expect those tests to notice — several of them pin
exact substrings of it as their oracles.

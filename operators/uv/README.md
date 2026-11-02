# uv

Runs a Python script the Protocol carries, pinned by its SHA-256, with exact `package==version` requirements, so two runs a month apart run the same code. Use it for a step no other operator covers. Its code lives in arc itself (`src/operator.rs`).

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Runs a Python script pinned by its digest with uv.

### What it wraps

- [uv](https://docs.astral.sh/uv/): Runs the script with the pinned requirements it is given, in an environment uv builds and caches.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `reads` | list of any | yes | What the script consumes, each name's kind read from how it is written. |

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `produces` | list of any | yes | What the script writes, each name's kind read from how it is written. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `args` | list of string | no | none | Arguments appended after the script path, verbatim and in order. |
| `deps` | list of string | no | none | Third-party requirements, each pinned `package==version`, passed to uv as --with. A range, a wildcard, an environment marker or a direct reference is refused at manifest load: any of them lets two Runs a month apart install different code while the manifest reads as unchanged. |
| `script` | string | yes | none | The Python script to run, relative to the protocol directory. The Protocol carries it; this operator never fetches one. |
| `sha256` | string | yes | none | The script's SHA-256, lowercase hex. Checked against the bytes on disk before uv is spawned, so `op: uv@1` addresses exact bytes the way a frozen-script operator does. Editing the script is therefore a manifest edit too; the refusal prints the digest it found. |

### Example step

```yaml
- name: summarise
  op: uv@1
  with:
    script: scripts/summarise.py
    sha256: 9f2c4e8b1a7d3f6e0c5b8a2d4f1e7c9b3a6d0f8e2c5b7a1d4f9e3c6b0a8d2f5e
    deps: ["polars==1.12.0"]
    reads: [build/breweries.parquet]
    produces: [build/summary.parquet]
```

<!-- arc:reference:end -->

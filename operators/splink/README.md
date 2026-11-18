# splink

Scores record pairs with a model you supply. `op: splink@1` reads one or more Parquet inputs, each under a role, and a Splink settings file, which is Splink's own JSON and carries the blocking rules, comparisons and the `m` and `u` probabilities. It writes Splink's predictions to one Parquet file, sorted so two runs on the same inputs are byte-identical. The step does not train: the model is scored as given. Cleaning the inputs and breaking ties between pairs belong in the Protocol's SQL, upstream and downstream of this step.

<!-- arc:reference:start -->

## Reference

<!-- Generated from arc's operator catalogue (src/operator.rs). Edit it there, then run `ARC_UPDATE_READMES=1 cargo test --lib operator::reference`. -->

Scores record pairs with a Splink model you supply and writes the predictions to a Parquet file.

### What it wraps

- [Splink](https://moj-analytical-services.github.io/splink/): Scores each blocked pair with the model's Fellegi-Sunter weights, which the step takes as given and never trains.
- [DuckDB](https://duckdb.org): The backend Splink runs its comparisons on.

### Reads

| Key | Names | Required | Description |
|---|---|---|---|
| `inputs.path` | list of file | yes | A Parquet file the model scores, one per entry of `inputs`. |
| `model` | file | yes | The Splink settings file, as Splink's own JSON. |

### Writes

| Key | Names | Required | Description |
|---|---|---|---|
| `out` | file | yes | The Parquet file Splink's predictions are written to. |

### Parameters

| Key | Type | Required | Default | Description |
|---|---|---|---|---|
| `inputs` | list of mapping | yes | none | The Parquet files the model scores, each under a role. The role is the name the model's SQL uses for that file's rows (and what `source_dataset` carries in the output); one input is a dedupe, two or more are a link. A role given twice is refused. |
| `threshold_match_probability` | number | no | none | Keep pairs at or above this match probability. Set at most one of the two thresholds; with neither, every blocked pair is written. |
| `threshold_match_weight` | number | no | none | Keep pairs at or above this match weight. Set at most one of the two thresholds. |

### Example step

```yaml
- name: match
  op: splink@1
  with:
    inputs:
      - role: left
        path: build/left.parquet
      - role: right
        path: build/right.parquet
    model: models/match.json
    threshold_match_probability: 0.9
    out: build/matches.parquet
```

<!-- arc:reference:end -->

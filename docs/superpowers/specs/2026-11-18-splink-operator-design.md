# A general Splink operator, whose matching model comes from the Protocol

Design, 2026-11-18. Awaiting Hugh's approval; nothing below is built.

## Intent

`splink_resolve@1` is one crosswalk built as an operator. It reads two files named `edgar` and `gleif`, and its script holds the whole EDGAR to GLEIF model: the name cleaning, the blocking keys, the fixed m and u probabilities, the thresholds and the tie-breaks, with EDGAR's `company_name` and GLEIF's `legal_name` written in. A fix to that model, such as the leading THE that costs Boeing, Coca-Cola and Disney their LEIs, therefore ships as a new arc release, though only one Protocol cares.

An operator is what many Protocols reuse; a source pair is what one Protocol configures. This design replaces `splink_resolve` with `splink@1`, an operator that runs Splink's prediction over the tables a step names, with a model the Protocol supplies. The EDGAR to GLEIF model moves into open-analytics' edgar_gleif Protocol, where its fixes then land as ordinary Protocol changes.

## Goals

- **One operator for any Splink linkage.** `splink@1` links or deduplicates one or more input files with a Splink model the Protocol supplies, and writes Splink's scored pairs. Nothing in it names a source, a column or a dataset.
- **The model is Splink's own format.** The Protocol supplies a Splink settings file, the JSON `Linker` accepts as a path and `linker.misc.save_model_to_json` writes, so a model trained or charted in a notebook is used unchanged and Splink's documentation describes every field.
- **Everything around the model is SQL in the Protocol.** Cleaning, the column mapping and the blocking keys are `sql:` steps before the operator. Choosing a best match, the tie-breaks and the statuses are a `sql:` step after it. arc's SQL reader then sees each of them, and each is tested with `arc test`.
- **The crosswalk moves with its output unchanged.** On the same inputs, edgar_gleif on `splink@1` publishes what it publishes on `splink_resolve@1`. Then `splink_resolve` is removed.
- **The rule that would have refused `splink_resolve`.** REVIEW.md and arcform's guidance on writing an operator say that an operator whose inputs are named for one source, or whose script holds one dataset's columns, endpoints, paging or model, is a finding at Medium or above.

## Non-goals

- **Training.** `splink@1` predicts from a complete model. Every comparison level other than the null level must carry both an m and a u probability, and the operator refuses any other model, because a run that silently estimates parameters is not reproducible. Estimating u by seeded random sampling and freezing the result, which the review below recommends for the crosswalk, is done once in a notebook or by a later `train` mode, and the frozen file is checked into the Protocol.
- **Clustering.** The crosswalk picks a best edge per EDGAR row, which `cluster_pairwise_predictions_at_threshold` does not express. A `clusters` output can be added later as a minor version.
- **Fixing the crosswalk here.** The leading-article fix and the other name-key changes the review argues for land in open-analytics, after the move, as a change to the crosswalk's model with its own re-run and republish.

## The operator

```yaml
- name: score_pairs
  op: splink@1
  with:
    inputs:                       # one file deduplicates; two or more link
      - { role: edgar, path: build/edgar_in.parquet }
      - { role: gleif, path: build/gleif_in.parquet }
    model: models/edgar_gleif_model.json
    threshold_match_probability: 0.30   # or threshold_match_weight; optional
    out: build/scored_pairs.parquet
```

- **Reads** each input path as a file, and the model file as a file, so a change to either makes the step stale. Each input's `role` is the name Splink gives its rows in `source_dataset`, as `input_table_aliases` does today.
- **Writes** one Parquet file: Splink's prediction table as `predict()` returns it (`match_weight`, `match_probability`, the `_l` and `_r` ids and source datasets, each comparison's `gamma_` level), plus the columns the model names in `additional_columns_to_retain`. Rows are sorted by the two ids so the file is byte-identical across runs.
- **Refuses**, before Splink starts: a model with an untrained level; a `link_type` that does not fit the number of inputs (`dedupe_only` takes one, `link_only` two or more); an input missing the model's `unique_id_column_name` or a column a comparison uses; a role named twice.
- **Runs** under uv with Splink and DuckDB pinned exactly, as a frozen script embedded in arc, the same way the other Python operators run.
- **Pins Splink 5.** Splink's documentation, which the review read, is now Splink 5, which also drops pandas from Splink's required dependencies. The move's parity check decides: if Splink 5 changes any published value on the same inputs, `splink@1` pins Splink 4.0.17 instead, and moving to 5 becomes its own change.

## Moving the crosswalk

In open-analytics' edgar_gleif Protocol, the one `splink_resolve` step becomes four, each holding what the script holds today:

1. `sql:` `edgar_in` and `gleif_in`. Today's `NORM_SQL` key and `blocking_select()` keys, written over the two sources into the common columns Splink compares (`uid`, `nname`, `country`, `blk_*`).
2. `models/edgar_gleif_model.json`. Today's settings, written out as Splink's JSON: `link_only`, the four blocking rules, the one name comparison with its frozen m and u, and the prior of 0.01.
3. `op: splink@1`. The step above, at today's prediction threshold of 0.30.
4. `sql:` `resolved`. Today's `edges` query: the best edge per EDGAR row, the ISSUED > LAPSED > RETIRED and US tie-breaks, and `confirmed`, `ambiguous` and `candidate`.

The parity check runs both Protocols on the same inputs and compares the published file row by row. It passes when the rows, ids, statuses and every value match, with `match_probability` and `match_weight` equal to within 1e-12. Then `splink_resolve` is removed from the catalogue, with its README and a CHANGELOG Removed entry.

## The review rule

REVIEW.md gains one line under Meridian-specific: *A change that adds or extends an operator whose inputs are named for one source, or whose script holds one dataset's columns, endpoints, paging or model, is a finding at Medium or above: an operator takes those from the Protocol's `with:`.* The same rule goes into `docs/ARCHITECTURE.md`'s section on operator descriptions and READMEs, where an author learns how an operator is written.

REVIEW.md states that every Meridian code repo carries it byte for byte, so this line lands in each of them in the same pass.

## Review of splink_resolve against Splink's guidance

Read against Splink's documentation at `~/github/moj-analytical-services/splink` (Splink 5.0.0, November 2026). Splink's topic guides say almost nothing about organisation names. The nearest guidance is its business-rates demo (`docs/demos/examples/duckdb_no_test/business_rates_match.nb.py`), which matches company names from Companies House.

Every number below comes from resolving the full published inputs: 10,404 EDGAR ticker-rows against 3,273,734 GLEIF rows. 1.0.0 confirms 4,914 rows, and a name key revised along these findings confirms 5,791. The revised key is kept on branch `wip/splink-name-key-evidence`, with a test for each case.

| Topic | Splink's guidance | splink_resolve | Verdict |
|---|---|---|---|
| Name cleaning | No company-specific rules; the business demo's own list of next steps includes legal-form aliases (`LTD` to `LIMITED`) and spaced initials. | Uppercases, turns punctuation into spaces, strips one trailing suffix. | **Costs matches.** Each case below. |
| Blocking | Several strict rules over one loose rule (`topic_guides/blocking/blocking_rules.md`); count comparisons before predicting; equi-joins only. | Four equi-join rules on derived keys, with stopwords stripped and length guards. | Fine. Leading-article names do block together (on the exact core key); the miss is in the comparison key. |
| Comparisons | Jaro-Winkler suits names whose start matters; the business demo uses a token intersection weighted by token frequency, since a company name is a bag of words. | Exact, then Jaro-Winkler at 0.95, 0.92 and 0.88, on the whole string. | Departs. It costs only candidates, since only exact names confirm: THE BOEING against BOEING scores 0.81 and falls through every level. |
| Term frequency | Adjust exact levels for skew (`topic_guides/comparisons/term-frequency.md`); `NameComparison` does by default. | None. | Departs; no confirmed match changes, because a shared name already makes a row ambiguous. It would make `match_probability` honest for common names. |
| m and u | Estimate u directly by random sampling (`topic_guides/training/training_rationale.md`); the business demo skips m training but still estimates u. | Both set by hand. | Hand-set m is defensible with one comparison. Hand-set u is not needed for reproducibility, since seeded sampling is deterministic. |
| Prior and thresholds | For a one-to-one link, the prior is about one over the larger table's size. | 0.01 against a true scale near 1 in 3.3M. With the hand-set u the two errors offset. The 0.88 level scores 0.17 and always falls below the 0.30 prediction threshold; `confirmed_threshold` and `candidate_threshold` are never read. | Departs. Published probabilities are overstated, and the 0.88 level is dead. |
| Best edge | The business demo takes the best edge per record by match weight. | The same, then registration status and a US preference break ties. | Fine in kind. Ties between different entities that share a key are broken by status, not by name. That is how 1.0.0 matched Toro Co to TORO CORP. and Accenture plc to ACCENTURE INC. |

The cases in name cleaning, each measured by its gain in exact matches over 1.0.0's key:

- **A leading or trailing article (+68).** GLEIF writes THE BOEING COMPANY, THE COCA-COLA COMPANY and THE WALT DISNEY COMPANY; EDGAR writes BOEING CO, COCA COLA CO and Walt Disney Co. GLEIF has 68,427 names that open with THE; EDGAR has none.
- **EDGAR's state and listing tags (+295).** FEDERAL SIGNAL CORP /DE/, CITIZENS FINANCIAL GROUP INC/RI, Glencore plc/ADR.
- **Stacked legal forms.** PETROCHINA CO LTD, TREX CO INC. Stacking every suffix gains 172, but stacking HOLDINGS and GROUP as well would equate SIRIUS XM HOLDINGS INC with its subsidiary SIRIUS XM INC, so only the legal forms stack.
- **& against AND (+44).** PACIFIC GAS & ELECTRIC Co against PACIFIC GAS AND ELECTRIC COMPANY. A CO that follows & is part of the name, though: MORGAN STANLEY & CO. LLC is not MORGAN STANLEY.
- **Dotted and undotted initials (+41).** TELEFONICA S A against TELEFONICA SA, KONINKLIJKE PHILIPS NV against N.V. Only away from the start of a name: joined there, GAP INC meets G.A.P. and CTS CORP meets C.T.S.
- **Accents (+6).** Awale Resources Ltd against Awalé Resources Limited.
- **The Japanese legal form, and other scripts.** 株式会社 is the Japanese legal form: KDDI株式会社 is KDDI CORP. Turning every non-Latin letter into a space, as 1.0.0 does, matches BP PLC to BPエナジージャパン株式会社, a Japanese subsidiary. A word mixing scripts therefore leaves a name with no key, and a word wholly in another script is dropped as a translation (AMTD Digital Inc. 尚乘數字科技集團).
- **LP and LLC are not legal-form noise.** 1.0.0 strips both, so it confirms Tradeweb Markets Inc. to TRADEWEB MARKETS LLC and JBG SMITH Properties to its operating partnership. Of the 44 rows 1.0.0 confirms and the revised key does not, 38 are such matches, and the other six become ambiguous because two entities share the name.
- **A legal-form tie-break.** When entities share a key, prefer the one whose legal form also matches: WD 40 CO takes WD-40 Company, not WD-40 COMPANY LIMITED. Of the 60 companies whose confirmed LEI changes, every one checked by eye is a correction, except ADT Inc., which is wrong both before and after.

Each case becomes a Protocol test in open-analytics with the published names as fixtures, once the cleaning is SQL there. It needs no Python, since DuckDB's regular expressions have Unicode script classes and its list functions can join a run of single letters.

## Calls made on Hugh's behalf

- Cleaning, column mapping and blocking keys are upstream `sql:` steps, and best edge, tie-breaks and statuses a downstream `sql:` step, rather than settings of the operator. The operator holds only what Splink does.
- The model is a file in Splink's own JSON format, not arc-specific YAML.
- Prediction only, from a complete model: no training, no clustering.
- Splink 5, unless the parity check shows a changed value.
- The operator is named `splink`, after what it wraps, as `dlt` and `uv` are.

## Open for Hugh

- **REVIEW.md in every repo.** The rule's line lands in each Meridian code repo's REVIEW.md in the same pass, since the file is meant to be identical in all of them. Alternatively it lands in arcform's REVIEW.md alone, and the others follow.

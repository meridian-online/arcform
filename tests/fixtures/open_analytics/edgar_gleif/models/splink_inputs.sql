-- Prepare the two sides of the name match for `score_pairs` (op: splink@1), which takes
-- the model from models/edgar_gleif_model.json and everything else from these files.
-- This SQL and resolved.sql are the crosswalk's own half of the match.
--
-- The name key (`nname`) is the only signal the model compares, and the only thing an
-- exact-name confirmation rests on, so each rule below is one way EDGAR and GLEIF write
-- the same entity differently. On the corpus of November 2026 they raised confirmed edges
-- for filers with a ticker from 5,077 to 5,701 (README.md has the breakdown). In order:
--   - Fold accents (Awalé -> AWALE) and NFKD's compatibility forms (ＩＨＩ -> IHI, IQ² ->
--     IQ2). A word wholly in another script is dropped, as GLEIF appends a translation
--     (AMTD Digital Inc. 尚乘數字科技集團). A word mixing scripts leaves the name with no
--     key: the Latin letters left after dropping the rest are not the name, so BP PLC
--     would match BPエナジージャパン株式会社. 株式会社, the Japanese legal form, is dropped
--     first, so KDDI株式会社 is KDDI and 株式会社ＩＨＩ is IHI.
--   - Drop EDGAR's trailing state or listing tag (/DE/, /MD, /ADR, /NEW).
--   - & and + are AND; punctuation becomes a space.
--   - A leading or trailing THE is dropped: GLEIF writes THE BOEING COMPANY, EDGAR BOEING
--     CO. Keeping it cost Boeing, Coca-Cola, Disney and Goldman Sachs their LEIs.
--   - A run of single letters is joined (S A -> SA, L L C -> LLC) unless it opens the
--     name, so dotted and undotted legal forms agree without GAP INC meeting G.A.P.
--   - Every trailing legal form is dropped (CO LTD, CORP PLC), since they are spellings
--     of one entity's form, except a CO after AND: MORGAN STANLEY & CO. LLC is not MORGAN
--     STANLEY. LP and LLC are kept: a same-named LP or LLC is usually a different entity
--     from the listed corporation (a REIT's operating partnership, a broker-dealer
--     subsidiary). HOLDINGS or GROUP is dropped only as the last word of a name with no
--     legal form, so SIRIUS XM HOLDINGS INC is not its subsidiary SIRIUS XM INC.
--   - AND is dropped, so A & B and A AND B agree.
--
-- `fname` (form_key) is the same name with its legal forms kept, each in one spelling
-- (COMPANY -> CO). The model does not compare it: resolved.sql breaks a tie between GLEIF
-- entities sharing a name key with it, so WD 40 CO takes WD-40 Company over WD-40 COMPANY
-- LIMITED.
--
-- These rules were measured with a Python implementation. These macros give the same
-- keys as it on every name in the EDGAR and GLEIF inputs, except the 60,323 holding a
-- Latin letter NFKD does not fold (Ø, Ł, Æ, ß and the rest): Python left those words
-- mixing scripts and the name with no key, and the macros spell them as ASCII. The
-- macros stay in the database, so tests.yaml holds each rule to its cases by calling
-- name_key directly.

-- The compatibility forms of NFKD that occur in company names, folded as NFKD folds them:
-- full-width ASCII (ＩＨＩ), superscript and subscript digits (IQ², H₂O), º and ª, the
-- Roman numerals, ™, ℠, №, the vulgar fractions and the ligatures ﬁ and ﬂ. DuckDB has no
-- NFKD; strip_accents below does the rest of what it does here, folding accents. Then
-- the Latin letters neither folds that occur in GLEIF's names (Ł, Ø, Đ, Ð, ı, Ħ, Ŧ, ƫ, ĸ,
-- Ə, Æ, Œ, ß, Þ, Ŋ), spelt as ASCII: left alone, Ørsted A/S would read as a word mixing
-- scripts and lose its key.
CREATE OR REPLACE MACRO name_fold(name) AS (
  list_reduce(
    [['Ⅻ', 'XII'], ['Ⅺ', 'XI'], ['Ⅹ', 'X'], ['Ⅸ', 'IX'], ['Ⅷ', 'VIII'], ['Ⅶ', 'VII'], ['Ⅵ', 'VI'],
     ['Ⅴ', 'V'], ['Ⅳ', 'IV'], ['Ⅲ', 'III'], ['Ⅱ', 'II'], ['Ⅰ', 'I'], ['™', 'TM'], ['℠', 'SM'],
     ['№', 'No'], ['¼', '1 4'], ['½', '1 2'], ['¾', '3 4'],
     ['Æ', 'AE'], ['æ', 'ae'], ['Œ', 'OE'], ['œ', 'oe'], ['ẞ', 'SS'], ['ß', 'ss'], ['Þ', 'TH'], ['þ', 'th'],
     ['Ŋ', 'NG'], ['ŋ', 'ng'], ['ﬁ', 'fi'], ['ﬂ', 'fl']],
    lambda acc, pair: replace(acc, pair[1], pair[2]),
    translate(translate(coalesce(name, ''), 'ŁłØøĐđÐðıĦħŦŧƫĸƏə', 'LlOoDdDdiHhTttkEe'),
      '！＂＃＄％＆＇（）＊＋，－．／０１２３４５６７８９：；＜＝＞？＠ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ［＼］＾＿｀ａｂｃｄｅｆｇｈｉｊｋｌｍｎｏｐｑｒｓｔｕｖｗｘｙｚ｛｜｝～⁰¹²³⁴⁵⁶⁷⁸⁹₀₁₂₃₄₅₆₇₈₉ºª',
      '!"#$%&''()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\]^_`abcdefghijklmnopqrstuvwxyz{|}~01234567890123456789oa'))
);

CREATE OR REPLACE MACRO name_tokens(name) AS (
  list_transform([
    -- Words: compatibility forms and accents folded, 株式会社 dropped.
    list_filter(regexp_split_to_array(replace(strip_accents(name_fold(name)),
        '株式会社', ' '), '[\s\p{Z}]+'), lambda w: w <> '')
  ], lambda words:
    CASE
      -- A word holding a letter outside ASCII and an ASCII letter or digit: no key.
      WHEN list_bool_or(list_transform(words, lambda w:
             regexp_matches(regexp_replace(w, '[[:ascii:]]', '', 'g'), '\p{L}')
             AND regexp_matches(w, '[A-Za-z0-9]'))) THEN []
      ELSE list_transform([
        -- Words wholly in another script dropped; EDGAR's tag dropped; & and + are AND;
        -- punctuation to spaces; then the tokens.
        list_filter(string_split(regexp_replace(
            replace(replace(upper(regexp_replace(array_to_string(
                list_filter(words, lambda w: NOT regexp_matches(regexp_replace(w, '[[:ascii:]]', '', 'g'), '\p{L}')),
                ' '), '\s*/[A-Za-z]{2,3}/?\s*$', '')), '&', ' AND '), '+', ' AND '),
            '[^A-Z0-9 ]', ' ', 'g'), ' '), lambda t: t <> '')
      ], lambda t0: list_transform([
        -- A leading, then a trailing, THE dropped.
        list_transform([CASE WHEN t0[1] = 'THE' THEN list_slice(t0, 2, len(t0)) ELSE t0 END],
          lambda t1: CASE WHEN t1[-1] = 'THE' THEN list_slice(t1, 1, len(t1) - 1) ELSE t1 END)[1]
      ], lambda t: list_transform([
        -- A run of single letters joined unless it opens the name.
        list_filter(string_split(array_to_string(list_transform(range(1, len(t) + 1), lambda i:
            CASE WHEN i > 1
                      AND regexp_full_match(t[i], '[A-Z]') AND regexp_full_match(t[i - 1], '[A-Z]')
                      AND list_bool_or(list_transform(list_slice(t, 1, i), lambda x: NOT regexp_full_match(x, '[A-Z]')))
                 THEN '' ELSE ' ' END || t[i]), ''), ' '), lambda x: x <> '')
      ], lambda j:
        -- COMPANY after AND is CO.
        list_transform(range(1, len(j) + 1), lambda i:
          CASE WHEN j[i] = 'COMPANY' AND i > 1 AND j[i - 1] = 'AND' THEN 'CO' ELSE j[i] END)
      )[1])[1])[1]
    END
  )[1]
);

CREATE OR REPLACE MACRO legal_form_count(t) AS (
  -- How many trailing legal forms to drop: all of them, leaving at least one token, and
  -- not a CO that follows AND.
  coalesce(list_position(list_transform(range(len(t), 1, -1), lambda i:
      t[i] NOT IN ('CORPORATION', 'CORP', 'INCORPORATED', 'INC', 'COMPANY', 'CO', 'LIMITED', 'LTD', 'PLC')
      OR (t[i] = 'CO' AND t[i - 1] = 'AND')), true) - 1, greatest(len(t) - 1, 0))
);

CREATE OR REPLACE MACRO name_key(name) AS (
  list_transform([name_tokens(name)], lambda t:
    list_transform([list_slice(t, 1, len(t) - legal_form_count(t))], lambda s:
      array_to_string(list_filter(
        CASE WHEN len(s) = len(t) AND len(s) > 1 AND s[-1] IN ('HOLDINGS', 'GROUP')
             THEN list_slice(s, 1, len(s) - 1) ELSE s END,
        lambda x: x <> 'AND'), ' '))[1])[1]
);

CREATE OR REPLACE MACRO form_key(name) AS (
  array_to_string(list_transform(list_filter(name_tokens(name), lambda x: x <> 'AND'), lambda x:
    CASE x WHEN 'CORPORATION' THEN 'CORP' WHEN 'COMPANY' THEN 'CO'
           WHEN 'INCORPORATED' THEN 'INC' WHEN 'LIMITED' THEN 'LTD' ELSE x END), ' ')
);

-- EDGAR is every named SEC filer at ticker grain (former names included). Its `uid` is
-- numbered in a fixed order so a re-run numbers the same rows the same way; it is an
-- internal join key and never reaches the published file.
CREATE OR REPLACE TABLE edgar_src AS
WITH n AS (
  SELECT cik, ticker, company_name, name_key(company_name) AS nname, form_key(company_name) AS fname
  FROM read_parquet('build/sec_entities.parquet')
)
SELECT 'e' || CAST(row_number() OVER (ORDER BY cik, ticker, company_name) AS VARCHAR) AS uid,
       cik, ticker, company_name AS raw_name, nname, fname
FROM n
WHERE nname <> '';

-- GLEIF: every ISSUED, LAPSED or RETIRED registration (a lapsed registration does not
-- change the entity's LEI), keyed by its LEI. `country` (domicile) is a blocking key;
-- `fname`, `jurisdiction` and `reg_status` feed resolved.sql's tie-break.
CREATE OR REPLACE TABLE gleif_src AS
WITH n AS (
  SELECT lei, legal_name, jurisdiction, country, registration_status,
         name_key(legal_name) AS nname, form_key(legal_name) AS fname
  FROM read_parquet('build/gleif.parquet')
)
SELECT lei AS uid, legal_name AS raw_name, jurisdiction, country,
       registration_status AS reg_status, nname, fname
FROM n
WHERE nname <> '' AND registration_status IN ('ISSUED', 'LAPSED', 'RETIRED');

-- The blocking keys, the same for both sides. Every name key derives from `core`, the
-- name with one leading stopword token removed (STICHTING, SOCIETE, ...), so those rows
-- disperse instead of pooling into one block. THE stays in the list, though the name key
-- has already dropped it: on the full GLEIF copy it led 66k names. A key is NULL when its guard fails, which keeps the row out of that block.
--   blk_p6    6-character prefix, paired with country in the model (names >= 6)
--   blk_p8    8-character prefix, no country: foreign near-misses (names >= 8)
--   blk_tok   first token of 4 or more characters, paired with country
--   blk_exact the whole core name, no country: foreign ADRs of any length
-- EDGAR filers are US-listed, so EDGAR's `country` is the literal 'US'.
CREATE OR REPLACE TABLE edgar_in AS
WITH base AS (
  SELECT uid, nname, 'US' AS country,
         CASE WHEN split_part(nname, ' ', 1) IN ('THE', 'STICHTING', 'SOCIETE', 'STIFTUNG', 'FONDATION', 'ASSOCIATION', 'FUNDACION', 'FONDAZIONE')
              THEN substr(nname, length(split_part(nname, ' ', 1)) + 2) ELSE nname END AS core
  FROM edgar_src
)
SELECT uid, nname, country,
       CASE WHEN length(core) >= 6 THEN substr(core, 1, 6) END AS blk_p6,
       CASE WHEN length(core) >= 8 THEN substr(core, 1, 8) END AS blk_p8,
       CASE WHEN length(split_part(core, ' ', 1)) >= 4 THEN split_part(core, ' ', 1) END AS blk_tok,
       CASE WHEN length(core) >= 2 THEN core END AS blk_exact
FROM base;

CREATE OR REPLACE TABLE gleif_in AS
WITH base AS (
  SELECT uid, nname, country,
         CASE WHEN split_part(nname, ' ', 1) IN ('THE', 'STICHTING', 'SOCIETE', 'STIFTUNG', 'FONDATION', 'ASSOCIATION', 'FUNDACION', 'FONDAZIONE')
              THEN substr(nname, length(split_part(nname, ' ', 1)) + 2) ELSE nname END AS core
  FROM gleif_src
)
SELECT uid, nname, country,
       CASE WHEN length(core) >= 6 THEN substr(core, 1, 6) END AS blk_p6,
       CASE WHEN length(core) >= 8 THEN substr(core, 1, 8) END AS blk_p8,
       CASE WHEN length(split_part(core, ' ', 1)) >= 4 THEN split_part(core, ' ', 1) END AS blk_tok,
       CASE WHEN length(core) >= 2 THEN core END AS blk_exact
FROM base;

-- splink@1 reads Parquet, and both inputs must carry the same columns with the same types.
COPY edgar_in TO 'build/edgar_in.parquet' (FORMAT parquet);
COPY gleif_in TO 'build/gleif_in.parquet' (FORMAT parquet);

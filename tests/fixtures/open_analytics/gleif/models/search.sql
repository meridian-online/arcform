-- The search index: one row per (word token, entity), the token drawn from the entity's
-- legal name and city. Sorted by token at export with small row groups, a prefix range
-- scan (`token >= 'micro' AND token < 'micrp'`) reads about one row group, which is what
-- makes word-prefix typeahead quick over https. strip_accents + lower matches the
-- corpus normalisation, so a search literal normalised the same way lines up. Tokens of
-- one character are dropped as noise. It carries only the identity columns a search
-- result shows, which keeps the footer and each matched row group small.
CREATE OR REPLACE TABLE gleif_search AS
SELECT DISTINCT
  token, lei, legal_name, country, city, entity_status
FROM (
  SELECT *,
    unnest(string_split(lower(strip_accents(concat_ws(' ', legal_name, city))), ' ')) AS token
  FROM gleif_out
)
WHERE length(token) >= 2;

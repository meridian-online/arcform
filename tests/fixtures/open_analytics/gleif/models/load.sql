-- Load the GLEIF golden-copy full CSV into the published 10-field schema.
-- Run by the `load` step. `all_varchar=true` avoids type-inference surprises across
-- the ~200 golden-copy columns; only the two dates are cast. The [1:10] slice turns
-- the ISO timestamps into plain calendar dates. Normalisation is deliberately light:
-- these are the official register's own values, passed through verbatim.
CREATE OR REPLACE TABLE gleif AS
  SELECT
    "LEI"                                    AS lei,
    "Entity.LegalName"                       AS legal_name,
    "Entity.LegalAddress.Country"            AS country,
    "Entity.LegalAddress.City"               AS city,
    "Entity.LegalJurisdiction"               AS jurisdiction,
    "Entity.LegalForm.EntityLegalFormCode"   AS legal_form,
    "Entity.EntityStatus"                    AS entity_status,
    "Registration.RegistrationStatus"        AS registration_status,
    TRY_CAST("Registration.InitialRegistrationDate"[1:10] AS DATE) AS initial_registration,
    TRY_CAST("Registration.NextRenewalDate"[1:10]         AS DATE) AS next_renewal
  FROM read_csv('build/gleif_src/*.csv', header = true, all_varchar = true,
                quote = '"', escape = '"');

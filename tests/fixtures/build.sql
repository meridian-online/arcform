-- Reproducible fixture for `arc init --from-descriptor`.
--
-- A minimal two-table "signups" database with one real foreign key. Build it
-- into a fresh DuckDB and run `dovetail relate` against it to produce the
-- committed signups.datapackage.json descriptor:
--
--     duckdb signups.duckdb -init /dev/null -f build.sql
--     dovetail relate signups.duckdb --out signups.datapackage.json
--
-- accounts.id is a unique key; logins.account_id references it with no orphan
-- rows and FK-shaped naming, so relate verifies referential integrity and
-- auto-accepts logins.account_id -> accounts.id. No other candidate edge holds,
-- so the descriptor carries exactly one accepted foreign key.

CREATE TABLE accounts (
    id         INTEGER,
    email      VARCHAR,
    created_at TIMESTAMP
);
INSERT INTO accounts VALUES
    (1, 'ada@example.com',     '2026-01-02 09:00:00'),
    (2, 'alan@example.com',    '2026-01-03 10:30:00'),
    (3, 'grace@example.com',   '2026-01-05 14:15:00'),
    (4, 'edsger@example.com',  '2026-01-07 08:45:00'),
    (5, 'barbara@example.com', '2026-01-09 16:20:00');

CREATE TABLE logins (
    id           INTEGER,
    account_id   INTEGER,
    logged_in_at TIMESTAMP
);
INSERT INTO logins VALUES
    (100, 1, '2026-01-02 09:05:00'),
    (101, 2, '2026-01-03 11:00:00'),
    (102, 1, '2026-01-04 07:30:00'),
    (103, 3, '2026-01-06 12:00:00'),
    (104, 5, '2026-01-10 18:00:00');

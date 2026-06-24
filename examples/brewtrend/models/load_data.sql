-- Create a table for casks data from JSON file
CREATE OR REPLACE TABLE casks AS
SELECT *
FROM read_json('data/cask/cask.json', format = 'array');

-- Create a table for core formula data from JSON file
CREATE OR REPLACE TABLE core AS
SELECT *
FROM read_json('data/formula/formula.json', format = 'array');

-- Create a table for brew analytics data from multiple JSON files
CREATE OR REPLACE TABLE brew_analytics AS
SELECT *
FROM read_json(
    [
        'data/cask/30d.json',
        'data/cask/90d.json',
        'data/formula/30d.json',
        'data/formula/90d.json'
    ],
    filename = TRUE,
    columns = {
        category: 'VARCHAR',
        total_items: 'BIGINT',
        start_date: 'DATE',
        end_date: 'DATE',
        total_count: 'BIGINT',
        formulae: 'JSON'
    }
);

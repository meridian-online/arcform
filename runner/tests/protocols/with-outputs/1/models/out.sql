COPY (SELECT range AS n FROM range(10)) TO 'build/out.parquet';

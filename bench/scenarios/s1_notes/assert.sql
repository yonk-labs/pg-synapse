-- s1_notes assertion: exactly one row with body = 'BENCH_MARK_OK' exists.
SELECT count(*) = 1 AS passed
FROM demo.notes
WHERE body = 'BENCH_MARK_OK';

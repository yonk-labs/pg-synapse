-- s2_triage assertion: every ticket has a non-null category AND priority.
-- (escalated correctness is a bonus; base pass requires classification.)
SELECT bool_and(category IS NOT NULL AND priority IS NOT NULL) AS passed
FROM support.tickets;

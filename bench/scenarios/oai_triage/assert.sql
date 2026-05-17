-- oai_triage assertion: the specialist's answer contains 72 (18 * 4 = 72).
SELECT (SELECT answer FROM triage.log LIMIT 1) LIKE '%72%' AS passed;

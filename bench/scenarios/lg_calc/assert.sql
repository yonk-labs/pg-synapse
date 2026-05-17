-- lg_calc assertion: the agent computed (12+30)*7 = 294 and stored it.
SELECT (SELECT value FROM lg.result WHERE label = 'answer') = 294 AS passed;

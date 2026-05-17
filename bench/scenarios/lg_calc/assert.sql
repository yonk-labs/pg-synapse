-- lg_calc assertion: the agent's final output text contains "294" ((12+30)*7).
-- Checks synapse.executions.output instead of a DB table, so the scenario
-- tests typed-tool use without model-authored SQL.
SELECT (
    SELECT output LIKE '%294%'
    FROM synapse.executions
    ORDER BY started_at DESC LIMIT 1
) AS passed;

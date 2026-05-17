-- oai_triage assertion: the agent's output text contains 72 (18 * 4 = 72).
-- Checks synapse.executions.output instead of triage.log, so the scenario
-- tests typed-tool delegation without model-authored SQL.
SELECT (
    SELECT output LIKE '%72%'
    FROM synapse.executions
    ORDER BY started_at DESC LIMIT 1
) AS passed;

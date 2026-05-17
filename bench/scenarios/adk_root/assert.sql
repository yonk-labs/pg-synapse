-- adk_root assertion: the agent's output confirms it received a timestamp.
-- Checks synapse.executions.output for timestamp-shaped content (year + T separator)
-- instead of a DB table, so the scenario tests typed-tool use only.
SELECT (
    SELECT output ~* '\d{4}.*T'
    FROM synapse.executions
    ORDER BY started_at DESC LIMIT 1
) AS passed;

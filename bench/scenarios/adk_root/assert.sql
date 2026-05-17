-- adk_root assertion: the agent received a timestamp and recorded true.
SELECT (SELECT has_time FROM adk.probe LIMIT 1) = true AS passed;

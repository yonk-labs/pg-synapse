-- a3_triage assertion.
-- Returns a single boolean column `passed` that is true only when ALL of:
--
-- 1. Classification completeness: every ticket has a non-null category,
--    priority in the allowed set, and a non-null handled_at.
--
-- 2. Escalation correctness (both directions):
--    For every ticket, escalated must equal
--    (customer tier = 'enterprise' AND priority = 'urgent').
--    This catches both false negatives (enterprise+urgent NOT escalated)
--    and false positives (non-enterprise or non-urgent incorrectly escalated).
--
-- 3. Audit completeness: support.audit has exactly one row per ticket
--    (no missing rows, no duplicate rows).

SELECT
    -- Condition 1: classification completeness
    bool_and(
        t.category IS NOT NULL
        AND t.priority IN ('low', 'normal', 'high', 'urgent')
        AND t.handled_at IS NOT NULL
    )
    -- Condition 2: escalation correctness (both directions)
    AND bool_and(
        t.escalated = (c.tier = 'enterprise' AND t.priority = 'urgent')
    )
    -- Condition 3: audit has exactly one row per ticket
    AND (
        (SELECT count(DISTINCT ticket_id) FROM support.audit)
        = (SELECT count(*) FROM support.tickets)
    )
    AND (
        (SELECT count(*) FROM support.audit)
        = (SELECT count(*) FROM support.tickets)
    )
    AS passed
FROM support.tickets t
JOIN support.customers c ON c.id = t.customer_id;

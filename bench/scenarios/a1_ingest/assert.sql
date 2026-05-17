-- a1_ingest assertion: verifies row counts, FK consistency, and specific fixture values.
-- Returns a single boolean column "passed".
SELECT
    -- Customers: exactly 5 rows
    (SELECT count(*) FROM ingest.customers) = 5
    -- Orders: exactly 6 rows
    AND (SELECT count(*) FROM ingest.orders) = 6
    -- Every order's customer_id has a matching customer
    AND NOT EXISTS (
        SELECT 1 FROM ingest.orders o
        WHERE NOT EXISTS (
            SELECT 1 FROM ingest.customers c WHERE c.id = o.customer_id
        )
    )
    -- Spot-check: customer id=1 is Alice Nguyen at alice@example.com in US
    AND EXISTS (
        SELECT 1 FROM ingest.customers
        WHERE id = 1
          AND name = 'Alice Nguyen'
          AND email = 'alice@example.com'
          AND country = 'US'
    )
    -- Spot-check: customer id=5 is Eva Rossi in IT
    AND EXISTS (
        SELECT 1 FROM ingest.customers
        WHERE id = 5
          AND email = 'eva@example.com'
          AND country = 'IT'
    )
    -- Spot-check: order 104 has amount 75.25 and status 'shipped'
    AND EXISTS (
        SELECT 1 FROM ingest.orders
        WHERE order_id = 104
          AND customer_id = 3
          AND amount = 75.25
          AND status = 'shipped'
    )
    -- Spot-check: order 105 has amount 200.00 and status 'completed'
    AND EXISTS (
        SELECT 1 FROM ingest.orders
        WHERE order_id = 105
          AND customer_id = 4
          AND amount = 200.00
          AND status = 'completed'
    )
AS passed;

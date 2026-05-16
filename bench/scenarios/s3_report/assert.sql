-- s3_report assertion: the recorded top_region matches the true argmax from orders.
-- Computes the real answer inline and compares it to what the agent stored.
SELECT (f.value = truth.region) AS passed
FROM sales.findings f,
     (
         SELECT region
         FROM sales.orders
         GROUP BY region
         ORDER BY SUM(amount) DESC
         LIMIT 1
     ) truth
WHERE f.metric = 'top_region';

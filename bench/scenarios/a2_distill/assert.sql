-- a2_distill assertion.
--
-- Passes when ALL of:
--   1. feedback.digest has exactly 8 rows.
--   2. Every sentiment value is one of (positive, negative, neutral).
--   3. At least 6 of the 8 items match the known-good sentiment labels
--      (loose grading: measures agent loop correctness, not perfect sentiment).
--
-- Known-good sentiment map (clear-cut labels from fixture text):
--   id 1 -> positive  (smooth onboarding, excellent docs)
--   id 2 -> positive  (rock solid, no outages)
--   id 3 -> negative  (confusing API, no useful errors)
--   id 4 -> negative  (billing support 2 weeks, frustrating)
--   id 5 -> positive  (clean dashboard, intuitive)
--   id 6 -> positive  (migration worked, no data loss)
--   id 7 -> negative  (irrelevant search, no filter)
--   id 8 -> neutral   (meets basic needs, nothing exceptional or broken)

WITH
expected AS (
    SELECT *
    FROM (VALUES
        (1, 'positive'),
        (2, 'positive'),
        (3, 'negative'),
        (4, 'negative'),
        (5, 'positive'),
        (6, 'positive'),
        (7, 'negative'),
        (8, 'neutral')
    ) AS t(id, expected_sentiment)
),
row_count_ok AS (
    SELECT (COUNT(*) = 8) AS ok FROM feedback.digest
),
labels_valid AS (
    SELECT (COUNT(*) = 0) AS ok
    FROM feedback.digest
    WHERE sentiment NOT IN ('positive', 'negative', 'neutral')
),
match_count AS (
    SELECT COUNT(*) AS matches
    FROM feedback.digest d
    JOIN expected e ON d.id = e.id AND d.sentiment = e.expected_sentiment
)
SELECT
    (SELECT ok FROM row_count_ok)
    AND (SELECT ok FROM labels_valid)
    AND (SELECT matches >= 6 FROM match_count)
    AS passed;

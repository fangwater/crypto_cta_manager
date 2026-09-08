-- Each distinct target vector owns an independent five-slice price schedule.
-- The fifth five-second bar ends 240 seconds after the first complete bar
-- entirely following the target's received timestamp.

ALTER TABLE cta_theoretical_nav_pending
    ADD COLUMN deltas jsonb NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(deltas) = 'object');

ALTER TABLE cta_theoretical_nav_events
    ADD COLUMN sample_mids jsonb NOT NULL DEFAULT '[]'::jsonb
    CHECK (jsonb_typeof(sample_mids) = 'array');

UPDATE cta_theoretical_nav_pending
SET window_end_us = (
    ((received_at_us + 4999999) / 5000000) * 5000000
    + 245000000
);

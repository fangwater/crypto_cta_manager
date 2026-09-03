-- Keep only target changes in the theoretical TWAP queue. Position strategies
-- may republish an unchanged target frequently, which must not grow pending
-- storage or shorten the effective execution window.

CREATE TABLE cta_theoretical_nav_latest_targets (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    binding_name text NOT NULL CHECK (length(binding_name) > 0),
    position_strategy_name text NOT NULL CHECK (length(position_strategy_name) > 0),
    venue text NOT NULL CHECK (length(venue) > 0),
    targets jsonb NOT NULL CHECK (jsonb_typeof(targets) = 'object'),
    received_at_us bigint NOT NULL CHECK (received_at_us > 0),
    update_seq bigint NOT NULL CHECK (update_seq BETWEEN 0 AND 4294967295),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, binding_name)
);

INSERT INTO cta_theoretical_nav_latest_targets (
    source_id, binding_name, position_strategy_name, venue, targets,
    received_at_us, update_seq
)
SELECT DISTINCT ON (source_id, binding_name)
    source_id, binding_name, position_strategy_name, venue, targets,
    received_at_us, update_seq
FROM cta_theoretical_nav_pending
ORDER BY source_id, binding_name, received_at_us DESC, update_seq DESC;

WITH ordered AS (
    SELECT
        source_id,
        binding_name,
        received_at_us,
        update_seq,
        venue,
        targets,
        lag(venue) OVER binding_updates AS previous_venue,
        lag(targets) OVER binding_updates AS previous_targets
    FROM cta_theoretical_nav_pending
    WINDOW binding_updates AS (
        PARTITION BY source_id, binding_name
        ORDER BY received_at_us, update_seq
    )
), duplicates AS (
    SELECT source_id, binding_name, received_at_us, update_seq
    FROM ordered
    WHERE venue = previous_venue AND targets = previous_targets
)
DELETE FROM cta_theoretical_nav_pending pending
USING duplicates
WHERE pending.source_id = duplicates.source_id
  AND pending.binding_name = duplicates.binding_name
  AND pending.received_at_us = duplicates.received_at_us
  AND pending.update_seq = duplicates.update_seq;

WITH pending_windows AS (
    SELECT
        source_id,
        binding_name,
        received_at_us,
        update_seq,
        lead(received_at_us) OVER (
            PARTITION BY source_id, binding_name
            ORDER BY received_at_us, update_seq
        ) AS next_received_at_us
    FROM cta_theoretical_nav_pending
)
UPDATE cta_theoretical_nav_pending pending
SET window_end_us = LEAST(
    pending.received_at_us + 300000000,
    COALESCE(
        pending_windows.next_received_at_us,
        pending.received_at_us + 300000000
    )
)
FROM pending_windows
WHERE pending.source_id = pending_windows.source_id
  AND pending.binding_name = pending_windows.binding_name
  AND pending.received_at_us = pending_windows.received_at_us
  AND pending.update_seq = pending_windows.update_seq;

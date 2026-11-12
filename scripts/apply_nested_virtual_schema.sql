-- Explicit additive maintenance; normal startup must never execute this file.
-- Back up the Manager database and pause configuration writes before applying.
BEGIN;
LOCK TABLE cta_virtual_accounts IN SHARE ROW EXCLUSIVE MODE;
CREATE TABLE cta_virtual_account_children (
    parent_virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id) ON DELETE CASCADE,
    child_virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id),
    multiplier double precision NOT NULL CHECK (multiplier >= 0 AND multiplier < 'Infinity'::double precision),
    PRIMARY KEY (parent_virtual_id, child_virtual_id),
    CHECK (parent_virtual_id <> child_virtual_id)
);
CREATE INDEX cta_virtual_account_children_child_idx ON cta_virtual_account_children(child_virtual_id);
COMMIT;

# Virtual accounts and following

Virtual accounts contain strategy bindings only, with no exchange credentials, Exec, RocksDB, positions, or NAV. Administrators maintain them in the independent Virtual 账户管理 workspace at `/manager/virtual/`, reachable from the main navigation, workspace overview, and strategy configuration. The workspace lists virtual accounts, strategy combinations, follower multipliers, and pending publication status. Follower identities and statuses are filtered by real account view grants. Each real account can use independent configuration or follow exactly one virtual account with a finite nonnegative multiplier. Effective shares are virtual shares × multiplier. Template edits synchronize the real catalog transactionally and queue immediate runtime publication. Position strategy publications use the usual account archive and publishing paths. Order template edits also queue followers.

Removing a template binding or changing follow targets sets obsolete real bindings to zero and publishes the complete zero target under the original strategy name. The stopped bindings remain for retry and attribution. A binding name cannot be reassigned to another position strategy. Switching back to independent preserves the effective configuration; pending deliveries finish before independent edits are accepted. A followed virtual account cannot be deleted. Multiplier zero stops all its strategies. Follow mode disables manual binding and direct Exec order-parameter edits, but permits manual republishing after pending configuration deliveries complete. An explicit follow-mode save republishes the full effective configuration, including retained zero stops.

Only admins write virtual accounts. Real account configuration uses existing account configure grants, and attaching a template requires configure rights for every included strategy. Virtual account listings are filtered by strategy visibility. Venue mismatches and experimental-algorithm activation without the existing token are rejected before configuration changes.

Delivery status appears in the account editor. Failures remain durable across Manager restarts and retry every ten seconds. Archive writes precede runtime publishes, freezing effective shares and current theoretical fees. Multiple pending changes coalesce to the newest desired configuration. No startup DDL, Exec RocksDB writes, or trading-service changes.

## API

- `GET /api/catalog/virtual-accounts` lists visible templates.
- `PUT /api/catalog/virtual-accounts/{virtual_id}` replaces `{name, bindings: [{binding_name, position_strategy_name, order_strategy_name, shares}]}`.
- `DELETE /api/catalog/virtual-accounts/{virtual_id}` deletes an unfollowed template.
- `PUT /api/catalog/accounts/{source_id}/configuration` accepts `{"mode":"follow","virtual_id":"model","multiplier":2}` or `{"mode":"independent"}`.
- Existing account studio responses add `configuration` and `pending_publishes`.

## Existing database maintenance

The SQL below is explicit additive maintenance for an existing Manager database. Review, back up the target database, and apply once within a transaction before starting the updated binary. Fresh databases use migrations/schema.sql.

On 2026-10-09 UTC this maintenance was applied to jp-meta after a complete PostgreSQL backup, in one transaction. Release `20261009T043909Z` (Manager `5a05f28`) enabled the virtual-account workspace. Authenticated empty-template creation, reading and deletion passed; no real account was switched to follow mode. The initial follow and publication-queue counts were both zero. Backup and verification evidence are retained under `/home/ubuntu/crypto_cta_manager/backups/combined-20261009T043632Z`. This schema maintenance did not change Exec RocksDB or historical catalog data. The final joint release is `20261009T053323Z` (`1b285b5`); see [trade03 deployment verification](jp_meta_bnb_trade03_20261009.md#联合发布最终验证).

```sql
BEGIN;

-- Virtual accounts own configuration only; they are never Exec order sources.
CREATE TABLE cta_virtual_accounts (
    virtual_id text PRIMARY KEY,
    name text NOT NULL CHECK (length(btrim(name)) > 0 AND octet_length(name) <= 200),
    updated_at_us bigint NOT NULL
);
CREATE TABLE cta_virtual_account_bindings (
    virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id) ON DELETE CASCADE,
    binding_name text NOT NULL,
    position_strategy_name text NOT NULL REFERENCES cta_position_strategies(strategy_name),
    order_strategy_name text NOT NULL REFERENCES cta_order_strategies(strategy_name),
    shares double precision NOT NULL CHECK (shares >= 0 AND shares < 'Infinity'::double precision),
    PRIMARY KEY (virtual_id, binding_name)
);
CREATE TABLE cta_account_follows (
    source_id text PRIMARY KEY REFERENCES cta_order_sources(source_id),
    virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id),
    multiplier double precision NOT NULL CHECK (multiplier >= 0 AND multiplier < 'Infinity'::double precision),
    updated_at_us bigint NOT NULL
);
CREATE INDEX cta_account_follows_virtual_id_idx ON cta_account_follows(virtual_id);
-- Durable delivery status, not a second order/target history.
CREATE TABLE cta_follow_publish_queue (
    source_id text NOT NULL,
    binding_name text NOT NULL,
    revision bigint NOT NULL,
    archived boolean NOT NULL DEFAULT false,
    error text,
    PRIMARY KEY (source_id, binding_name),
    FOREIGN KEY (source_id, binding_name) REFERENCES cta_account_strategy_bindings(source_id, binding_name) ON DELETE CASCADE
);
COMMIT;
```

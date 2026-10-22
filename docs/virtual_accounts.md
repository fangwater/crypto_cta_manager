# Virtual accounts and following

Virtual accounts contain strategy bindings only, with no exchange credentials, Exec, RocksDB, positions, or NAV. Administrators maintain them in the independent Virtual 账户管理 workspace at `/manager/virtual/`, reachable from the main navigation, workspace overview, and strategy configuration. The workspace lists virtual accounts, strategy combinations, follower multipliers, and pending publication status. Follower identities and statuses are filtered by real account view grants. Each real account can use independent configuration or follow multiple virtual accounts, each with a finite nonnegative multiplier. Effective shares for each publication binding are the sum of virtual shares × that virtual's multiplier. Matching binding names must reference the same position strategy and order template; conflicts reject the whole change, including template edits. Different publication names remain separate strategy identities even when they use the same position template. Template edits synchronize the real catalog transactionally and queue immediate runtime publication. Position strategy publications use the usual account archive and publishing paths. Order template edits also queue followers.

Removing a template binding or a followed virtual recomputes all remaining contributions and sets only obsolete real bindings to zero and publishes the complete zero target under the original strategy name. The stopped bindings remain for retry and attribution. A binding name cannot be reassigned to another position strategy. Switching back to independent preserves the effective configuration; pending deliveries finish before independent edits are accepted. A followed virtual account cannot be deleted. Multiplier zero removes that virtual's contribution; contributions from other followed virtuals remain. Empty follow lists and duplicate virtual IDs are rejected; switching to independent remains an explicit action. Follow mode disables manual binding and direct Exec order-parameter edits, but permits manual republishing after pending configuration deliveries complete. An explicit follow-mode save republishes the full effective configuration, including retained zero stops.

New virtual accounts receive an automatic number (`virtual01`, `virtual02`, and so on) and an editable display alias. Numbers are allocated in the same PostgreSQL transaction as account creation, under the shared configuration lock, so concurrent creations cannot overwrite one another. Existing account IDs and follow relationships remain unchanged when aliases are edited.

The workspace opens the first existing account by default. The account list shows the fixed number and alias on separate lines, while the editor separates identity, strategy cards, save/reset actions, and follower status. Each strategy card keeps its name above the execution-template and shares controls; long names wrap and narrow screens stack the controls. Adding a strategy requires an explicit Add action. Refreshing follower status preserves unsaved edits, and Reset restores the selected account. Accounts with followers show why deletion is unavailable.

Admins, virtual creators and delegated managers may edit virtual accounts. Only admins and creators manage delegation. Real account configuration uses existing account configure grants, and attaching a template requires configure rights for every included strategy. Virtual account listings are filtered by strategy visibility. Creating, editing, or following a virtual account uses these existing permissions without a separate experimental-algorithm token. Venue and reserved-hedge target checks still run before configuration changes.

Delivery status appears in the account editor. Failures remain durable across Manager restarts and retry every ten seconds. Archive writes precede runtime publishes, freezing effective shares and current theoretical fees. Multiple pending changes coalesce to the newest desired configuration. No startup DDL, Exec RocksDB writes, or trading-service changes.

## API

2026-10-11 jp-meta deployment: `virtual02`, alias `lgq_cta`, owns five one-share
bindings (`lgq_cta_factor_036`, `lgq_cta_factor_051`, `lgq_cta_factor_170`,
`lgq_cta_TD_TI_002`, `lgq_cta_baseline_004`) using order template
`lgq_cta_ctaker`. Real `binance_exec_trade05` follows it with multiplier 1 and
uses RX02 RapidX credentials held only on the host. RX03 is stopped. The signal
publisher posts position strategies through the existing :4191 Manager gateway.
Since 2026-10-12 each enabled symbol/factor pair uses its own 200 USDT entry
allocation; factors keep independent targets and PnL attribution. Existing
positions retain their entry quantity until exit.

`ctaker` belongs to BatchExec and submits the entire executable signed delta as
a MARKET order, with zero USDT target tolerance. Exit target 0 closes executed
strategy qty. Manager owns order parameters and the complete market-rule cache.
RapidX private rule requests bind the source's trading IP; native exchange
account-mode checks do not apply to a RapidX sub-portfolio. Exec retains account
risk checks, venue rules, and its usual net execution and position ledger.

- `GET /api/catalog/virtual-accounts` lists visible templates.
- `POST /api/catalog/virtual-accounts` creates `{name, bindings}` and returns the account with its automatic number.
- `PUT /api/catalog/virtual-accounts/{virtual_id}` replaces `{name, bindings: [{binding_name, position_strategy_name, order_strategy_name, shares}]}`.
- `DELETE /api/catalog/virtual-accounts/{virtual_id}` deletes an unfollowed template.
- `PUT /api/catalog/accounts/{source_id}/configuration` accepts `{"mode":"follow","follows":[{"virtual_id":"virtual01","multiplier":2},{"virtual_id":"virtual02","multiplier":0.5}]}` or `{"mode":"independent"}`.
- Existing account studio responses add `configuration` and `pending_publishes`.

## Existing database maintenance

For multiple follows, apply `scripts/apply_multiple_virtual_follows_schema.sql`
explicitly after a backup, with configuration writes paused during the coordinated
backend/frontend replacement. It changes only the follow primary key from
`source_id` to `(source_id, virtual_id)`; existing multipliers, bindings, pending
publications and historical records are preserved. Fresh databases already use
this key in `migrations/schema.sql`. Normal startup never applies maintenance.
The API has one current `follows` array format; upgrade frontend and backend
together. A single existing follow appears as one row. Do not roll back to an
older single-follow binary after adding a second follow without first restoring
one follow per account and reviewing effective targets.

For example, virtual01 contributes 2 shares × 3 and virtual02 contributes
5 shares × 0.5 to the same binding: the account publishes 8.5 shares once.
Pausing virtual01 leaves 2.5 shares. Editing either template recalculates both;
order-template updates queue each effective binding once. The account form
supports adding/removing rows and changing each multiplier, and background
status refreshes preserve unsaved rows.


The 2026-10-11 deployment also applied the current owner/manager-grant schema
explicitly after a full PostgreSQL backup. Existing virtual accounts retained
their IDs and bindings. The order-strategy algorithm constraint now accepts
`ctaker` alongside `batch`, `pov`, and `chase`. Existing schema maintenance,
including the owner foreign key's `ON DELETE SET NULL`, must match
`migrations/schema.sql`; the service does not run DDL at startup.

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
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id),
    multiplier double precision NOT NULL CHECK (multiplier >= 0 AND multiplier < 'Infinity'::double precision),
    updated_at_us bigint NOT NULL,
    PRIMARY KEY (source_id, virtual_id)
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

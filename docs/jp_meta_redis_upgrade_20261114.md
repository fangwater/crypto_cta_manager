# jp-meta Redis upgrade attempt — 2026-11-14 UTC

The operator authorized upgrading the shared Redis 7.0.15 to 7.4.12 to
support `CLIENT SETINFO`. The first cutover failed and was restored to 7.0.15.
At the operator's explicit direction, a second cutover succeeded at 06:51 UTC,
while Manager was still rebuilding its cold report.

Redis 7.4.12 with jemalloc 5.3.0 was built locally from the official release.
An isolated 7.0/7.4 rehearsal loaded the same RDB: all 374 keys and values
matched. A copied production AOF loaded successfully in 7.4; `CLIENT SETINFO`
succeeded, and the existing ACL still rejected `CONFIG GET`.

The production Redis stopped normally at 06:41:49. The 7.4 service failed
with systemd `203/EXEC`: the unit has `NoExecPaths=/` and permits execution
only from its listed paths, which did not include the new binary. The new
process never loaded production data. The 7.0 service was restored at 06:42:12,
using the original AOF. The Redis process was interrupted for approximately
23 seconds. A copy of the stopped AOF, RDB, configuration, original binary,
unit and process baseline is under host-only
`/home/ubuntu/crypto_cta_manager/backups/redis-7412-20261114/`.

After Redis returned, Manager continued to time out on bound-account target
publishes; the gateway recorded 499 responses. Manager was restarted at
06:43:35 to restore publication, triggering its ordinary cold NAV rebuild.
This exposed a separate Redis-disconnect recovery problem. The service's
Redis connection mutex and reconnect/keepalive paths need further investigation;
the precise cause is not yet established. No Exec trading process was restarted
for this attempt. The 7.4 test instances were stopped.

For the second attempt, a systemd drop-in added the exact 7.4 executable to
`ExecPaths`, and an isolated systemd execution test succeeded. A fresh stopped
7.0 AOF and RDB are in `pre-upgrade-second/` in the same host-only directory.
7.4.12 started as PID 4100946, loaded the AOF, returned PONG, exposed 374 keys,
retained the forbidden `CONFIG GET` ACL and CPU 28, and accepted `CLIENT SETINFO`.
The binary has jemalloc 5.3.0 but no libsystemd support. After verifying the
process was ready, readiness was notified to systemd and the persistent unit
was changed to `Type=simple` with `--supervised no` for future starts. The
Redis executable hash is
`943a182a12290aa906c84b8bb9096df3958e0b84458f3b38696e8d4acc27e391`.
Do not restore the old AOF blindly after 7.4 has accepted new writes: doing so
would discard those writes.

Manager's API became available at 07:00:42 and `rbf_small` immediately
resumed 200 responses and confirmed publication to both trade03 and trade04.
The first failed Redis cutover began disrupting bound-account publishes at
06:41:50; the Manager restart and cold rebuild extended the outage until
07:00:42. The startup rebuild duration was substantially longer than the
previous startup. At 07:01 Redis AOF writes were healthy, the pre-existing
`CLIENT SETINFO` error stopped recurring, and all 94 sampled trading,
persistence and Delist processes retained their process identities. The
executable is installed separately from Ubuntu's Redis package and requires
explicit maintenance for future Redis security updates. `Type=simple` does not
provide the prior systemd readiness notification on future starts.

## Follow-up investigation and local repair

At 07:27 UTC the Redis error counters were still `ERR=1`, `NOPERM=1`, unchanged
since the upgrade. The NOPERM is the explicit `CONFIG GET` permission test;
the ERR counter's historical command is not available. There were no new slow
AOF fsync notices, and `aof_delayed_fsync=0`. Manager had completed 3,675 target
requests since reopening, all status 200, including 524 for `rbf_small`.

Local regression tests reproduced two definite timeout gaps in the old code:
waiting for the Redis mutex and a stalled connection handshake both escaped
the configured request deadline. Heartbeat PING also had no enclosing timeout
while holding that mutex. These explain a way publication can remain blocked;
there is no stack capture proving the precise wait involved in this incident.

The local repair bounds lock acquisition, connection setup, and command/readback
by one deadline, bounds heartbeat, and clears a failed connection before releasing
the guard. Redis-rs connection/response timeouts are explicit and hidden connection
creation retries are disabled. Sixteen Redis-module tests passed against an
isolated Redis 7.4, including lock contention, a silent handshake, paused Redis
responses, a server restart followed by latest-target publication, and existing
market/source-isolation tests. `cargo check` and formatting passed. This repair
has not been deployed and no production process was restarted during this
follow-up investigation.

The startup repair now loads and validates account modes, sessions, permissions,
targets, order templates, bindings and fees before accepting target requests,
then builds NAV asynchronously. Analysis APIs expose loading/error until the
first complete build and never present an empty/partial zero NAV as a report.
Snapshot/allocation integrity checks and archive drains remain. Dependent
analysis workers and financial automation still wait for a complete report.
Fee edits notify the background worker without waiting for a long rebuild under
the publication configuration lock. Cold loading, initial build failure/retry,
last-good retention and publication with a cold cache are covered by local tests.

#!/usr/bin/env bash
exec /usr/bin/timeout --signal=TERM --kill-after=2s 8s /usr/local/bin/pmdaemon "$@"

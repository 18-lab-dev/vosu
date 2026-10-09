#!/bin/sh
# Container filesystems and restricted hosts may refuse capabilities; installing
# the dashboard must still succeed so users can retry setup on the actual host.
if ! /usr/bin/vosu --setup; then
    printf '%s\n' 'vosu: capability setup failed; run sudo vosu --setup on the host.' >&2
fi
exit 0

#!/bin/sh
# docs/specs/setup-wizard.md section 2.2: finds the host's Docker socket by itself, so the one compose file
# (docker/compose.yaml) needs no DOCKER_SOCK or DOCKER_GID. The compose file mounts the host's /var/run read-only at
# /host-run; each start this picks the socket in it and its group, then drops from root to uid 1000 (the agent's `node`,
# so the shared data volume stays readable to both) with that group, and runs the runner.
#
#   Docker Desktop on WSL: docker.sock.raw (root, group 0), there from the moment Docker is up.
#   A Linux server:        docker.sock (root:docker).
#
# Started as a non-root user (docker/ci/compose.yaml, which mounts the socket itself) it does nothing but run the command.
# DOCKER_SOCKET_DRY_RUN=1 prints `socket=<path> gid=<n>` and exits, which is what the tests run.
set -eu

HOST_RUN="${HOST_RUN:-/host-run}"

sock=""
for candidate in "$HOST_RUN/docker.sock.raw" "$HOST_RUN/docker.sock"; do
  if [ -S "$candidate" ]; then
    sock="$candidate"
    break
  fi
done
gid=""
if [ -n "$sock" ]; then
  gid="$(stat -c %g "$sock")"
fi

if [ -n "${DOCKER_SOCKET_DRY_RUN:-}" ]; then
  echo "socket=${sock:-none} gid=${gid:-none}"
  exit 0
fi

if [ "$(id -u)" != 0 ]; then
  exec "$@"
fi

if [ -z "$sock" ]; then
  echo "retaskable-ci: no Docker socket in $HOST_RUN (looked for docker.sock.raw and docker.sock). Start Docker Desktop, or on Linux check that /var/run/docker.sock exists. The runner starts anyway and says 'Docker missing'." >&2
  exec env HOME=/home/ci setpriv --reuid 1000 --regid 1000 --clear-groups "$@"
fi

ln -sf "$sock" /var/run/docker.sock
echo "retaskable-ci: using the Docker socket $sock (group $gid)." >&2
exec env HOME=/home/ci setpriv --reuid 1000 --regid 1000 --groups "$gid" "$@"

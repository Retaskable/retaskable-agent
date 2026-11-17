#!/bin/sh
# docs/specs/docker.md section 3.2: each start sets git's identity up from the environment, then runs the
# agent. Unlike the TypeScript agent's container, there is no checkout to update or `npm ci` to
# run first: the agent's own code is this image's compiled `retaskable` binary, baked in at build
# time (section 2); a newer agent means rebuilding the image.
#
# No token is read here: a saved GH_TOKEN (`retaskable secret set GH_TOKEN`) is put in the agent's
# environment and `gh auth setup-git` run by the agent itself at start (`Secrets::apply`), so git over
# https signs in with it, and the children it starts inherit it.
set -eu

if [ -n "${GIT_USER_NAME:-}" ]; then
  git config --global user.name "$GIT_USER_NAME"
fi
if [ -n "${GIT_USER_EMAIL:-}" ]; then
  git config --global user.email "$GIT_USER_EMAIL"
fi

exec retaskable

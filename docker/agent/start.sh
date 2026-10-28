#!/bin/sh
# docs/specs/docker.md section 3.2: each start sets git up from the environment, then runs the
# agent. Unlike the TypeScript agent's container, there is no checkout to update or `npm ci` to
# run first: the agent's own code is this image's compiled `retaskable` binary, baked in at build
# time (section 2); a newer agent means rebuilding the image.
set -eu

if [ -n "${GH_TOKEN:-}" ]; then
  gh auth setup-git
fi
if [ -n "${GIT_USER_NAME:-}" ]; then
  git config --global user.name "$GIT_USER_NAME"
fi
if [ -n "${GIT_USER_EMAIL:-}" ]; then
  git config --global user.email "$GIT_USER_EMAIL"
fi

exec retaskable

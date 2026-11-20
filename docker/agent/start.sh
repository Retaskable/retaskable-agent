#!/bin/sh
# docs/specs/docker.md section 3.2: runs the agent. There is no checkout to update or `npm ci` to run first: the agent's own
# code is this image's compiled `retaskable` binary, baked in at build time (section 2); a newer agent means a newer image.
#
# Nothing is read from the environment here: not a token (a saved GH_TOKEN is put in the agent's environment and
# `gh auth setup-git` run by the agent itself, `Secrets::apply`), and not git's identity, which the agent sets from the
# linked GitHub account where git has none (docs/specs/setup-wizard.md section 2.3).
set -eu

exec retaskable

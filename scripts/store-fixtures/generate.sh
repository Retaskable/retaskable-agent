#!/bin/sh
# Regenerates tests/fixtures/store/answers.json from a Retaskable.com checkout (RETASKABLE_COM_DIR, after `npm ci --ignore-scripts` there).
set -eu
here=$(cd "$(dirname "$0")/../.." && pwd)
: "${RETASKABLE_COM_DIR:?set RETASKABLE_COM_DIR to a Retaskable.com checkout}"
cd "$RETASKABLE_COM_DIR"
node --import tsx "$here/scripts/store-fixtures/generate.mts" "$here/tests/fixtures/store/answers.json"

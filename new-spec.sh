#!/usr/bin/env sh
# Generate a spec (requirements → design → tasks, gated in order) from a
# description file.
#
# Usage: ./new-spec.sh <spec-name> <description-file>
#        ./new-spec.sh <spec-name> -            # read the description from stdin
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <spec-name> <description-file>" >&2
  exit 2
fi

name="$1"
brief="$2"

if [ "$brief" != "-" ] && [ ! -f "$brief" ]; then
  echo "error: description file not found: $brief" >&2
  exit 1
fi

exec harness spec new "$name" --from "$brief"

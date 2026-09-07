#!/bin/sh
# owned-by?: the file at $2 belongs to the user named $1.
owner=$(stat -f %Su "$2" 2>/dev/null) || exit 2
if [ "$owner" = "$1" ]; then
  printf '{"holds": true, "reason": "%s owns %s"}\n' "$1" "$2"
else
  printf '{"holds": false, "reason": "%s owns %s"}\n' "$owner" "$2"
fi

#!/bin/sh
# on-main?: the checked-out branch is main or master.
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || exit 3
case "$branch" in
  main|master) printf '{"holds": true, "reason": "on %s"}\n' "$branch" ;;
  *)           printf '{"holds": false, "reason": "on %s"}\n' "$branch" ;;
esac

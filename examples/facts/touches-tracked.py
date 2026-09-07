#!/usr/bin/env python3
"""touches-tracked?: the command's redirect targets are all tracked by git."""
import json, subprocess, sys

call = json.load(sys.stdin)
bash = call["subject"].get("bash")
if bash is None or bash["parse_error"]:
    print("not a parsed Bash call", file=sys.stderr)
    sys.exit(3)

targets = [
    r["target"]["literal"]
    for command in bash["commands"]
    for r in command["redirects"]
    if "literal" in r["target"]
]
untracked = []
for path in targets:
    status = subprocess.run(["git", "ls-files", "--error-unmatch", path],
                            capture_output=True, cwd=call["cwd"])
    if status.returncode != 0:
        untracked.append(path)

print(json.dumps({"holds": not untracked,
                  "reason": "untracked: " + ", ".join(untracked) if untracked else "all tracked"}))

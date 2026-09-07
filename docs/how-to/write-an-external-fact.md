# Write an external fact

A fact is a program the guard runs to learn one thing about a call: whether a proposition holds. You declare it in your rules file, name it in a condition, and the guard runs it when a rule needs the answer. Any language works. The contract is one JSON object in, one JSON object out.

## The contract

The guard starts your program with:

- argv: the arguments in the declaration, then the arguments the condition passed, in order;
- environment: your own, plus `CLAUDE_GUARD_CWD`, `CLAUDE_GUARD_SESSION_ID`, `CLAUDE_GUARD_TOOL`, and `CLAUDE_GUARD_PROTOCOL=1`;
- working directory: the call's `cwd`;
- stdin: one JSON object, then end of file.

```json
{"protocol": 1, "cwd": "/Users/me/proj", "session_id": "abc123", "tool": "Bash",
 "subject": {"bash": {"command": "git push", "commands": [...], "elaborated": [...], "uninspected": [], "parse_error": null}},
 "args": ["origin"]}
```

`subject` is what the rules saw, keyed by tool, the same value the log record carries; [the log format](../reference/log-format.md#subject) lists its shapes. A program that does not need it can ignore stdin.

Your program writes one object to stdout and exits 0:

```json
{"holds": true, "reason": "on main"}
```

`holds` is required. `reason` is optional and becomes evidence: it is appended to the decision's text in parentheses and stored in the log. Fields the guard does not know are ignored, so a program may print more for its own purposes.

Anything else is unknown: a non-zero exit, a timeout, a program that cannot start, or stdout that is not that object. An unknown carries a reason the guard composes, `on-main? is unknown: exited with status 3`, and a rule whose pattern matched on an unknown asks rather than deciding. So when your program cannot answer, exit non-zero and say why on stderr. Do not print a guess.

## Declare it

```scheme
(fact on-main? (exec "/Users/me/.config/claude-guard/facts/on-main.sh")
  :lifetime fresh :timeout "1s")
```

The name ends in `?`. The program path is not searched for under `~`; write it out, or name a program on `PATH`. Arguments after the program are passed first on every run:

```scheme
(fact branch-is? (exec "/Users/me/.config/claude-guard/facts/branch-is.sh" "--quiet")
  :lifetime fresh :timeout "1s")
```

`:lifetime fresh` and `:timeout` are both required. There are no defaults, on purpose: a default chosen now and changed later would silently change what your file means. Once your log has timings, `ms` on each `facts` entry, pick a timeout from them.

The declaration may sit anywhere in the file; facts are read before rules. A fact must be declared in the file whose rules name it.

## Pass arguments from the rule

A condition may pass strings and binders:

```scheme
(rule ownership
  (deny [cp ... ?dst] :when (owned-by? "root" ?dst)
    :reason "the target belongs to root."
    :instead "copy somewhere you own, or ask."))
```

`owned-by?` runs with argv `["root", "/etc/hosts"]` for `cp a /etc/hosts`. When a pattern matches in more than one way, the condition is tried under each binding set until one holds, so the program may be asked several times with different arguments in one call. It is asked once per distinct argument list; the answer is memoized for the rest of the call.

## A shell script

Shell is enough for a fact that wraps one command. This one and the Python fact below are in the repository under `examples/facts/`, with a rules file that uses them and a test that runs them:

```sh
#!/bin/sh
# owned-by?: the file at $2 belongs to the user named $1.
owner=$(stat -f %Su "$2" 2>/dev/null) || exit 2
if [ "$owner" = "$1" ]; then
  printf '{"holds": true, "reason": "%s owns %s"}\n' "$1" "$2"
else
  printf '{"holds": false, "reason": "%s owns %s"}\n' "$owner" "$2"
fi
```

On Linux, `stat -c %U`. Build the JSON with `printf` and keep the values simple; a path with a double quote in it would break this, and a real script should escape it or use a JSON tool.

## A Python script

Python reads the whole call from stdin and can look at the subject:

```python
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
```

The subject's `commands` are the words after quote removal, each literal or dynamic; a dynamic word such as `$OUT` has no text the program can act on, and this script skips it. The `elaborated` field has the same commands with options and their values classified, when you need that.

A warn row makes a fact like this advisory rather than blocking, and `not` turns "all tracked" into "something is not":

```scheme
(rule redirects
  (warn [... > ?out] :when (not (touches-tracked?))
    :reason "this writes a file git does not track."))
```

The model then sees `claude-guard noted `cargo build > out.txt`: this writes a file git does not track. (untracked: out.txt)` and the command runs.

## Run it by hand

```
claude-guard extern owned-by? root /etc/hosts
```

runs the fact the way the hook would, from the current directory with no session, and prints what it answered and how long it took. The exit code is the answer: 0 holds, 1 fails, 2 unknown. To run it against a real call, pipe a hook payload in:

```
echo '{"session_id":"t","cwd":"/Users/me/proj","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo build > out.txt"},"tool_use_id":"t1"}' \
  | claude-guard extern touches-tracked?
```

## Debug it

Stderr passes through, both under `extern` and under the hook, so print to it freely while developing. Under the hook, Claude Code shows the hook's stderr in its debug output.

An unknown's reason says which failure it was: `could not start`, `exited with status N`, `timed out after 1s`, or `stdout was not {"holds": bool, "reason"?: string}` followed by what stdout held. The last one is the usual first bug: a stray `echo` before the JSON.

A program that runs past its timeout is killed, together with every process it started, so a stuck `sleep` or a hung network call does not hold the session. If a fact is slow by nature, give it the timeout it needs and expect the guard to wait that long on every call that asks it.

## What a fact cannot do

A fact returns a truth and a reason, never a decision. It cannot say deny or ask; the rule that names it does. It cannot return a value for a rule to compare; put the comparison in the program and return whether it held. And it runs on the guard's side of the hook, so it cannot see the model, only the call.

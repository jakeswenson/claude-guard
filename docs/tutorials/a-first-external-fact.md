# A first external fact

By the end of this page the guard asks a script of yours before it decides, you have watched the script's answer change a decision, seen what happens when the script cannot answer, and read the log line that records all of it. Allow fifteen minutes. It assumes [getting started](getting-started.md): the hooks are wired and `~/.config/claude-guard/rules.scm` is your rules file.

The fact we build is `on-main?`: does the checked-out git branch carry the name `main` or `master`? A rule then asks before any `git push` while it holds. The same shape fits any question a program can turn into a yes or a no.

## 1. Write the script

A fact is a program that reads one JSON object on stdin and writes one on stdout. Ours needs nothing from stdin; it asks git.

```
mkdir -p ~/.config/claude-guard/facts
```

Save this as `~/.config/claude-guard/facts/on-main.sh` and make it executable with `chmod +x`. It is also in the repository as `examples/facts/on-main.sh`, where a test runs every step of this page.

```sh
#!/bin/sh
# on-main?: the checked-out branch is main or master.
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || exit 3
case "$branch" in
  main|master) printf '{"holds": true, "reason": "on %s"}\n' "$branch" ;;
  *)           printf '{"holds": false, "reason": "on %s"}\n' "$branch" ;;
esac
```

Three things to notice. The answer is one object with a `holds` boolean. The `reason` is optional and becomes evidence the model sees. And when git cannot say, outside a repository, the script exits 3 rather than guessing; the guard treats any non-zero exit as unknown, and an unknown is never a silent yes or no.

## 2. Run it by hand

From inside any git repository:

```
echo '{}' | ~/.config/claude-guard/facts/on-main.sh
```

prints `{"holds": true, "reason": "on main"}` or the `false` form with your branch's name. From a directory that is not a repository it prints nothing and exits 3. Check with `echo $?`.

## 3. Declare it

Open `~/.config/claude-guard/rules.scm` and add, anywhere in the file:

```scheme
(fact on-main? (exec "/Users/you/.config/claude-guard/facts/on-main.sh")
  :lifetime fresh
  :timeout "1s")
```

Use your real home directory; the guard does not expand `~`. Both keywords are required and have no defaults. `fresh` means the fact is asked on every call that needs it. `"1s"` is how long the script may take before the guard gives up on it.

Check the file loads:

```
claude-guard rules
```

A mistake in the declaration prints its line and column. A name that does not end in `?`, a missing `:timeout`, or a `:lifetime` other than `fresh` are the ones to expect.

## 4. Ask it the way the hook would

```
cd ~/some/git/repo
claude-guard extern on-main?
```

prints one line and exits with the answer:

```
on-main? holds: on main (9ms)
```

Exit 0 is holds, 1 is fails, 2 is unknown. Try it from a directory with no repository:

```
on-main? is unknown: exited with status 3 (4ms)
```

That is the script's exit 3 arriving as an unknown, with the fact's name in front so the reader knows which fact could not answer.

## 5. Write the rule

Add to the rules file:

```scheme
(rule pushes
  (ask [git -... push ...] :when (on-main?)
    :reason "this pushes the main branch."
    :instead "push a feature branch and open a pull request."))
```

`(on-main?)` is a condition like `(ancestor-has? ".jj")`. It may sit on a row, as here, or on the rule after its name, where it gates every row.

## 6. Watch it decide

Feed the hook a call by hand, from a repository on `main`:

```
echo '{"session_id":"t","cwd":"'"$PWD"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push origin main"},"tool_use_id":"t1"}' \
  | claude-guard hook
```

The decision on stdout is an ask, and the reason ends with the script's evidence in parentheses:

```
claude-guard asks about `git push origin main`: this pushes the main branch. Instead: push a feature branch and open a pull request. (on main)
```

Switch to another branch and run the same line: the fact fails, the row does not fire, and stdout is empty. The command passes.

Now run it with `cwd` set to a directory that is not a repository. The pattern matched, the condition could not be settled, and the guard asks rather than guesses:

```
claude-guard asks about `git push origin main`: this pushes the main branch. Instead: push a feature branch and open a pull request. (on-main? is unknown: exited with status 3)
```

In a Claude Code session the model sees that text and can remove the ambiguity itself, by running from the repository root, before you are asked.

## 7. Read the log line

```
ls -t ~/.local/state/claude-guard/sessions/ | head -1
```

Open that file. The line for the ask has a `facts` array:

```json
"facts": [{"name": "on-main?", "args": [], "truth": "true", "reason": "on main", "ms": 9}]
```

Every fact the evaluation asked is there, in order, with what it answered and how long it took. When a fact is slow, this is where you find out.

## Where to go next

- [Write an external fact](../how-to/write-an-external-fact.md) covers arguments, stdin, Python, and debugging with stderr.
- [The rule language](../reference/rule-language.md#fact-declarations) has the full protocol.
- [External facts](../explanation/external-facts.md) explains why a fact adds a relation and never a verdict, and why an unknown asks.

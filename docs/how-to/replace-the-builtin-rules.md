# Replace the built-in rules

The binary carries a rule file. When `~/.config/claude-guard/rules.scm` exists, it replaces the built-in file whole. There is no merging, so one file is the truth for why a call was stopped.

## Start from the built-in file

```
mkdir -p ~/.config/claude-guard
claude-guard rules --export > ~/.config/claude-guard/rules.scm
```

The export is the built-in file verbatim, comments included. Edit it: delete the rules you do not want, change reasons, add your own. Then confirm it loads:

```
claude-guard rules
```

The output names your file and counts its rules and rows. A file that does not load prints every problem as `path:line:col: message`, and the guard fails open, recording each call as observed without deciding, until the file is fixed.

## Keep it in dotfiles

The file is plain text and belongs with your dotfiles. The guard reads `$XDG_CONFIG_HOME/claude-guard/rules.scm` when that variable is set, else `~/.config/claude-guard/rules.scm`.

Command declarations are separate files under `~/.config/claude-guard/commands/`, one per program, and they merge rather than replace: the built-in set, then any `(command ...)` forms in your rules file, then the directory, later winning by name.

## Use a different file for one run

`CLAUDE_GUARD_RULES=path` makes the guard use that file, and a missing file is an error rather than a fallback. `CLAUDE_GUARD_COMMANDS_DIR=dir` does the same for declarations. Both suit trying a change before it lands in your config.

## Go back to the built-in rules

Delete or rename `~/.config/claude-guard/rules.scm`. The next call loads the embedded file again.

# Declare a command

A declaration tells the guard which options a program takes, which of them take a value, and whether the program carries another command inside it. With it, `[git -... stash ...]` matches `git -C . stash`, a literal `-f` finds itself inside `-fxd`, and a rule about `sed` holds for `sudo sed`.

Without a declaration a program still matches, one word at a time: every dash word is an option, and a flag's value looks like an argument.

## Generate one from carapace

If [carapace](https://github.com/carapace-sh/carapace-bin) is installed:

```
claude-guard commands add git jj cargo
```

Each name becomes `~/.config/claude-guard/commands/<name>.scm`, generated from `carapace <name> export`. The command reports how many options and subcommands it wrote, and refuses to overwrite an existing file unless you pass `--force`. `--stdout` prints instead of writing.

Not sure what to declare? `claude-guard commands` lists every program your sessions have run, with a `carapace` column, sorted so the programs where a declaration would change a decision come first. See [review what the guard is doing](review-the-guard.md).

After adding declarations, run `claude-guard elaborate --check-log`. It re-reads every command in your logs under the new declarations and reports any that no longer come back whole.

## Write one by hand

A declaration is a `(command ...)` form. It can live in its own file under the `commands/` directory or in your rules file; they are merged by name, and a later source wins.

```scheme
(command git
  (option "-C" :value)                 ; takes a value: -C path or -Cpath
  (option "-P" "--no-pager")           ; a flag, short and long forms
  (option "--color" :optional)         ; a value only when attached: --color=always
  (subcommand stash :alias save        ; options after `stash` are stash's
    (option "-q" "--quiet")))
```

Option names are strings and look like `"-c"` or `"--long"`. A subcommand is named by its first argument and may carry its own options and subcommands. Only the first argument can name a subcommand.

## Programs that carry other commands

This is the part no completion database knows, so it is worth writing by hand. `:inner` says where the inner command or script sits:

```scheme
(command sudo (option "-u" "--user" :value) :inner (command))          ; the arguments are a command
(command timeout (option "-k" :value)      :inner (command :from 1))  ; after the duration
(command ssh (option "-o" :value)          :inner (script :from 1))   ; the words after the host
(command bash (option "-c")                :inner (script :from 0 :when "-c"))  ; argument 0, under -c
(command nu (option "-c" :value)           :inner (script :option "-c"))        ; the value of -c
```

A program with an `:inner` stops taking its own options at its first positional argument, so `sudo -u root git -C .` gives `-C` to git. Words shaped like `NAME=value` before the inner command belong to the wrapper, so `env FOO=1 sed` and `sudo FOO=1 sed` both reach `sed`.

The guard ships declarations for sudo, env, nice, nohup, timeout, xargs, ssh, bash, sh, nu, and python3. Print them for reference:

```
claude-guard rules --export
```

shows the rules; the declarations are in `rules/commands.scm` in the repository.

## Check what a declaration does

Every getopt rule the elaborator follows has a line in `spec/elaboration.scm`, written as a check you can read:

```scheme
(check "git clean -fxd" elaborates (git "clean" (option "-fxd" f x d) (subcommand clean))
  :commands ((command git (subcommand clean (option "-f") (option "-x") (option "-d")))))
```

To see how the guard reads a command under your declarations, look at the `elaborated` field of its log line: options with their flags and values, the subcommand path, and the inner command or script.

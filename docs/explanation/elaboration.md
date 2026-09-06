# Elaboration

A shell command arrives as a flat list of words. `git -C . stash` is four of them, and nothing in the list says that `.` belongs to `-C`. A pattern like `[git -... stash ...]` cannot know either, so on a flat list it fails: `-...` takes `-C`, refuses `.`, and `stash` is never reached.

Elaboration is the step between the segmenter and the matcher that adds what the word list lacks. Given a declaration of what `git` takes, it classifies each word: the name, an option with its value, an argument, a subcommand name, and, for programs that wrap other programs, where the inner command begins.

## The partition property

The one rule elaboration must never break: every input word appears exactly once in the output, in order, and flattening the output gives the input back. `git clean -fxd` becomes one option whose text is still `-fxd` and whose flags are `f`, `x`, `d`; the text is kept and the flags are derived. Nothing is normalized away, nothing is invented.

That property is what makes the elaborator trustworthy enough to sit in front of a matcher that denies things. It is tested three ways: unit tests assert it on every case, a fixture of real commands pulled from session logs asserts it on 393 of them, and `claude-guard elaborate --check-log` asserts it over every command in your own logs whenever you ask. It found a segmenter bug on its first run, a word being dropped, and that is the kind of bug it exists to find.

## Tokens keep their meaning

Elaboration does not change what `-...` or `*` mean. `-...` still means zero or more option words. What changes is what an option word is: for a declared program, the flag together with its value, because the arity is known. The matcher walks units, and a declared option with its value is one unit with two elements. `-...` takes the unit; a literal `-C` followed by a binder takes the two elements one at a time, so `[git -C ?dir ...]` binds `dir` whether the command said `-C .` or `-C.`.

For a program with no declaration, every word is a unit of one element and a dash word is an option, which is exactly the matcher's behavior before elaboration existed. So adding a declaration can only make matching more accurate, never change a rule about an undeclared program.

## Data, not code

Which flags take values is a fact about a program, not a policy, and there are thousands of such facts. They are declared in the rule language, `(command git (option "-C" :value) ...)`, and for most programs they are generated rather than written: `claude-guard commands add git` asks [carapace](https://github.com/carapace-sh/carapace-bin) for its completion data and converts it. Carapace knows hundreds of programs, and its export says for each flag whether it takes a value, which is the one bit the elaborator needs.

The design deliberately does not ship a corpus. The built-in declarations are only the dozen programs that carry other commands, sudo, ssh, bash and their kin, because no completion database knows that and because a rule about `sed` is worthless if `sudo sed` slips past it. Everything else grows from your own logs: `claude-guard commands` shows which programs you run and where a declaration would have changed a decision, and you add them one at a time.

## Inner commands

`:inner` is one mechanism for three things that used to be three separate problems: wrappers like `sudo` and `timeout` whose arguments are a command, `ssh` whose trailing words are a script for a remote shell, and `bash -c` and `nu -c` whose script is an argument or an option value. In each case the inner text is elaborated, or segmented and then elaborated, and matched like a top-level call. A rule is tried outermost first, eight levels deep, and the deny names the inner command that matched.

Two details fell out of testing. A wrapper stops taking its own options at its first positional, so `sudo -u root git -C .` gives `-C` to git and not to sudo. And `NAME=value` words before the inner command belong to the wrapper, so `env FOO=1 sed` and `sudo FOO=1 sed` both reach `sed`.

## What it does not do

Getopt is a convention, not a law. `tar xvf`, `ps aux`, `dd if=`, and `set +x` do not follow it, and the elaborator leaves such words as arguments. An external elaborator over stdio, the same escape hatch the design gives predicates, is the planned answer for programs whose grammar needs real parsing. An inner script that fails to segment is skipped rather than turned into an ask, which is a known gap.

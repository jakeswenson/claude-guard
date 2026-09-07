# The rule language

The complete grammar and its meaning. A rule's behavior is decidable from this page without reading the source. The executable spec under `spec/` states the same facts as `check` lines that the tests run.

## Files

A file is a sequence of top-level forms, each a `(rule ...)` or a `(command ...)`. Comments run from `;` to the end of the line. The reader knows five node kinds: lists in parentheses, patterns in square brackets, strings in double quotes with `\"`, `\\`, `\n`, and `\t` escapes, keywords starting with `:`, and symbols, which are any other run of characters up to whitespace or one of `()[]";`. There are no numbers.

Which files load, and in what order, is in [the command line](command-line.md#files).

## Rules

```
rule := (rule <name> [:when <cond>] row+)
row  := (deny | ask | warn <subject> [:when <cond>] :reason "..." [:instead "..."])
```

`<name>` is a symbol. `:reason` is required. `:instead` is required on `deny` and `ask` and optional on `warn`. A rule with no rows, a keyword given twice, or a keyword the grammar does not know is a load error.

### Subjects

```
subject := [word*]                                 ; a Bash command pattern
         | (write | edit | multi-edit | read <path>)   ; a file tool pattern
path    := "literal" | ?name
```

A file tool pattern matches the tool of that name. A literal path matches the call's path after `/private` normalization; a binder captures it.

### Pattern words

| Word | Matches |
|---|---|
| a symbol | one literal word, byte-equal |
| `"a string"` | one literal word, byte-equal; for words with spaces or quotes |
| `*` | one unit, literal or dynamic |
| `...` | zero or more elements, any kind |
| `-*` | one option unit |
| `-...` | zero or more option units |
| `?name` | one literal word, captured under `name` |
| `> w`, `>> w`, `< w` | a redirect of that kind, in any position; `w` is any word above |

A redirect pattern `> w` accepts a write or an append; `>> w` an append only; `< w` a read. `2>&1` and heredocs never reach the matcher.

A dynamic word, one the shell would expand, matches only `*` and `...`.

### Units and elements

The matcher walks units. Without a declaration for the program, every word is one unit with one element, and a literal word starting with `-` is an option unit. With a declaration, an option and its value form one unit whose elements are the option's canonical flags and then its value: `-fxd` gives `-f`, `-x`, `-d`; `-C.` and `-C .` both give `-C`, `.`; `--git-dir=x` gives `--git-dir`, `x`. Words after `--` are argument units.

`*`, `-*`, and `-...` take whole units and only from a unit boundary. A literal, a binder, and `...` walk elements, so a literal can find a flag inside a cluster and a binder can take the value after a flag. The tokens keep their meaning either way; a declaration changes only what they see.

### Binding

A pattern may match in more than one way when it holds `...` or `-...`. Each way produces a binding set: what each `?name` captured. A binder that appears twice must capture the same word both times. The row's condition is evaluated under each binding set, and the row fires on the first set under which it holds; that set is recorded.

### Conditions

```
cond := (<fact> arg*)
      | (and cond+) | (or cond+) | (not cond)
arg  := ?name | "text"
```

A fact is a proposition about the call: named in a condition with its arguments, it holds, fails, or is unknown. The name ends in `?`. Naming a fact the guard does not know is a load error, and each fact checks its own arguments at load time.

Two facts exist:

| Fact | Holds when |
|---|---|
| `(ancestor-has? "name")` | the call's working directory or any directory above it contains an entry named `name` |
| `(under? <arg> "prefix")` | the path, resolved against the working directory if relative and with `/private` stripped, is the prefix or below it by path component |

Conditions are three-valued: true, false, unknown. `and`, `or`, and `not` follow Kleene's tables: one false settles an `and`, one true settles an `or`, `not` swaps true and false, and everything else that touches an unknown is unknown. No shipped fact produces unknown in this version; a fact that asks a program will. What an unknown does to a rule is under [Evaluation](#evaluation).

An answer carries a reason when the fact gave one. An unknown always carries one, prefixed with the fact's name: `in-jj-repo? is unknown: timed out after 1s`. An unknown `and` or `or` carries the reason of its first unknown part, in evaluation order. A settled one carries the deciding part's reason, or every part's reasons joined with `; ` when all agreed. `not` keeps the reason and flips the truth. Reasons are evidence for the log and the rendered text; they never change a truth.

A binder in a row's condition must be declared by the row's pattern. A rule's `:when` runs before any pattern matches and may use no binders. Both are load errors.

## Evaluation

For one tool call:

1. If the call is a Bash command the parser rejects, the answer is `ask` with the parser's message. Nothing else runs.
2. Rules in file order. A rule whose `:when` is false is skipped whole; one whose `:when` is unknown has its rows tried. Within a rule, rows in order. A row fires when its subject matches and its condition holds under some binding set. The first firing row anywhere is the answer.
3. A deny or ask row whose subject matched and whose condition, or whose rule's `:when`, is unknown answers `ask`, with the reason for the unknown in parentheses. A warn row in that position is skipped. Among several binding sets, one under which the condition holds wins over any unknown; with none holding, the first unknown set is the evidence. When both the rule's `:when` and the row's condition are unknown, the rule's reason is the evidence, since it ran first.
4. If no row fired and the command held a `$(...)` or backquote substitution, the answer is `warn` naming the uninspected text.
5. Otherwise the call passes and nothing is printed.

A command pattern is tried against every simple command of the call, and then against what each carries: an inner command as is, an inner script segmented first, to a depth of eight. The outermost match wins. What was matched is what the deny text names.

The rendered text is `claude-guard denied `<what>`: <reason> Instead: <instead>` for deny, `claude-guard asks about ...` for ask, and `claude-guard noted ...` for warn, with the `Instead:` clause absent when the row has none. An ask caused by an unknown ends with the evidence in parentheses: `claude-guard asks about `git stash`: <reason> Instead: <instead> (in-jj-repo? is unknown: timed out after 1s)`. The evidence names the fact and gives the fact's own reason.

## Command declarations

```
command := (command <name> decl)
decl    := (option "-c" ["--long"] [:value | :optional])*
           (subcommand <name> [:alias <name>]* decl)*
           [:inner (command [:from N])
                 | (script [:from N] [:when "-c"])
                 | (script :option "-c")]
```

An option has a short form, a long form, or both. `:value` means it takes one value, attached or as the next word. `:optional` means a value only when attached. An option with neither is a flag.

Elaboration of a declared command follows getopt: `--` ends options; `--name=value` is one option with an attached value; `-fxd` splits into flags when every letter is a declared short flag, and a letter that takes a value swallows the rest of the word or the next word; a word with an undeclared flag stays whole with nothing derived and takes no value; a dynamic word is an argument. The first argument may name a subcommand, after which the subcommand's declaration applies.

`:inner` says where the program carries another command. `(command)` and `(command :from N)`: the arguments, from position N, are a command; `NAME=value` words before it belong to the wrapper. `(script :from N)`: the arguments from N on are a script, joined with spaces, one word taken verbatim. `(script :from N :when "-c")`: the same, only when flag `-c` was given. `(script :option "-c")`: the value of `-c` is the script. A program with an `:inner` stops taking its own options at its first positional argument.

Elaboration is a partition: every input word appears exactly once in the elaborated form, in order, and flattening it returns the input. `claude-guard elaborate --check-log` verifies this over every logged command.

## Errors

Every error names its position as `file:line:col: message`. Reading, parsing, and type-checking are separate stages, and one load reports every failing top-level form across every file. A file that fails to load leaves the guard failing open: each call is recorded as observed and no decision is made until the file is fixed.

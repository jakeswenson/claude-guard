;; Elaboration: how a command is classified under a declaration, and what
;; the matcher then sees. D22 to D25.
;;
;; (check "command" elaborates (name item...) :commands ((command ...)))
;; renders the elaboration as: the name; each argument as a string; each
;; option as (option "text as written" flag... value), with the value a
;; string when it was its own word and (attached "v") when it lived inside
;; the option word; (dynamic "text") for a word the shell would expand;
;; (> "target") for a redirect; then (subcommand ...) and (inner ...) when
;; present. Elaboration is a partition: the strings and option texts, read
;; in order, are the input words.

;; --- an undeclared command is its name and arguments ---

(check "rg -n foo src" elaborates (rg "-n" "foo" "src"))
(check "> out"         elaborates (_ (> "out")))

;; --- values ---

(check "git -C . stash"        elaborates (git (option "-C" C ".") "stash")
  :commands ((command git (option "-C" :value))))
(check "git -C. stash"         elaborates (git (option "-C." C (attached ".")) "stash")
  :commands ((command git (option "-C" :value))))
(check "git --git-dir=.git st" elaborates (git (option "--git-dir=.git" git-dir (attached ".git")) "st")
  :commands ((command git (option "--git-dir" :value))))
(check "git --git-dir .git st" elaborates (git (option "--git-dir" git-dir ".git") "st")
  :commands ((command git (option "--git-dir" :value))))
(check "git -C"                elaborates (git (option "-C" C))
  :commands ((command git (option "-C" :value))))

;; An optional value is taken only when attached.
(check "git --color=always log" elaborates (git (option "--color=always" color (attached "always")) "log")
  :commands ((command git (option "--color" :optional))))
(check "git --color log"        elaborates (git (option "--color" color) "log")
  :commands ((command git (option "--color" :optional))))

;; --- clustering ---

(check "git clean -fxd" elaborates (git "clean" (option "-fxd" f x d) (subcommand clean))
  :commands ((command git (subcommand clean (option "-f") (option "-x") (option "-d")))))
(check "git commit -am wip" elaborates (git "commit" (option "-am" a m "wip") (subcommand commit))
  :commands ((command git (subcommand commit (option "-a") (option "-m" :value)))))
(check "git commit -amwip" elaborates (git "commit" (option "-amwip" a m (attached "wip")) (subcommand commit))
  :commands ((command git (subcommand commit (option "-a") (option "-m" :value)))))

;; The value letter may sit last in a cluster: `set -euxo pipefail`.
(check "set -euxo pipefail" elaborates (set (option "-euxo" e u x o "pipefail"))
  :commands ((command set (option "-e") (option "-u") (option "-x") (option "-o" :value))))
(check "set -euxo pipefail" elaborates (set "-euxo" "pipefail"))
;; `+x` is not a dash word, so the elaborator leaves it an argument.
(check "set +x" elaborates (set "+x")
  :commands ((command set (option "-x"))))

;; A word with an undeclared letter stays whole and takes nothing.
(check "git -zq . stash" elaborates (git (option "-zq") "." "stash")
  :commands ((command git (option "-q"))))
(check "git --no-such x"  elaborates (git (option "--no-such") "x")
  :commands ((command git (option "-C" :value))))

;; A lone dash is an argument, even as a value.
(check "git commit -m -" elaborates (git "commit" (option "-m" m "-") (subcommand commit))
  :commands ((command git (subcommand commit (option "-m" :value)))))

;; --- double dash ---

(check "git commit -- -m" elaborates (git "commit" (option "--") "-m" (subcommand commit))
  :commands ((command git (subcommand commit (option "-m" :value)))))

;; --- subcommands ---

;; Only the first argument names a subcommand; its options apply after it.
(check "git -P stash -q" elaborates (git (option "-P" P) "stash" (option "-q" q) (subcommand stash))
  :commands ((command git (option "-P") (subcommand stash (option "-q")))))
(check "git -q stash"    elaborates (git (option "-q") "stash" (subcommand stash))
  :commands ((command git (subcommand stash (option "-q")))))
(check "git log stash"   elaborates (git "log" "stash")
  :commands ((command git (subcommand stash))))
(check "git save"        elaborates (git "save" (subcommand save))
  :commands ((command git (subcommand stash :alias save))))

;; --- dynamic words ---

;; A dynamic word is an argument, dash or not; a dynamic value is still
;; the value. The segmenter keeps a dynamic word's raw text.
(check "git \"-$flag\" stash" elaborates (git (dynamic "\"-$flag\"") "stash")
  :commands ((command git (option "-C" :value))))
(check "git -C $dir stash"    elaborates (git (option "-C" C (dynamic "$dir")) "stash" (subcommand stash))
  :commands ((command git (option "-C" :value) (subcommand stash))))

;; --- inner commands ---

(check "sudo -u root git -C . stash" elaborates
  (sudo (option "-u" u "root") "git" "-C" "." "stash"
        (inner (command (git (option "-C" C ".") "stash"))))
  :commands ((command sudo (option "-u" :value) :inner (command))
             (command git (option "-C" :value))))

;; A wrapper stops taking its own options at its first positional.
(check "sudo -n git -n" elaborates (sudo (option "-n" n) "git" "-n" (inner (command (git "-n"))))
  :commands ((command sudo (option "-n") :inner (command))))

;; Assignments before the inner command belong to the wrapper.
(check "env FOO=1 sed -i x" elaborates (env "FOO=1" "sed" "-i" "x" (inner (command (sed "-i" "x"))))
  :commands ((command env :inner (command))))

;; The inner command can start after leading positionals.
(check "timeout 5s git stash" elaborates (timeout "5s" "git" "stash" (inner (command (git "stash"))))
  :commands ((command timeout :inner (command :from 1))))

;; --- inner scripts ---

;; One word is the script itself; several are rejoined, with quoting.
(check "ssh nas 'cd /a && rm -rf x'" elaborates (ssh "nas" "cd /a && rm -rf x" (inner (script "cd /a && rm -rf x")))
  :commands ((command ssh :inner (script :from 1))))
(check "ssh nas echo 'a b' c"        elaborates (ssh "nas" "echo" "a b" "c" (inner (script "echo 'a b' c")))
  :commands ((command ssh :inner (script :from 1))))
(check "ssh nas"                     elaborates (ssh "nas")
  :commands ((command ssh :inner (script :from 1))))

;; bash carries a script only under -c; nu's script is -c's value.
(check "bash -ec 'git stash'" elaborates (bash (option "-ec" e c) "git stash" (inner (script "git stash")))
  :commands ((command bash (option "-e") (option "-c") :inner (script :from 0 :when "-c"))))
(check "bash script.sh"       elaborates (bash "script.sh")
  :commands ((command bash (option "-c") :inner (script :from 0 :when "-c"))))
(check "nu -c 'ls | length'"  elaborates (nu (option "-c" c "ls | length") (inner (script "ls | length")))
  :commands ((command nu (option "-c" :value) :inner (script :option "-c"))))

;; --- what the matcher sees (D23) ---

;; The tokens keep their meaning; elaboration changes the words they see.
;; With a declaration, `-C .` is one option word, so `-...` takes it.
(check [git -... stash ...] matches "git -C . stash" :commands ((command git (option "-C" :value))))
(check [git -... stash ...] misses  "git -C . stash")
(check [git -... stash ...] matches "git -C. stash"  :commands ((command git (option "-C" :value))))

;; A literal looks inside a cluster and a binder takes an option's value
;; under either spelling.
(check [git clean -f ...] matches "git clean -fxd"
  :commands ((command git (subcommand clean (option "-f") (option "-x") (option "-d")))))
(check [git clean -f ...] misses  "git clean -xd"
  :commands ((command git (subcommand clean (option "-f") (option "-x") (option "-d")))))
(check [git -C ?dir ...] binds "git -C /tmp/x log" (?dir "/tmp/x") :commands ((command git (option "-C" :value))))
(check [git -C ?dir ...] binds "git -C/tmp/x log"  (?dir "/tmp/x") :commands ((command git (option "-C" :value))))
(check [git -C stash]    misses "git -C . stash"                    :commands ((command git (option "-C" :value))))

;; `*` takes a whole option unit; `-*` and `-...` cannot start inside one.
(check [git * stash]      matches "git -C . stash" :commands ((command git (option "-C" :value))))
(check [git clean -f -*]  misses  "git clean -fxd"
  :commands ((command git (subcommand clean (option "-f") (option "-x") (option "-d")))))
(check [git clean -f -*]  matches "git clean -f -x"
  :commands ((command git (subcommand clean (option "-f") (option "-x") (option "-d")))))

;; After `--`, a dash word is an argument under a declaration and an option
;; without one. A literal still matches it either way: a literal is the
;; word's text, whatever the word is.
(check [git commit -... -*] misses  "git commit -- -m" :commands ((command git (subcommand commit (option "-m" :value)))))
(check [git commit -... -*] matches "git commit -- -m")
(check [git commit -... -m] matches "git commit -- -m" :commands ((command git (subcommand commit (option "-m" :value)))))

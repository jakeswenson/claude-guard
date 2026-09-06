;; Term patterns: what a [pattern] matches, and what its binders capture.
;;
;; A check is (check [pattern] <verb> "command" ...). `matches` and
;; `misses` claim a match or none. `binds` claims the exact binding sets:
;; (?name "word") pairs for one set, or several sets each in its own list.
;; The command must be one simple command; the segmenter splits it the
;; way it splits a real call, so quoting works as in the shell.

;; --- literals ---

(check [git stash] matches "git stash")
(check [git stash] misses  "git stas")
(check [git stash] misses  "git Stash")
(check [git stash] misses  "git stash pop")
(check [git stash] misses  "git")

;; Quotes fold on the command side: the matcher sees words, not text.
(check [git stash] matches "git \"stash\"")
(check [git stash] matches "'git' sta\\sh")

;; A string in a pattern is a literal that may hold spaces.
(check [echo "hi there"] matches "echo 'hi there'")
(check [echo "hi there"] misses  "echo hi there")

;; An assignment-shaped word after the command name is an ordinary word.
;; Before the name it is an environment assignment and never reaches
;; the matcher.
(check [ssh -o ConnectTimeout=10 nas] matches "ssh -o ConnectTimeout=10 nas")
(check [make CC=clang all]           matches "make CC=clang all")
(check [make all]                    matches "CC=clang make all")

;; --- * takes exactly one word ---

(check [git *] matches "git stash")
(check [git *] misses  "git")
(check [git *] misses  "git stash pop")

;; --- ... takes zero or more words ---

(check [git stash ...] matches "git stash")
(check [git stash ...] matches "git stash pop")
(check [git stash ...] matches "git stash push -m wip")
(check [git stash ...] misses  "git log")

;; ... in the middle backtracks.
(check [cp ... x] matches "cp -r a b x")
(check [cp ... x] matches "cp x")
(check [cp ... x] misses  "cp x d")

;; Two of them in one pattern.
(check [git ... stash ...] matches "git -C . stash pop")
(check [git ... stash ...] matches "git stash")
(check [git ... stash ...] misses  "git log")

;; --- -* takes one dash word, -... takes zero or more ---

(check [git -* stash] matches "git --no-pager stash")
(check [git -* stash] matches "git -v stash")
(check [git -* stash] misses  "git stash")
(check [git -* stash] misses  "git log stash")

(check [git -... stash ...] matches "git stash")
(check [git -... stash ...] matches "git --no-pager -v stash pop")
(check [git -... stash ...] misses  "git log stash")

;; A flag's value is not a dash word, so `-...` stops at it. By design.
(check [git -... stash ...] misses "git -C . stash")

;; --- dynamic words ---

;; A word the shell would expand matches only * and ...: the guard does
;; not know what it will become.
(check [git stash]    misses  "git $cmd")
(check [git -* stash] misses  "git \"-$flag\" stash")
(check [git *]        matches "git $cmd")
(check [git ...]      matches "git $cmd $args")

;; --- redirects ---

;; A redirect in a pattern is found in any position on the command.
(check [cat ... > x] matches "cat > x a b")
(check [cat ... > x] matches "cat a > x b")
(check [cat ... > x] matches "cat a 2> err > x")
(check [cat ... > x] misses  "cat a b")

;; `>` accepts a write or an append; `>>` accepts an append only; `<` a read.
(check [cat ... > *]  matches "cat a > b")
(check [cat ... > *]  matches "cat a >> b")
(check [cat ... > *]  matches "cat a 2> b")
(check [cat ... > *]  matches "cat a &> b")
(check [cat ... >> *] matches "cat a >> b")
(check [cat ... >> *] misses  "cat a > b")
(check [cat ... > *]  misses  "cat a < b")
(check [cat < in]     matches "cat < in")

;; The target follows the word rules.
(check [cat ... > *]   matches "cat a > \"$out\"")
(check [cat ... > out] matches "cat a > 'out'")
(check [cat ... > out] misses  "cat a > \"$out\"")

;; A wordless command, such as `( a; b ) > out` or a bare `> out`,
;; matches a pattern with only a redirect.
(check [... > x] matches "> x")
(check [> x]     matches "> x")
(check [> x]     misses  "echo hi > x")
(check [* > x]   misses  "> x")

;; Fd duplication never reaches the matcher, so `2>&1` in a pattern is a
;; literal word that only a quoted command word can equal.
(check [cmd 2>&1] matches "cmd '2>&1'")
(check [cmd 2>&1] misses  "cmd 2>&1")

;; --- binders (D19) ---

;; A binder captures exactly one literal word.
(check [cp ... ?dst] binds  "cp -r a b" (?dst "b"))
(check [cp ... ?dst] misses "cp")

;; It never captures a dynamic word: a condition needs the text, and the
;; shell has not produced it.
(check [cp ... ?dst] misses "cp a $dst")

;; A binder that appears twice must capture the same word both times.
(check [cp ?x ?x] binds  "cp a a" (?x "a"))
(check [cp ?x ?x] misses "cp a b")

;; A pattern without binders yields one empty binding set per match,
;; deduplicated to one however many ways it matched.
(check [git ... ...] binds "git a b c" ())

;; Every way to match yields its own binding set, shortest `...` first.
(check [f ... ?x ...] binds "f a b" ((?x "a")) ((?x "b")))

;; A redirect target can bind, once per matching redirect.
(check [... > ?out] binds  "echo hi > /tmp/x 2> err" ((?out "/tmp/x")) ((?out "err")))
(check [... > ?out] misses "echo hi > $out")
(check [... > ?out] misses "echo hi")

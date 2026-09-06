;; claude-guard built-in rules.
;;
;; This file is embedded in the binary and is the rule table when no user
;; file exists. `claude-guard rules --export` prints it as a starting point
;; for ~/.config/claude-guard/rules.scm, which replaces it whole.
;;
;; Rules run top to bottom; within a rule, rows run top to bottom. The
;; first deny, ask, or warn wins. A row is
;;
;;   (deny [pattern] [:when (condition)] :reason "..." :instead "...")
;;
;; where the pattern is one shell command with wildcards: `*` one word,
;; `...` zero or more words, `-*` one dash word, `-...` zero or more dash
;; words, `?name` one word captured for the condition, and `> X`, `>> X`,
;; `< X` a redirect in any position. Conditions: (ancestor-has? ".jj"),
;; (under? ?p "/tmp"), and, or, not.

;; --- Banned everywhere ---

(rule hard-denies
  (deny [git -... worktree ...]
    :reason  "git worktrees are banned here."
    :instead "use `jj workspace add`, and ask the user before creating one.")
  (deny [git -... stash ...]
    :reason  "jj has no dirty tree, so there is nothing to stash."
    :instead "use `jj new` to park the current change or `jj describe` to name it.")
  (deny [git -... checkout ...]
    :reason  "checkout moves a git HEAD that jj does not track."
    :instead "use `jj edit <rev>` or `jj new <rev>`.")
  (deny [sed ...]
    :reason  "sed is banned."
    :instead "use `sd` for replacements, `rg` for searching, or the Edit tool.")
  (deny [chezmoi -... apply ...]
    :reason  "chezmoi apply changes the live dotfiles and is never run from a session."
    :instead "show the diff with `chezmoi diff` and let the user apply.")
  (deny [cat ... > *]
    :reason  "writing files through a cat redirect is banned."
    :instead "use the Write tool."))

;; --- git inside a jj repo ---

(rule git-in-jj :when (ancestor-has? ".jj")
  (deny [git -... log ...]
    :reason "this repo is managed by jj." :instead "use `jj log`.")
  (deny [git -... status ...]
    :reason "this repo is managed by jj." :instead "use `jj status`.")
  (deny [git -... diff ...]
    :reason "this repo is managed by jj." :instead "use `jj diff`.")
  (deny [git -... show ...]
    :reason "this repo is managed by jj." :instead "use `jj show`.")
  (deny [git -... blame ...]
    :reason "this repo is managed by jj." :instead "use `jj file annotate`.")
  (deny [git -... add ...]
    :reason "this repo is managed by jj." :instead "nothing; jj tracks new files on its own.")
  (deny [git -... commit ...]
    :reason "this repo is managed by jj." :instead "use `jj commit` or `jj describe`.")
  (deny [git -... push ...]
    :reason "this repo is managed by jj." :instead "use `jj git push`.")
  (deny [git -... pull ...]
    :reason "this repo is managed by jj." :instead "use `jj git fetch`, then `jj rebase`.")
  (deny [git -... fetch ...]
    :reason "this repo is managed by jj." :instead "use `jj git fetch`.")
  (deny [git -... rebase ...]
    :reason "this repo is managed by jj." :instead "use `jj rebase`.")
  (deny [git -... branch ...]
    :reason "this repo is managed by jj." :instead "use `jj bookmark`.")
  (deny [git ...]
    :reason  "this repo is managed by jj."
    :instead "use the jj equivalent, or `jj git <subcommand>` for remote operations."))

;; --- Nothing under /tmp ---

(rule tmp-writes
  (deny [... > ?out] :when (under? ?out "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example.")
  (deny [tee ... ?file] :when (under? ?file "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example.")
  (deny [mktemp ...]
    :reason  "mktemp creates files under /tmp."
    :instead "write inside the project, or use a test or an example.")
  (deny [cp ... ?dst] :when (under? ?dst "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example.")
  (deny [mv ... ?dst] :when (under? ?dst "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example.")
  (deny (write ?path) :when (under? ?path "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example.")
  (deny (edit ?path) :when (under? ?path "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example.")
  (deny (multi-edit ?path) :when (under? ?path "/tmp")
    :reason  "no files under /tmp."
    :instead "write inside the project, or use a test or an example."))

;; --- Use the better tool ---

(rule tool-nudges
  (deny [grep ...]
    :reason  "grep is not the search tool here."
    :instead "use `rg`.")
  (deny [find ...]
    :reason  "find is not the file finder here."
    :instead "use `fd`."))

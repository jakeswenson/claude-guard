;; The rules the docs build around the example facts. Paths are absolute
;; because the guard does not expand `~`; change them to where you put
;; the scripts. tests/cli.rs runs this file, with the paths swapped for
;; the repository's copies, so what the docs say is what the binary does.

(fact on-main? (exec "/Users/you/.config/claude-guard/facts/on-main.sh")
  :lifetime fresh
  :timeout "1s")

(fact owned-by? (exec "/Users/you/.config/claude-guard/facts/owned-by.sh")
  :lifetime fresh
  :timeout "1s")

(fact touches-tracked? (exec "/Users/you/.config/claude-guard/facts/touches-tracked.py")
  :lifetime fresh
  :timeout "2s")

(rule pushes
  (ask [git -... push ...] :when (on-main?)
    :reason "this pushes the main branch."
    :instead "push a feature branch and open a pull request."))

(rule ownership
  (deny [cp ... ?dst] :when (owned-by? "root" ?dst)
    :reason "the target belongs to root."
    :instead "copy somewhere you own, or ask."))

(rule redirects
  (warn [... > ?out] :when (not (touches-tracked?))
    :reason "this writes a file git does not track."))

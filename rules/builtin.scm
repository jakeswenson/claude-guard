;; claude-guard built-in rules.
;;
;; This file is embedded in the binary and is the rule table when no user
;; file exists. `claude-guard rules --export` writes it out as a starting
;; point for ~/.config/claude-guard/rules.scm, which replaces it whole.
;;
;; Rules run top to bottom; within a rule, rows run top to bottom. The
;; first deny, ask, or warn wins.
;;
;; The full table moves here from rules.rs in claude-guard-110.6. Until
;; then this file is loaded and type-checked but not consulted.

(rule hard-denies
  (deny [sed ...]
    :reason  "sed is banned."
    :instead "use `sd` for replacements, `rg` for searching, or the Edit tool."))

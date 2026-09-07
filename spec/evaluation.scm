;; Evaluation: what the engine decides for one rule and one command.
;;
;; A check is (check (rule ...) denies|asks|warns|passes "command"
;; ["text"] ...). The rule is parsed and run by the engine against the
;; command, with `:cwd` (default "/spec"), `:ancestors`, and `:facts`
;; standing in as in a condition check. The "text" after the verb must
;; equal what the model or the user sees. No check touches the disk.

;; --- a row fires when its subject matches and its condition holds ---

(check (rule r (deny [git -... stash ...] :reason "no stash." :instead "jj new."))
       denies "git stash"
       "claude-guard denied `git stash`: no stash. Instead: jj new.")
(check (rule r (deny [git -... stash ...] :reason "no stash." :instead "jj new."))
       passes "git log")
(check (rule r (ask [jj abandon ...] :reason "look first." :instead "jj status."))
       asks "jj abandon x"
       "claude-guard asks about `jj abandon x`: look first. Instead: jj status.")
(check (rule r (warn [cargo clean] :reason "slow."))
       warns "cargo clean"
       "claude-guard noted `cargo clean`: slow.")

;; A row's condition decides under the bindings its pattern produced.
(check (rule r (deny [cp ... ?dst] :when (under? ?dst "/tmp") :reason "no /tmp." :instead "here."))
       denies "cp a /tmp/b")
(check (rule r (deny [cp ... ?dst] :when (under? ?dst "/tmp") :reason "no /tmp." :instead "here."))
       passes "cp a /var/b")

;; A rule's `:when` gates every row: false skips the rule whole.
(check (rule r :when (ancestor-has? ".jj") (deny [git ...] :reason "jj." :instead "jj."))
       denies "git log" :ancestors (".jj"))
(check (rule r :when (ancestor-has? ".jj") (deny [git ...] :reason "jj." :instead "jj."))
       passes "git log")

;; --- a fact's reason is evidence, appended in parentheses (D10) ---

;; When the condition held, the reasons the facts gave follow the row's
;; text, so the model sees what the world said, not only what the rule
;; said. A fact with no reason adds nothing; no reasons, no parentheses.
(check (rule r (deny [git -... stash ...] :when (managed?) :reason "no stash." :instead "jj new."))
       denies "git stash"
       "claude-guard denied `git stash`: no stash. Instead: jj new. (jj root is /x)"
       :facts ((managed? holds "jj root is /x")))
(check (rule r (deny [git -... stash ...] :when (managed?) :reason "no stash." :instead "jj new."))
       denies "git stash"
       "claude-guard denied `git stash`: no stash. Instead: jj new."
       :facts ((managed? holds)))
(check (rule r (warn [cargo clean] :when (slow-disk?) :reason "slow."))
       warns "cargo clean"
       "claude-guard noted `cargo clean`: slow. (disk is 91% full)"
       :facts ((slow-disk? holds "disk is 91% full")))

;; The rule's `:when` reasons come first, then the row's, joined.
(check (rule r :when (managed?) (deny [x] :when (and (a?) (b?)) :reason "r." :instead "i."))
       denies "x"
       "claude-guard denied `x`: r. Instead: i. (jj root is /x; a held; b held)"
       :facts ((managed? holds "jj root is /x") (a? holds "a held") (b? holds "b held")))

;; --- unknown on a matched pattern asks (D14) ---

;; A deny or ask row whose pattern matched and whose condition is unknown
;; asks, with the reason in parentheses so the agent can settle it.
(check (rule r (deny [git -... stash ...] :when (in-jj-repo?) :reason "no stash." :instead "jj new."))
       asks "git stash"
       "claude-guard asks about `git stash`: no stash. Instead: jj new. (in-jj-repo? is unknown: timed out after 1s)"
       :facts ((in-jj-repo? unknown "timed out after 1s")))
(check (rule r (ask [git -... stash ...] :when (in-jj-repo?) :reason "sure?" :instead "jj new."))
       asks "git stash"
       "claude-guard asks about `git stash`: sure? Instead: jj new. (in-jj-repo? is unknown: timed out after 1s)"
       :facts ((in-jj-repo? unknown "timed out after 1s")))

;; A warn row in the same position skips: a warning with an unsettled
;; premise is noise.
(check (rule r (warn [git -... stash ...] :when (in-jj-repo?) :reason "hm."))
       passes "git stash"
       :facts ((in-jj-repo? unknown "timed out after 1s")))

;; A pattern that did not match is not an ask, whatever the facts say.
(check (rule r (deny [git -... stash ...] :when (in-jj-repo?) :reason "no stash." :instead "jj new."))
       passes "git log"
       :facts ((in-jj-repo? unknown "timed out after 1s")))

;; Among several binding sets, one that holds wins over any unknown; with
;; none holding, the first unknown is the evidence.
(check (rule r (deny [cp ... ?x ...] :when (and (under? ?x "/tmp") (slow?)) :reason "no." :instead "ask."))
       asks "cp a /tmp/b"
       "claude-guard asks about `cp a /tmp/b`: no. Instead: ask. (slow? is unknown: timed out)"
       :facts ((slow? unknown "timed out")))
(check (rule r (deny [cp ... ?x ...] :when (or (under? ?x "/tmp") (slow?)) :reason "no." :instead "ask."))
       denies "cp a /tmp/b"
       :facts ((slow? unknown "timed out")))

;; The unknown's reason is the first unknown fact in evaluation order.
(check (rule r (deny [x] :when (and (u?) (v?)) :reason "r." :instead "i."))
       asks "x"
       "claude-guard asks about `x`: r. Instead: i. (u? is unknown: u out)"
       :facts ((u? unknown "u out") (v? unknown "v out")))

;; An unknown in a row that skips does not stop a later row from
;; deciding: first opinion wins, and a skipped warn is no opinion.
(check (rule r (warn [x] :when (u?) :reason "hm.")
               (deny [x] :reason "no." :instead "i."))
       denies "x"
       :facts ((u? unknown "u out")))

;; --- an unknown rule-level `:when` asks on a matching row (ADR 0001) ---

;; The rule's opinion is unresolved, not absent: its rows are tried, and
;; a matching deny or ask row asks with the rule's evidence.
(check (rule r :when (in-jj-repo?) (deny [git ...] :reason "jj." :instead "use jj."))
       asks "git log"
       "claude-guard asks about `git log`: jj. Instead: use jj. (in-jj-repo? is unknown: timed out)"
       :facts ((in-jj-repo? unknown "timed out")))
(check (rule r :when (in-jj-repo?) (warn [git ...] :reason "jj."))
       passes "git log"
       :facts ((in-jj-repo? unknown "timed out")))
(check (rule r :when (in-jj-repo?) (deny [git ...] :reason "jj." :instead "use jj."))
       passes "cargo build"
       :facts ((in-jj-repo? unknown "timed out")))

;; The rule's `:when` ran first, so when both it and the row's condition
;; are unknown, the rule's reason is the evidence.
(check (rule r :when (a?) (deny [x] :when (b?) :reason "r." :instead "i."))
       asks "x"
       "claude-guard asks about `x`: r. Instead: i. (a? is unknown: a out)"
       :facts ((a? unknown "a out") (b? unknown "b out")))

;; A held rule `:when` with an unknown row condition is the row's evidence.
(check (rule r :when (a?) (deny [x] :when (b?) :reason "r." :instead "i."))
       asks "x"
       "claude-guard asks about `x`: r. Instead: i. (b? is unknown: b out)"
       :facts ((a? holds) (b? unknown "b out")))

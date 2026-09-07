;; Facts: what a declared fact does to a rule, stated without spawning.
;;
;; A `(fact name (exec ...) ...)` declaration registers a program under a
;; name; `:facts` registers a stub under a name instead, with a fixed
;; answer, so every line here runs the real engine and the real registry
;; and never a process. A stub with `:args` answers only for those
;; arguments and fails for any other, naming what it was asked with,
;; which is how a line shows that a condition passed what it meant to.
;; `:asked` lists what the evaluation asked, in order, which is what the
;; log record carries. The protocol a real program speaks is tested in
;; Rust with `sh`, since it needs a process.

;; --- a declared fact decides like a built-in one ---

(check (rule r (deny [git -... stash ...] :when (managed?) :reason "no stash." :instead "jj new."))
       denies "git stash" :facts ((managed? holds)))
(check (rule r (deny [git -... stash ...] :when (managed?) :reason "no stash." :instead "jj new."))
       passes "git stash" :facts ((managed? fails)))
(check (rule r :when (managed?) (deny [git ...] :reason "jj." :instead "use jj."))
       denies "git log" :facts ((managed? holds)))
(check (rule r :when (managed?) (deny [git ...] :reason "jj." :instead "use jj."))
       passes "git log" :facts ((managed? fails)))

;; Its reason is evidence after the row's text (D10), and its unknown
;; asks (D14); spec/evaluation.scm has every case of both.
(check (rule r (deny [git -... stash ...] :when (managed?) :reason "no stash." :instead "jj new."))
       denies "git stash"
       "claude-guard denied `git stash`: no stash. Instead: jj new. (jj root is /x)"
       :facts ((managed? holds "jj root is /x")))
(check (rule r (deny [git -... stash ...] :when (managed?) :reason "no stash." :instead "jj new."))
       asks "git stash"
       "claude-guard asks about `git stash`: no stash. Instead: jj new. (managed? is unknown: exited with status 3)"
       :facts ((managed? unknown "exited with status 3")))
(check (rule r (warn [git -... stash ...] :when (managed?) :reason "hm."))
       passes "git stash" :facts ((managed? unknown "exited with status 3")))

;; --- the condition's arguments reach the fact, in order ---

;; Literals and binders, as written; a binder carries what the pattern
;; captured for that binding set.
(check (rule r (deny [cp ... ?dst] :when (owned? "root" ?dst) :reason "no." :instead "ask."))
       denies "cp a /etc/hosts"
       :facts ((owned? holds :args ("root" "/etc/hosts"))))
(check (rule r (deny [cp ... ?dst] :when (owned? "root" ?dst) :reason "no." :instead "ask."))
       passes "cp a /home/me/x"
       :facts ((owned? holds :args ("root" "/etc/hosts"))))
(check (rule r (deny [cp ?src ?dst] :when (same-owner? ?src ?dst) :reason "no." :instead "ask."))
       denies "cp a b"
       :facts ((same-owner? holds :args ("a" "b"))))
(check (rule r (deny [cp ?src ?dst] :when (same-owner? ?dst ?src) :reason "no." :instead "ask."))
       passes "cp a b"
       :facts ((same-owner? holds :args ("a" "b"))))

;; A fact with no arguments is asked with none.
(check (rule r (deny [x] :when (f?) :reason "r." :instead "i."))
       denies "x" :facts ((f? holds :args ())))

;; Among several binding sets, each set asks with its own capture, until
;; one holds.
(check (rule r (deny [cp ... ?x ...] :when (tracked? ?x) :reason "no." :instead "ask."))
       denies "cp a b c"
       :asked ((tracked? "a") (tracked? "b"))
       :facts ((tracked? holds :args ("b"))))

;; --- a fact is asked once per call for the same arguments ---

;; The rule's `:when` and the row's condition name the same fact: one
;; ask, one entry, and the second answer comes from the memo.
(check (rule r :when (managed?) (deny [git ...] :when (managed?) :reason "jj." :instead "use jj."))
       denies "git log"
       :asked ((managed?))
       :facts ((managed? holds)))

;; Different arguments are different questions.
(check (rule r (deny [cp ?src ?dst] :when (and (owned? ?src) (owned? ?dst)) :reason "no." :instead "ask."))
       denies "cp a b"
       :asked ((owned? "a") (owned? "b"))
       :facts ((owned? holds)))

;; A built-in is memoized the same way.
(check (rule r :when (ancestor-has? ".jj") (deny [git ...] :when (ancestor-has? ".jj") :reason "jj." :instead "use jj."))
       denies "git log"
       :asked ((ancestor-has? ".jj"))
       :ancestors (".jj"))

;; --- what is asked follows evaluation order, and stops at the answer ---

;; The rule's `:when` first; false skips its rows, so their facts are
;; never asked.
(check (rule r :when (a?) (deny [x] :when (b?) :reason "r." :instead "i."))
       passes "x"
       :asked ((a?))
       :facts ((a? fails) (b? holds)))

;; A row whose pattern does not match asks nothing.
(check (rule r (deny [y] :when (b?) :reason "r." :instead "i."))
       passes "x"
       :asked ()
       :facts ((b? holds)))

;; `and` stops at the first false, `or` at the first true.
(check (rule r (deny [x] :when (and (a?) (b?)) :reason "r." :instead "i."))
       passes "x"
       :asked ((a?))
       :facts ((a? fails) (b? holds)))
(check (rule r (deny [x] :when (or (a?) (b?)) :reason "r." :instead "i."))
       denies "x"
       :asked ((a?))
       :facts ((a? holds) (b? holds)))

;; First opinion wins: a later row's facts are never asked.
(check (rule r (deny [x] :when (a?) :reason "r." :instead "i.")
               (deny [x] :when (b?) :reason "r." :instead "i."))
       denies "x"
       :asked ((a?))
       :facts ((a? holds) (b? holds)))

;; A skipped warn row did ask; the answer is in the record even though
;; the row said nothing.
(check (rule r (warn [x] :when (u?) :reason "hm.")
               (deny [x] :when (a?) :reason "r." :instead "i."))
       denies "x"
       :asked ((u?) (a?))
       :facts ((u? unknown "slow") (a? holds)))

;; --- an unknown from a program combines like any other (Kleene) ---

;; spec/conditions.scm has the tables; these are the shapes a rule file
;; writes with a declared fact that could not answer.
(check (and (ancestor-has? ".jj") (managed?)) unknown "managed? is unknown: timed out after 1s"
       :ancestors (".jj") :facts ((managed? unknown "timed out after 1s")))
(check (and (ancestor-has? ".jj") (managed?)) fails
       :facts ((managed? unknown "timed out after 1s")))
(check (or (ancestor-has? ".jj") (managed?)) holds
       :ancestors (".jj") :facts ((managed? unknown "timed out after 1s")))
(check (or (ancestor-has? ".jj") (managed?)) unknown "managed? is unknown: timed out after 1s"
       :facts ((managed? unknown "timed out after 1s")))
(check (not (managed?)) unknown "managed? is unknown: timed out after 1s"
       :facts ((managed? unknown "timed out after 1s")))

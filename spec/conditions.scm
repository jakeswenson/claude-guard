;; Conditions: what a row's `:when` evaluates to.
;;
;; A check is (check (condition) holds|fails|unknown ["reason"] ...).
;; `:with` gives the binding set the row's pattern produced. `:cwd` is
;; the call's cwd, default "/spec". `:ancestors` lists the entries some
;; ancestor of cwd has; everything else does not exist. `:ancestors
;; unknown` makes `ancestor-has?` answer unknown, which is what a
;; timed-out extern fact will do. `:facts` stands in any fact by name
;; with a fixed answer: (name holds|fails|unknown ["reason"]). A "reason"
;; after the verb must equal the answer's reason. No check touches the
;; disk.

;; --- ancestor-has? ---

(check (ancestor-has? ".jj") holds :ancestors (".jj"))
(check (ancestor-has? ".jj") fails :ancestors (".git"))
(check (ancestor-has? ".jj") fails)

;; --- under? (D21) ---

;; The path is the prefix or below it, by path component.
(check (under? ?p "/tmp") holds :with ((?p "/tmp/x")))
(check (under? ?p "/tmp") holds :with ((?p "/tmp/x/y")))
(check (under? ?p "/tmp") holds :with ((?p "/tmp")))
(check (under? ?p "/tmp") fails :with ((?p "/tmpfoo")))
(check (under? ?p "/tmp") fails :with ((?p "/var/tmp/x")))

;; macOS mounts /tmp as /private/tmp; both spellings are the same place,
;; on either side.
(check (under? "/private/tmp/y" "/tmp") holds)
(check (under? "/tmp/y" "/private/tmp") holds)

;; A relative path resolves against the call's cwd.
(check (under? ?p "/Users/x/proj")     holds :with ((?p "src/main.rs")) :cwd "/Users/x/proj")
(check (under? ?p "/Users/x/proj/src") holds :with ((?p "src/main.rs")) :cwd "/Users/x/proj")
(check (under? ?p "/tmp")              fails :with ((?p "src/main.rs")) :cwd "/Users/x/proj")
(check (under? "x" "/tmp")             holds :cwd "/tmp")
(check (under? "x" "/tmp")             fails :cwd "/Users/x")

;; --- and, or, not: Kleene's tables ---

;; One false settles an `and`; one true settles an `or`. Everything else
;; that touches an unknown stays unknown, so an unknown never becomes a
;; confident answer by accident.
(check (and (ancestor-has? "t") (ancestor-has? "t")) holds   :ancestors ("t"))
(check (and (ancestor-has? "t") (ancestor-has? "f")) fails   :ancestors ("t"))
(check (and (ancestor-has? "t") (ancestor-has? "u")) unknown :ancestors unknown)
(check (and (under? "/a" "/b") (ancestor-has? "u")) fails   :ancestors unknown)

(check (or (ancestor-has? "f") (ancestor-has? "f")) fails   :ancestors ("t"))
(check (or (ancestor-has? "f") (ancestor-has? "t")) holds   :ancestors ("t"))
(check (or (under? "/a" "/b") (ancestor-has? "u")) unknown :ancestors unknown)
(check (or (under? "/a" "/a") (ancestor-has? "u")) holds   :ancestors unknown)

(check (not (ancestor-has? "t")) fails   :ancestors ("t"))
(check (not (ancestor-has? "f")) holds   :ancestors ("t"))
(check (not (ancestor-has? "u")) unknown :ancestors unknown)

;; Nesting composes.
(check (and (ancestor-has? ".jj") (not (or (under? "/a" "/b") (under? "/c" "/d"))))
       holds :ancestors (".jj"))

;; --- facts by name (ADR 0003) ---

;; Any fact the registry knows can be named, with any arguments it
;; accepts; a name it does not know is a load error, not an unknown.
(check (in-git? "x") holds :facts ((in-git? holds)))
(check (in-git?)     fails :facts ((in-git? fails)))
(check (slow?)       unknown :facts ((slow? unknown "timed out after 1s")))

;; --- reasons travel with the answer ---

;; A fact's own reason comes through as is. An unknown's names the fact,
;; so the evidence an ask carries says which fact could not be settled.
(check (a?) holds   "a held"    :facts ((a? holds "a held")))
(check (u?) unknown "u? is unknown: u timed out" :facts ((u? unknown "u timed out")))

;; An unknown `and` or `or` carries the first unknown, in evaluation order.
(check (and (a?) (u?) (v?)) unknown "u? is unknown: u timed out"
       :facts ((a? holds "a held") (u? unknown "u timed out") (v? unknown "v timed out")))
(check (or (f?) (v?) (u?)) unknown "v? is unknown: v timed out"
       :facts ((f? fails "f failed") (u? unknown "u timed out") (v? unknown "v timed out")))

;; The part that settled it carries its reason: one false for an `and`,
;; one true for an `or`.
(check (and (a?) (u?) (f?)) fails "f failed"
       :facts ((a? holds "a held") (u? unknown "u timed out") (f? fails "f failed")))
(check (or (f?) (u?) (a?)) holds "a held"
       :facts ((a? holds "a held") (u? unknown "u timed out") (f? fails "f failed")))

;; When every part agrees, their reasons are joined; parts with none are
;; left out.
(check (and (a?) (t?) (b?)) holds "a held; b held"
       :facts ((a? holds "a held") (t? holds) (b? holds "b held")))
(check (or (f?) (g?)) fails "f failed; g failed"
       :facts ((f? fails "f failed") (g? fails "g failed")))

;; `not` flips the truth and keeps the reason.
(check (not (f?)) holds   "f failed"    :facts ((f? fails "f failed")))
(check (not (u?)) unknown "u? is unknown: u timed out" :facts ((u? unknown "u timed out")))

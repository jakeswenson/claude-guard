;; Conditions: what a row's `:when` evaluates to.
;;
;; A check is (check (condition) holds|fails|unknown ...). `:with` gives
;; the binding set the row's pattern produced. `:cwd` is the call's cwd,
;; default "/spec". `:ancestors` lists the entries some ancestor of cwd
;; has; everything else does not exist. `:ancestors unknown` makes the
;; filesystem answer unknown, which is what a timed-out extern will do.

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

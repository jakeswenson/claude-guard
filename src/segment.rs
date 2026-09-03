//! Flatten a shell command into the simple commands the rules look at.
//!
//! brush-parser produces a full bash AST. The rules only care about one
//! question per simple command: what are its words, and what does it
//! redirect to. This module walks the tree and answers that, in source
//! order, for every simple command it can reach.
//!
//! What is walked: pipelines, `&&`/`||`/`;`/`&` lists, subshells, brace
//! groups, `for`/`while`/`until`/`if`/`case` bodies, function bodies,
//! coprocesses, and process substitutions like `<(cmd)`, which the parser
//! already hands us as a tree.
//!
//! What is not walked, and why:
//! - `$(cmd)` and `` `cmd` ``: their text would need a second parse. The
//!   word that holds them becomes [`Word::Dynamic`] and the inner text is
//!   reported in [`Segments::uninspected`] so a rule can warn about it.
//! - heredoc bodies: data to the command, even when that command is `bash`.
//! - arithmetic commands and `[[ ]]` expressions: no commands live there
//!   short of a nested `$(...)`, which is the gap above.

use std::fmt;
use std::io::Cursor;

use brush_parser::ast;
use brush_parser::word::{self, WordPiece, WordPieceWithSource};
use brush_parser::{Parser, ParserOptions};

/// Everything the segmenter found in one Bash tool call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Segments {
    /// Simple commands in source order. A nested command follows the
    /// command that contains it.
    pub commands: Vec<SimpleCommand>,
    /// Source text of every `$(...)` and backquoted substitution, in
    /// source order. Nothing inside these was inspected.
    pub uninspected: Vec<String>,
}

/// One command the shell would exec: its words and its file redirects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SimpleCommand {
    /// Command name first, then arguments. Empty only for a redirect
    /// with no command, such as `( a; b ) > out` or a bare `> out`.
    pub words: Vec<Word>,
    pub redirects: Vec<Redirect>,
}

/// A word after quote removal, or the raw text when the shell would
/// expand it at run time and the guard cannot know the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Word {
    Literal(String),
    Dynamic(String),
}

/// A redirect that names a file. Fd duplication (`2>&1`), heredocs, and
/// here-strings never reach the rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub kind: RedirectKind,
    pub target: Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    /// `>`, `>|`, `<>`, `&>`, and `n>`.
    Write,
    /// `>>` and `&>>`.
    Append,
    /// `<` and `n<`.
    Read,
}

/// The parser could not make sense of the command. The message is
/// brush-parser's own, e.g. `syntax error at line 1 col 7`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentError(String);

impl fmt::Display for SegmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SegmentError {}

/// Parse `command` as bash and flatten it.
pub fn segment(command: &str) -> Result<Segments, SegmentError> {
    let options = ParserOptions::default();
    let program = Parser::new(Cursor::new(command.as_bytes()), &options)
        .parse_program()
        .map_err(|e| SegmentError(e.to_string()))?;

    let mut walker = Walker {
        options,
        out: Segments::default(),
    };
    for list in &program.complete_commands {
        walker.list(list)?;
    }
    Ok(walker.out)
}

/// Accumulates commands while descending the AST. Every `word` call goes
/// through [`Walker::fold`], which is where substitutions get reported.
struct Walker {
    options: ParserOptions,
    out: Segments,
}

impl Walker {
    fn list(&mut self, list: &ast::CompoundList) -> Result<(), SegmentError> {
        for ast::CompoundListItem(and_or, _separator) in &list.0 {
            self.pipeline(&and_or.first)?;
            for next in &and_or.additional {
                let (ast::AndOr::And(p) | ast::AndOr::Or(p)) = next;
                self.pipeline(p)?;
            }
        }
        Ok(())
    }

    fn pipeline(&mut self, pipeline: &ast::Pipeline) -> Result<(), SegmentError> {
        for command in &pipeline.seq {
            self.command(command)?;
        }
        Ok(())
    }

    fn command(&mut self, command: &ast::Command) -> Result<(), SegmentError> {
        match command {
            ast::Command::Simple(simple) => self.simple(simple),
            ast::Command::Compound(compound, redirects) => {
                self.compound(compound)?;
                self.trailing_redirects(redirects.as_ref())
            }
            ast::Command::Function(def) => {
                let ast::FunctionBody(compound, redirects) = &def.body;
                self.compound(compound)?;
                self.trailing_redirects(redirects.as_ref())
            }
            // The test expression is not walked; see the module docs.
            ast::Command::ExtendedTest(_, redirects) => self.trailing_redirects(redirects.as_ref()),
        }
    }

    fn compound(&mut self, compound: &ast::CompoundCommand) -> Result<(), SegmentError> {
        use ast::CompoundCommand::*;
        match compound {
            Arithmetic(_) => Ok(()),
            ArithmeticForClause(clause) => self.list(&clause.body.list),
            BraceGroup(group) => self.list(&group.list),
            Subshell(subshell) => self.list(&subshell.list),
            ForClause(clause) => {
                for value in clause.values.iter().flatten() {
                    self.fold(value)?;
                }
                self.list(&clause.body.list)
            }
            CaseClause(clause) => {
                self.fold(&clause.value)?;
                for item in &clause.cases {
                    for pattern in &item.patterns {
                        self.fold(pattern)?;
                    }
                    if let Some(body) = &item.cmd {
                        self.list(body)?;
                    }
                }
                Ok(())
            }
            IfClause(clause) => {
                self.list(&clause.condition)?;
                self.list(&clause.then)?;
                for else_clause in clause.elses.iter().flatten() {
                    if let Some(condition) = &else_clause.condition {
                        self.list(condition)?;
                    }
                    self.list(&else_clause.body)?;
                }
                Ok(())
            }
            WhileClause(clause) | UntilClause(clause) => {
                let ast::WhileOrUntilClauseCommand(condition, body, _) = clause;
                self.list(condition)?;
                self.list(&body.list)
            }
            Coprocess(coproc) => self.command(&coproc.body),
        }
    }

    /// Redirects on a compound command apply to the whole group, so they
    /// get an entry of their own with no words.
    fn trailing_redirects(
        &mut self,
        redirects: Option<&ast::RedirectList>,
    ) -> Result<(), SegmentError> {
        let Some(redirects) = redirects else {
            return Ok(());
        };
        let mut entry = SimpleCommand::default();
        let mut nested = Vec::new();
        for redirect in &redirects.0 {
            self.redirect(redirect, &mut entry, &mut nested)?;
        }
        self.emit(entry, nested)
    }

    fn simple(&mut self, simple: &ast::SimpleCommand) -> Result<(), SegmentError> {
        let mut entry = SimpleCommand::default();
        let mut nested = Vec::new();

        let prefix = simple.prefix.iter().flat_map(|p| &p.0);
        let suffix = simple.suffix.iter().flat_map(|s| &s.0);
        for item in prefix {
            self.item(item, &mut entry, &mut nested)?;
        }
        if let Some(name) = &simple.word_or_name {
            entry.words.push(self.fold(name)?);
        }
        for item in suffix {
            self.item(item, &mut entry, &mut nested)?;
        }
        self.emit(entry, nested)
    }

    fn item(
        &mut self,
        item: &ast::CommandPrefixOrSuffixItem,
        entry: &mut SimpleCommand,
        nested: &mut Vec<ast::SubshellCommand>,
    ) -> Result<(), SegmentError> {
        use ast::CommandPrefixOrSuffixItem as Item;
        match item {
            Item::Word(w) => entry.words.push(self.fold(w)?),
            Item::IoRedirect(redirect) => self.redirect(redirect, entry, nested)?,
            // `FOO=bar cmd`: the assignment is dropped, but its value may
            // hold a substitution worth reporting.
            Item::AssignmentWord(assignment, _) => match &assignment.value {
                ast::AssignmentValue::Scalar(w) => {
                    self.fold(w)?;
                }
                ast::AssignmentValue::Array(elements) => {
                    for (key, value) in elements {
                        if let Some(key) = key {
                            self.fold(key)?;
                        }
                        self.fold(value)?;
                    }
                }
            },
            Item::ProcessSubstitution(kind, subshell) => {
                entry.words.push(Word::Dynamic(format!("{kind}({})", subshell.list)));
                nested.push(subshell.clone());
            }
        }
        Ok(())
    }

    fn redirect(
        &mut self,
        redirect: &ast::IoRedirect,
        entry: &mut SimpleCommand,
        nested: &mut Vec<ast::SubshellCommand>,
    ) -> Result<(), SegmentError> {
        use ast::IoFileRedirectKind as K;
        use ast::IoFileRedirectTarget as T;
        match redirect {
            ast::IoRedirect::File(_, kind, target) => {
                let kind = match kind {
                    K::Write | K::Clobber | K::ReadAndWrite => RedirectKind::Write,
                    K::Append => RedirectKind::Append,
                    K::Read => RedirectKind::Read,
                    K::DuplicateInput | K::DuplicateOutput => return Ok(()),
                };
                let target = match target {
                    T::Filename(w) => self.fold(w)?,
                    T::ProcessSubstitution(sub_kind, subshell) => {
                        nested.push(subshell.clone());
                        Word::Dynamic(format!("{sub_kind}({})", subshell.list))
                    }
                    T::Fd(_) | T::Duplicate(_) => return Ok(()),
                };
                entry.redirects.push(Redirect { kind, target });
            }
            ast::IoRedirect::OutputAndError(w, append) => {
                let kind = if *append {
                    RedirectKind::Append
                } else {
                    RedirectKind::Write
                };
                let target = self.fold(w)?;
                entry.redirects.push(Redirect { kind, target });
            }
            // Input, not a file. The word may still carry a substitution.
            ast::IoRedirect::HereString(_, w) => {
                self.fold(w)?;
            }
            // The body is data; see the module docs.
            ast::IoRedirect::HereDocument(_, _) => {}
        }
        Ok(())
    }

    /// Push a finished command, then walk the process substitutions it
    /// contained so their commands land right after it.
    fn emit(
        &mut self,
        entry: SimpleCommand,
        nested: Vec<ast::SubshellCommand>,
    ) -> Result<(), SegmentError> {
        if !entry.words.is_empty() || !entry.redirects.is_empty() {
            self.out.commands.push(entry);
        }
        for subshell in &nested {
            self.list(&subshell.list)?;
        }
        Ok(())
    }

    /// Quote removal. Every piece the shell would expand at run time makes
    /// the word dynamic, and every command substitution is reported.
    fn fold(&mut self, w: &ast::Word) -> Result<Word, SegmentError> {
        let pieces = word::parse(&w.value, &self.options).map_err(|e| SegmentError(e.to_string()))?;
        let mut literal = String::new();
        let mut dynamic = false;
        self.fold_pieces(&pieces, &mut literal, &mut dynamic);
        Ok(if dynamic {
            Word::Dynamic(w.value.clone())
        } else {
            Word::Literal(literal)
        })
    }

    fn fold_pieces(&mut self, pieces: &[WordPieceWithSource], literal: &mut String, dynamic: &mut bool) {
        for WordPieceWithSource { piece, .. } in pieces {
            match piece {
                WordPiece::Text(s) | WordPiece::SingleQuotedText(s) | WordPiece::AnsiCQuotedText(s) => {
                    literal.push_str(s);
                }
                // `\x` arrives as the two characters; the shell keeps `x`.
                WordPiece::EscapeSequence(s) => literal.push_str(s.strip_prefix('\\').unwrap_or(s)),
                WordPiece::DoubleQuotedSequence(inner) | WordPiece::GettextDoubleQuotedSequence(inner) => {
                    self.fold_pieces(inner, literal, dynamic);
                }
                WordPiece::CommandSubstitution(text) | WordPiece::BackquotedCommandSubstitution(text) => {
                    self.out.uninspected.push(text.clone());
                    *dynamic = true;
                }
                WordPiece::TildeExpansion(_)
                | WordPiece::ParameterExpansion(_)
                | WordPiece::ArithmeticExpression(_) => *dynamic = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Word {
        Word::Literal(s.into())
    }

    fn dynamic(s: &str) -> Word {
        Word::Dynamic(s.into())
    }

    /// A command of literal words and no redirects.
    fn cmd(words: &[&str]) -> SimpleCommand {
        SimpleCommand {
            words: words.iter().map(|w| lit(w)).collect(),
            redirects: vec![],
        }
    }

    fn commands(input: &str) -> Vec<SimpleCommand> {
        segment(input).unwrap().commands
    }

    fn redirect(kind: RedirectKind, target: Word) -> Redirect {
        Redirect { kind, target }
    }

    #[test]
    fn a_single_command_splits_into_words() {
        assert_eq!(commands("git stash"), vec![cmd(&["git", "stash"])]);
    }

    #[test]
    fn list_and_pipe_operators_split_commands() {
        assert_eq!(
            commands("a && b || c; d & e | f |& g"),
            ["a", "b", "c", "d", "e", "f", "g"]
                .iter()
                .map(|w| cmd(&[w]))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_real_test_and_filter_pipeline() {
        let input = r#"cd /Users/jakes/code/personal/jakes/WinDough; cargo nextest run -p windough-core 2>&1 | grep -E "^error|FAIL|Summary" -A 8 | head -30"#;
        assert_eq!(
            segment(input).unwrap(),
            Segments {
                commands: vec![
                    cmd(&["cd", "/Users/jakes/code/personal/jakes/WinDough"]),
                    cmd(&["cargo", "nextest", "run", "-p", "windough-core"]),
                    cmd(&["grep", "-E", "^error|FAIL|Summary", "-A", "8"]),
                    cmd(&["head", "-30"]),
                ],
                uninspected: vec![],
            }
        );
    }

    #[test]
    fn quotes_and_escapes_fold_into_one_literal() {
        assert_eq!(
            commands(r#"git "sta"sh 'x y' a\ b "c"'d'e"#),
            vec![cmd(&["git", "stash", "x y", "a b", "cde"])]
        );
    }

    #[test]
    fn expansions_make_the_whole_word_dynamic() {
        assert_eq!(
            commands(r#"echo $HOME ~/x "pre-$f" $((1+1)) plain"#),
            vec![SimpleCommand {
                words: vec![
                    lit("echo"),
                    dynamic("$HOME"),
                    dynamic("~/x"),
                    dynamic(r#""pre-$f""#),
                    dynamic("$((1+1))"),
                    lit("plain"),
                ],
                redirects: vec![],
            }]
        );
    }

    #[test]
    fn command_substitutions_are_dynamic_and_reported() {
        let input = "echo $(git stash) `date` \"$(jj log)\"";
        assert_eq!(
            segment(input).unwrap(),
            Segments {
                commands: vec![SimpleCommand {
                    words: vec![
                        lit("echo"),
                        dynamic("$(git stash)"),
                        dynamic("`date`"),
                        dynamic("\"$(jj log)\""),
                    ],
                    redirects: vec![],
                }],
                uninspected: vec!["git stash".into(), "date".into(), "jj log".into()],
            }
        );
    }

    #[test]
    fn substitutions_in_assignments_and_here_strings_are_reported() {
        let input = "FOO=$(git rev-parse HEAD) X=1 cargo build <<< \"$(date)\"";
        assert_eq!(
            segment(input).unwrap(),
            Segments {
                commands: vec![cmd(&["cargo", "build"])],
                uninspected: vec!["git rev-parse HEAD".into(), "date".into()],
            }
        );
    }

    #[test]
    fn process_substitutions_are_walked_after_their_command() {
        assert_eq!(
            commands("diff <(git log) <(jj log)"),
            vec![
                SimpleCommand {
                    words: vec![lit("diff"), dynamic("<(git log)"), dynamic("<(jj log)")],
                    redirects: vec![],
                },
                cmd(&["git", "log"]),
                cmd(&["jj", "log"]),
            ]
        );
    }

    #[test]
    fn file_redirects_are_typed_and_fd_duplication_is_dropped() {
        assert_eq!(
            commands("x > a >> b < c 2> d &> e &>> f >| g <> h 2>&1 >&2"),
            vec![SimpleCommand {
                words: vec![lit("x")],
                redirects: vec![
                    redirect(RedirectKind::Write, lit("a")),
                    redirect(RedirectKind::Append, lit("b")),
                    redirect(RedirectKind::Read, lit("c")),
                    redirect(RedirectKind::Write, lit("d")),
                    redirect(RedirectKind::Write, lit("e")),
                    redirect(RedirectKind::Append, lit("f")),
                    redirect(RedirectKind::Write, lit("g")),
                    redirect(RedirectKind::Write, lit("h")),
                ],
            }]
        );
    }

    #[test]
    fn a_redirect_target_can_be_dynamic() {
        assert_eq!(
            commands(r#"echo hi > "$out" 2> >(tee err)"#),
            vec![
                SimpleCommand {
                    words: vec![lit("echo"), lit("hi")],
                    redirects: vec![
                        redirect(RedirectKind::Write, dynamic(r#""$out""#)),
                        redirect(RedirectKind::Write, dynamic(">(tee err)")),
                    ],
                },
                cmd(&["tee", "err"]),
            ]
        );
    }

    #[test]
    fn compound_bodies_are_walked_in_source_order() {
        let input = "for f in a b; do git add $f; done; \
                     if true; then jj st; elif false; then c; else d; fi; \
                     while :; do e; done; until :; do g; done; \
                     ( h ); { i; }; case $x in y) j;; *) k;; esac; \
                     fn() { l; }; coproc m";
        assert_eq!(
            commands(input),
            vec![
                SimpleCommand {
                    words: vec![lit("git"), lit("add"), dynamic("$f")],
                    redirects: vec![],
                },
                cmd(&["true"]),
                cmd(&["jj", "st"]),
                cmd(&["false"]),
                cmd(&["c"]),
                cmd(&["d"]),
                cmd(&[":"]),
                cmd(&["e"]),
                cmd(&[":"]),
                cmd(&["g"]),
                cmd(&["h"]),
                cmd(&["i"]),
                cmd(&["j"]),
                cmd(&["k"]),
                cmd(&["l"]),
                cmd(&["m"]),
            ]
        );
    }

    #[test]
    fn a_compound_redirect_becomes_a_wordless_entry() {
        assert_eq!(
            commands("( a; b ) > /tmp/out"),
            vec![
                cmd(&["a"]),
                cmd(&["b"]),
                SimpleCommand {
                    words: vec![],
                    redirects: vec![redirect(RedirectKind::Write, lit("/tmp/out"))],
                },
            ]
        );
    }

    #[test]
    fn a_bare_redirect_is_kept() {
        assert_eq!(
            commands("> /tmp/out"),
            vec![SimpleCommand {
                words: vec![],
                redirects: vec![redirect(RedirectKind::Write, lit("/tmp/out"))],
            }]
        );
    }

    #[test]
    fn assignment_only_and_empty_input_emit_nothing() {
        assert_eq!(segment("X=1").unwrap(), Segments::default());
        assert_eq!(segment("").unwrap(), Segments::default());
        assert_eq!(segment("# just a comment").unwrap(), Segments::default());
    }

    #[test]
    fn an_unclosed_quote_is_an_error() {
        let err = segment(r#"echo "oops"#).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unterminated double quote at 1,6 (detected near line 1 col 11)"
        );
    }

    #[test]
    fn a_dangling_operator_is_an_error() {
        let err = segment("git stash &&").unwrap_err();
        assert_eq!(err.to_string(), "syntax error at end of input");
    }

    #[test]
    #[ignore = "heredoc bodies are data and are not walked; follow-up task"]
    fn commands_fed_to_a_shell_through_a_heredoc_are_found() {
        assert!(commands("bash <<EOF\ngit stash\nEOF\n").contains(&cmd(&["git", "stash"])));
    }

    #[test]
    #[ignore = "[[ ]] expressions are not walked; follow-up task"]
    fn substitutions_inside_extended_tests_are_reported() {
        let found = segment("[[ -n $(git stash list) ]]").unwrap();
        assert_eq!(found.uninspected, vec!["git stash list".to_string()]);
    }
}

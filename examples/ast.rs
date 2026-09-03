//! Dump what brush-parser sees for a shell command.
//!
//! Usage:
//!   cargo run --example ast -- '<command>'         # program AST
//!   cargo run --example ast -- --word '<word>'     # word pieces
//!
//! Exists to check parser behavior on inputs the rules care about
//! before committing to how `segment.rs` walks the tree.

use std::io::Cursor;

use brush_parser::{Parser, ParserOptions, word};

fn main() {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let opts = ParserOptions::default();

  match args.as_slice() {
    [flag, w] if flag == "--word" => match word::parse(w, &opts) {
      Ok(pieces) => println!("{pieces:#?}"),
      Err(e) => println!("word parse error: {e}"),
    },
    [cmd] => {
      let mut parser = Parser::new(Cursor::new(cmd.as_bytes()), &opts);
      match parser.parse_program() {
        Ok(program) => println!("{program:#?}"),
        Err(e) => println!("parse error: {e}"),
      }
    }
    _ => {
      eprintln!("usage: cargo run --example ast -- '<command>' | --word '<word>'");
      std::process::exit(1);
    }
  }
}

# smoke

Asserts the crate compiles and its main types can be constructed.

## Key Components

- `lexer_and_parser_construct` - builds a `Lexer` over a string and a
  `Parser` over a single-`Eof` token vector, checking neither panics to
  construct.
- `ast_expr_constructs` - builds an `Expr::Literal` and checks it compares
  equal to an identically constructed value.

## Usage Example

```rust
#[test]
fn example_constructs_a_lexer() {
    let _lexer = sql::Lexer::new("SELECT 1;");
}
```

Run as part of `cargo test --workspace`; catches a broken build or a
signature change in `Lexer`/`Parser`/`Expr` before it surfaces indirectly
in a downstream crate's tests.

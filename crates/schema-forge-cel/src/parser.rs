//! Recursive-descent / precedence-climbing parser for the CEL expression grammar
//! (#107).
//!
//! Consumes the [`crate::lexer`] token stream and produces a typed
//! [`crate::ast::Expr`]. Iteration macros (`all`, `exists`, `exists_one`, `map`,
//! `filter`) and `has()` are lowered to comprehension / presence-test nodes at
//! parse time by the pure functions in the private [`macros`] submodule.

use crate::ast::{BinaryOp, Comprehension, Expr, Literal, UnaryOp};
use crate::error::{ParseError, Position};
use crate::lexer::{lex, Tok, Token};

/// Parse CEL `source` into a typed [`Expr`].
///
/// # Errors
/// Returns a positioned [`ParseError`] on any lexical or syntactic error.
pub fn parse(source: &str) -> Result<Expr, ParseError> {
    let tokens = lex(source)?;
    let mut parser = Parser { tokens, idx: 0 };
    let expr = parser.parse_expr()?;
    parser.expect(&Tok::Eof, "end of input")?;
    Ok(expr)
}

/// Re-export of [`crate::ast::unparse`] for callers using the parser module.
pub use crate::ast::unparse;

struct Parser {
    tokens: Vec<Token>,
    idx: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.idx].tok
    }

    fn peek_at(&self, ahead: usize) -> &Tok {
        match self.tokens.get(self.idx + ahead) {
            Some(t) => &t.tok,
            None => &Tok::Eof,
        }
    }

    fn pos(&self) -> Position {
        self.tokens[self.idx].pos
    }

    fn bump(&mut self) -> Tok {
        let tok = self.tokens[self.idx].tok.clone();
        if self.idx + 1 < self.tokens.len() {
            self.idx += 1;
        }
        tok
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == tok {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Tok, what: &str) -> Result<(), ParseError> {
        if self.peek() == tok {
            self.bump();
            Ok(())
        } else {
            Err(self.error_here(format!("expected {what}")))
        }
    }

    fn error_here(&self, message: impl Into<String>) -> ParseError {
        ParseError::with_position(message, self.pos())
    }

    // Expr = ConditionalOr ["?" ConditionalOr ":" Expr]
    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        let cond = self.parse_or()?;
        if self.eat(&Tok::Question) {
            let then = self.parse_or()?;
            self.expect(&Tok::Colon, "':' in conditional")?;
            let els = self.parse_expr()?;
            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            });
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while self.eat(&Tok::Or) {
            let rhs = self.parse_and()?;
            lhs = binary(BinaryOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_relation()?;
        while self.eat(&Tok::And) {
            let rhs = self.parse_relation()?;
            lhs = binary(BinaryOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_relation(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_addition()?;
        while let Some(op) = relop(self.peek()) {
            self.bump();
            let rhs = self.parse_addition()?;
            lhs = binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_addition(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_multiplication()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinaryOp::Add,
                Tok::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_multiplication()?;
            lhs = binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_multiplication(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinaryOp::Mul,
                Tok::Slash => BinaryOp::Div,
                Tok::Percent => BinaryOp::Rem,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = binary(op, lhs, rhs);
        }
        Ok(lhs)
    }

    // Unary = Member | "!" {"!"} Member | "-" {"-"} Member
    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        let op = match self.peek() {
            Tok::Not => UnaryOp::Not,
            Tok::Minus => UnaryOp::Neg,
            _ => return self.parse_member(),
        };
        // `-9223372036854775808` is `i64::MIN`: a unary minus directly applied to
        // the `2^63` magnitude folds to the literal rather than negating it (which
        // would overflow). The fold applies only to an immediately adjacent token.
        if op == UnaryOp::Neg && self.peek_at(1) == &Tok::IntMinMagnitude {
            self.bump(); // -
            self.bump(); // 2^63 magnitude
            return Ok(Expr::Literal(Literal::Int(i64::MIN)));
        }
        self.bump();
        // A run of the same operator nests; `parse_unary` recursion also handles
        // mixed runs such as `!-x` (Not over Neg over member).
        let operand = self.parse_unary()?;
        Ok(Expr::Unary {
            op,
            operand: Box::new(operand),
        })
    }

    // Member = Primary { "." IDENT ["(" args ")"] | "[" Expr "]" | "{" FieldInits "}" }
    fn parse_member(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    expr = self.parse_dot_suffix(expr)?;
                }
                Tok::LBrack => {
                    self.bump();
                    let index = self.parse_expr()?;
                    self.expect(&Tok::RBrack, "']' to close index")?;
                    expr = Expr::Index {
                        operand: Box::new(expr),
                        index: Box::new(index),
                    };
                }
                Tok::LBrace => {
                    // Message construction on a type-name member, e.g. `a.b.C{...}`.
                    let type_name = type_name_of(&expr).ok_or_else(|| {
                        self.error_here("struct construction requires a type name")
                    })?;
                    self.bump();
                    let fields = self.parse_field_inits()?;
                    self.expect(&Tok::RBrace, "'}' to close struct")?;
                    expr = Expr::Struct { type_name, fields };
                }
                _ => return Ok(expr),
            }
        }
    }

    /// Parse the suffix after a `.`: either a field selection or a method call.
    fn parse_dot_suffix(&mut self, operand: Expr) -> Result<Expr, ParseError> {
        let field = self.expect_member_name("field or method name after '.'")?;
        if self.eat(&Tok::LParen) {
            let args = self.parse_arg_list()?;
            self.expect(&Tok::RParen, "')' to close call")?;
            return macros::lower_method(operand, field, args);
        }
        Ok(Expr::Select {
            operand: Box::new(operand),
            field,
            test_only: false,
        })
    }

    // Primary
    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Tok::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(&Tok::RParen, "')' to close group")?;
                Ok(inner)
            }
            Tok::LBrack => self.parse_list(),
            Tok::LBrace => self.parse_map(),
            Tok::Dot | Tok::Ident(_) => self.parse_ident_primary(),
            _ => self.parse_literal_primary(),
        }
    }

    fn parse_literal_primary(&mut self) -> Result<Expr, ParseError> {
        let lit = match self.peek().clone() {
            Tok::Int(i) => Literal::Int(i),
            Tok::Uint(u) => Literal::Uint(u),
            Tok::Double(d) => Literal::Double(d),
            Tok::Str(s) => Literal::String(s),
            Tok::Bytes(b) => Literal::Bytes(b),
            Tok::True => Literal::Bool(true),
            Tok::False => Literal::Bool(false),
            Tok::Null => Literal::Null,
            Tok::IntMinMagnitude => {
                // `2^63` is only legal as the operand of unary minus (folded in
                // `parse_unary`); on its own it exceeds the `int` range.
                return Err(self.error_here("int literal out of range"));
            }
            Tok::Reserved(word) => {
                return Err(
                    self.error_here(format!("reserved identifier '{word}' cannot be used here"))
                );
            }
            _ => return Err(self.error_here("expected an expression")),
        };
        self.bump();
        Ok(Expr::Literal(lit))
    }

    /// Parse a primary beginning with an optional leading `.` and an identifier:
    /// a bare identifier, a global function call, or a (dotted) struct
    /// construction.
    fn parse_ident_primary(&mut self) -> Result<Expr, ParseError> {
        let leading_dot = self.eat(&Tok::Dot);
        let first = self.expect_ident("identifier")?;

        // Global function call: `name(args)` (only when not a dotted name).
        if self.peek() == &Tok::LParen {
            self.bump();
            let args = self.parse_arg_list()?;
            self.expect(&Tok::RParen, "')' to close call")?;
            return macros::lower_global(first, args);
        }

        // Look ahead for a dotted struct type: IDENT ("." IDENT)* "{".
        if let Some(type_name) = self.try_dotted_struct_name(leading_dot, &first) {
            self.expect(&Tok::LBrace, "'{' to open struct")?;
            let fields = self.parse_field_inits()?;
            self.expect(&Tok::RBrace, "'}' to close struct")?;
            return Ok(Expr::Struct { type_name, fields });
        }

        // Bare identifier struct: `Name{...}`.
        if self.peek() == &Tok::LBrace {
            self.bump();
            let fields = self.parse_field_inits()?;
            self.expect(&Tok::RBrace, "'}' to close struct")?;
            let type_name = if leading_dot {
                format!(".{first}")
            } else {
                first
            };
            return Ok(Expr::Struct { type_name, fields });
        }

        let name = if leading_dot {
            format!(".{first}")
        } else {
            first
        };
        Ok(Expr::Ident(name))
    }

    /// If the upcoming tokens form `("." IDENT)+ "{"`, consume the dotted suffix
    /// and return the assembled dotted type name. Otherwise consume nothing and
    /// return `None` (the `.` chain is left for the member loop to parse as
    /// selections).
    fn try_dotted_struct_name(&mut self, leading_dot: bool, first: &str) -> Option<String> {
        // Scan without consuming.
        let mut ahead = 0;
        let mut parts = 1usize;
        loop {
            if self.peek_at(ahead) != &Tok::Dot {
                break;
            }
            match self.peek_at(ahead + 1) {
                Tok::Ident(_) | Tok::Reserved(_) => {
                    ahead += 2;
                    parts += 1;
                }
                _ => break,
            }
        }
        if parts < 2 || self.peek_at(ahead) != &Tok::LBrace {
            return None;
        }
        // Commit: consume the dotted segments.
        let mut name = String::new();
        if leading_dot {
            name.push('.');
        }
        name.push_str(first);
        while self.peek() == &Tok::Dot {
            self.bump();
            let part = self
                .expect_member_name("name segment")
                .expect("lookahead guaranteed an ident");
            name.push('.');
            name.push_str(&part);
            if self.peek() == &Tok::LBrace {
                break;
            }
        }
        Some(name)
    }

    fn parse_list(&mut self) -> Result<Expr, ParseError> {
        self.bump(); // [
        let mut items = Vec::new();
        while self.peek() != &Tok::RBrack {
            items.push(self.parse_expr()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrack, "']' to close list")?;
        Ok(Expr::List(items))
    }

    fn parse_map(&mut self) -> Result<Expr, ParseError> {
        self.bump(); // {
        let mut entries = Vec::new();
        while self.peek() != &Tok::RBrace {
            let key = self.parse_expr()?;
            self.expect(&Tok::Colon, "':' in map entry")?;
            let value = self.parse_expr()?;
            entries.push((key, value));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RBrace, "'}' to close map")?;
        Ok(Expr::Map(entries))
    }

    // FieldInits = IDENT ":" Expr {"," IDENT ":" Expr}
    fn parse_field_inits(&mut self) -> Result<Vec<(String, Expr)>, ParseError> {
        let mut fields = Vec::new();
        while self.peek() != &Tok::RBrace {
            let name = self.expect_member_name("struct field name")?;
            self.expect(&Tok::Colon, "':' in struct field")?;
            let value = self.parse_expr()?;
            fields.push((name, value));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(fields)
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        while self.peek() != &Tok::RParen {
            args.push(self.parse_expr()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        Ok(args)
    }

    /// Consume a bare identifier (reserved words are NOT accepted here).
    fn expect_ident(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.bump();
                Ok(name)
            }
            Tok::Reserved(word) => {
                Err(self.error_here(format!("reserved identifier '{word}' cannot be used here")))
            }
            _ => Err(self.error_here(format!("expected {what}"))),
        }
    }

    /// Consume a member name: an identifier OR a reserved word (reserved words
    /// are permitted as field selectors, method names, and struct field names).
    fn expect_member_name(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek().clone() {
            Tok::Ident(name) | Tok::Reserved(name) => {
                self.bump();
                Ok(name)
            }
            _ => Err(self.error_here(format!("expected {what}"))),
        }
    }
}

fn binary(op: BinaryOp, lhs: Expr, rhs: Expr) -> Expr {
    Expr::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
    }
}

fn relop(tok: &Tok) -> Option<BinaryOp> {
    match tok {
        Tok::Lt => Some(BinaryOp::Lt),
        Tok::Le => Some(BinaryOp::Le),
        Tok::Gt => Some(BinaryOp::Gt),
        Tok::Ge => Some(BinaryOp::Ge),
        Tok::Eq => Some(BinaryOp::Eq),
        Tok::Ne => Some(BinaryOp::Ne),
        Tok::In => Some(BinaryOp::In),
        _ => None,
    }
}

/// Recover a dotted type name from an ident/select chain, used when a `{` follows
/// a member. Returns `None` if the expression is not a pure name chain.
fn type_name_of(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::Select {
            operand,
            field,
            test_only: false,
        } => {
            let base = type_name_of(operand)?;
            Some(format!("{base}.{field}"))
        }
        _ => None,
    }
}

/// Macro lowering: pure functions mapping macro call syntax to comprehension /
/// presence-test AST. Each only lowers when the name and argument shape match;
/// otherwise the call is left as an ordinary [`Expr::Call`].
mod macros {
    use super::{BinaryOp, Comprehension, Expr, Literal, ParseError, UnaryOp};

    /// The canonical cel-spec accumulator variable name.
    const ACCU: &str = "@result";

    /// Lower a global call. Only `has(...)` is a global macro.
    pub(super) fn lower_global(name: String, args: Vec<Expr>) -> Result<Expr, ParseError> {
        if name == "has" {
            return lower_has(args);
        }
        Ok(Expr::Call {
            target: None,
            function: name,
            args,
        })
    }

    /// Lower a method call. The iteration macros lower to comprehensions when the
    /// argument shape matches; otherwise the call is left as a method call.
    pub(super) fn lower_method(
        target: Expr,
        name: String,
        args: Vec<Expr>,
    ) -> Result<Expr, ParseError> {
        let lowered = match (name.as_str(), args.len()) {
            ("all", 2) => Some(lower_all(target.clone(), &args)),
            ("exists", 2) => Some(lower_exists(target.clone(), &args)),
            ("exists_one", 2) => Some(lower_exists_one(target.clone(), &args)),
            ("filter", 2) => Some(lower_filter(target.clone(), &args)),
            ("map", 2) => Some(lower_map2(target.clone(), &args)),
            ("map", 3) => Some(lower_map3(target.clone(), &args)),
            // Two-variable comprehension macros (cel-spec `macros2`). The first two
            // arguments bind the index/key and the element/value.
            ("all", 3) => Some(lower_all_v2(target.clone(), &args)),
            ("exists", 3) => Some(lower_exists_v2(target.clone(), &args)),
            ("existsOne", 3) => Some(lower_exists_one_v2(target.clone(), &args)),
            ("transformList", 3) => Some(lower_transform_list3(target.clone(), &args)),
            ("transformList", 4) => Some(lower_transform_list4(target.clone(), &args)),
            ("transformMap", 3) => Some(lower_transform_map3(target.clone(), &args)),
            ("transformMap", 4) => Some(lower_transform_map4(target.clone(), &args)),
            _ => None,
        };
        match lowered {
            Some(result) => result,
            None => Ok(Expr::Call {
                target: Some(Box::new(target)),
                function: name,
                args,
            }),
        }
    }

    fn lower_has(mut args: Vec<Expr>) -> Result<Expr, ParseError> {
        if args.len() != 1 {
            return Err(ParseError::new("has() requires exactly one argument"));
        }
        match args.pop().expect("len checked") {
            Expr::Select {
                operand,
                field,
                test_only: false,
            } => Ok(Expr::Select {
                operand,
                field,
                test_only: true,
            }),
            _ => Err(ParseError::new("has() requires a field selection argument")),
        }
    }

    /// Extract a loop variable name (must be a bare identifier) from `args[idx]`.
    fn iter_var_at(args: &[Expr], idx: usize) -> Result<String, ParseError> {
        match &args[idx] {
            Expr::Ident(name) => Ok(name.clone()),
            _ => Err(ParseError::new(
                "macro iteration variable must be a simple identifier",
            )),
        }
    }

    /// The single-variable loop variable (the macro's first argument).
    fn iter_var(args: &[Expr]) -> Result<String, ParseError> {
        iter_var_at(args, 0)
    }

    fn accu() -> Expr {
        Expr::Ident(ACCU.to_string())
    }

    /// The accumulator-defining parts of a comprehension: the cel-spec
    /// `accu_init` / `loop_condition` / `loop_step` / `result` quadruple. Grouped
    /// into one struct so the comprehension constructors stay within the
    /// argument-count lint and read as `(iteration) over (range) accumulating (loop)`.
    struct Loop {
        init: Expr,
        cond: Expr,
        step: Expr,
        result: Expr,
    }

    fn comprehension(var: String, range: Expr, lp: Loop) -> Expr {
        comprehension2(var, None, range, lp)
    }

    fn comprehension2(var: String, var2: Option<String>, range: Expr, lp: Loop) -> Expr {
        Expr::Comprehension(Box::new(Comprehension {
            iter_var: var,
            iter_var2: var2,
            iter_range: range,
            accu_var: ACCU.to_string(),
            accu_init: lp.init,
            loop_condition: lp.cond,
            loop_step: lp.step,
            result: lp.result,
        }))
    }

    // all(x, p): init true; while @result; step @result && p; result @result.
    //
    // The loop_condition stays a bare `@result` (resp. `!@result` for `exists`),
    // NOT cel-spec's `@not_strictly_false(@result)` wrapper. The corresponding
    // error-absorption ("a predicate error on one element must not abort when a
    // later element already determines the result") is implemented in the
    // evaluator's comprehension loop (`eval::eval_comprehension`), which defers a
    // predicate error and discards it if the loop reaches a determinate
    // accumulator. Keeping the lowering minimal means the evaluator owns the one
    // source of truth for absorption.
    fn lower_all(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let var = iter_var(args)?;
        Ok(comprehension(var, range, all_loop(args[1].clone())))
    }

    // exists(x, p): init false; while !@result; step @result || p; result @result.
    fn lower_exists(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let var = iter_var(args)?;
        Ok(comprehension(var, range, exists_loop(args[1].clone())))
    }

    // exists_one(x, p): init 0; step p ? @result + 1 : @result; result @result == 1.
    fn lower_exists_one(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let var = iter_var(args)?;
        Ok(comprehension(var, range, exists_one_loop(args[1].clone())))
    }

    // map(x, t): init []; step @result + [t]; result @result.
    fn lower_map2(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let var = iter_var(args)?;
        Ok(comprehension(var, range, list_build_loop(args[1].clone())))
    }

    // map(x, p, t): init []; step p ? @result + [t] : @result; result @result.
    fn lower_map3(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let var = iter_var(args)?;
        let lp = list_build_filtered_loop(args[1].clone(), args[2].clone());
        Ok(comprehension(var, range, lp))
    }

    // filter(x, p): init []; step p ? @result + [x] : @result; result @result.
    fn lower_filter(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let var = iter_var(args)?;
        let lp = list_build_filtered_loop(args[1].clone(), Expr::Ident(var.clone()));
        Ok(comprehension(var, range, lp))
    }

    // -- Two-variable macros (cel-spec `macros2`). --
    //
    // The loop shapes mirror the single-variable forms exactly; the only
    // difference is the second iteration variable, materialized by the evaluator.

    fn lower_all_v2(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let (v1, v2) = iter_vars2(args)?;
        Ok(comprehension2(
            v1,
            Some(v2),
            range,
            all_loop(args[2].clone()),
        ))
    }

    fn lower_exists_v2(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let (v1, v2) = iter_vars2(args)?;
        Ok(comprehension2(
            v1,
            Some(v2),
            range,
            exists_loop(args[2].clone()),
        ))
    }

    fn lower_exists_one_v2(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let (v1, v2) = iter_vars2(args)?;
        Ok(comprehension2(
            v1,
            Some(v2),
            range,
            exists_one_loop(args[2].clone()),
        ))
    }

    // transformList(i, v, t): init []; step @result + [t]; result @result.
    fn lower_transform_list3(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let (v1, v2) = iter_vars2(args)?;
        Ok(comprehension2(
            v1,
            Some(v2),
            range,
            list_build_loop(args[2].clone()),
        ))
    }

    // transformList(i, v, f, t): init []; step f ? @result + [t] : @result.
    fn lower_transform_list4(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let (v1, v2) = iter_vars2(args)?;
        let lp = list_build_filtered_loop(args[2].clone(), args[3].clone());
        Ok(comprehension2(v1, Some(v2), range, lp))
    }

    // transformMap(k, v, t): init {}; step @mapInsert(@result, k, t); result @result.
    fn lower_transform_map3(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let (v1, v2) = iter_vars2(args)?;
        let step = map_insert_step(&v1, args[2].clone());
        let lp = Loop {
            init: Expr::Map(Vec::new()),
            cond: Expr::Literal(Literal::Bool(true)),
            step,
            result: accu(),
        };
        Ok(comprehension2(v1, Some(v2), range, lp))
    }

    // transformMap(k, v, f, t): init {}; step f ? @mapInsert(@result, k, t) : @result.
    fn lower_transform_map4(range: Expr, args: &[Expr]) -> Result<Expr, ParseError> {
        let (v1, v2) = iter_vars2(args)?;
        let insert = map_insert_step(&v1, args[3].clone());
        let step = Expr::Ternary {
            cond: Box::new(args[2].clone()),
            then: Box::new(insert),
            els: Box::new(accu()),
        };
        let lp = Loop {
            init: Expr::Map(Vec::new()),
            cond: Expr::Literal(Literal::Bool(true)),
            step,
            result: accu(),
        };
        Ok(comprehension2(v1, Some(v2), range, lp))
    }

    /// Extract the two iteration variable names (both must be bare identifiers).
    fn iter_vars2(args: &[Expr]) -> Result<(String, String), ParseError> {
        Ok((iter_var_at(args, 0)?, iter_var_at(args, 1)?))
    }

    /// Build the internal `@mapInsert(@result, key, value)` step call. `@mapInsert`
    /// is an engine-internal function (the `@` prefix is unspellable in surface
    /// CEL) dispatched only from generated `transformMap` steps.
    fn map_insert_step(key_var: &str, value: Expr) -> Expr {
        Expr::Call {
            target: None,
            function: "@mapInsert".to_string(),
            args: vec![accu(), Expr::Ident(key_var.to_string()), value],
        }
    }

    fn all_loop(pred: Expr) -> Loop {
        Loop {
            init: Expr::Literal(Literal::Bool(true)),
            cond: accu(),
            step: super::binary(BinaryOp::And, accu(), pred),
            result: accu(),
        }
    }

    fn exists_loop(pred: Expr) -> Loop {
        Loop {
            init: Expr::Literal(Literal::Bool(false)),
            cond: Expr::Unary {
                op: UnaryOp::Not,
                operand: Box::new(accu()),
            },
            step: super::binary(BinaryOp::Or, accu(), pred),
            result: accu(),
        }
    }

    fn exists_one_loop(pred: Expr) -> Loop {
        Loop {
            init: Expr::Literal(Literal::Int(0)),
            cond: Expr::Literal(Literal::Bool(true)),
            step: Expr::Ternary {
                cond: Box::new(pred),
                then: Box::new(super::binary(
                    BinaryOp::Add,
                    accu(),
                    Expr::Literal(Literal::Int(1)),
                )),
                els: Box::new(accu()),
            },
            result: super::binary(BinaryOp::Eq, accu(), Expr::Literal(Literal::Int(1))),
        }
    }

    fn list_build_loop(transform: Expr) -> Loop {
        Loop {
            init: Expr::List(Vec::new()),
            cond: Expr::Literal(Literal::Bool(true)),
            step: super::binary(BinaryOp::Add, accu(), Expr::List(vec![transform])),
            result: accu(),
        }
    }

    fn list_build_filtered_loop(pred: Expr, transform: Expr) -> Loop {
        Loop {
            init: Expr::List(Vec::new()),
            cond: Expr::Literal(Literal::Bool(true)),
            step: Expr::Ternary {
                cond: Box::new(pred),
                then: Box::new(super::binary(
                    BinaryOp::Add,
                    accu(),
                    Expr::List(vec![transform]),
                )),
                els: Box::new(accu()),
            },
            result: accu(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Expr {
        parse(src).unwrap_or_else(|e| panic!("parse {src:?} failed: {e}"))
    }

    #[test]
    fn precedence_mul_over_add() {
        assert_eq!(
            p("1 + 2 * 3"),
            binary(
                BinaryOp::Add,
                Expr::Literal(Literal::Int(1)),
                binary(
                    BinaryOp::Mul,
                    Expr::Literal(Literal::Int(2)),
                    Expr::Literal(Literal::Int(3)),
                ),
            )
        );
    }

    #[test]
    fn precedence_and_over_or() {
        // a || b && c == a || (b && c)
        assert_eq!(
            p("a || b && c"),
            binary(
                BinaryOp::Or,
                Expr::Ident("a".into()),
                binary(
                    BinaryOp::And,
                    Expr::Ident("b".into()),
                    Expr::Ident("c".into())
                ),
            )
        );
    }

    #[test]
    fn subtraction_is_left_associative() {
        // 1 - 2 - 3 == (1 - 2) - 3
        assert_eq!(
            p("1 - 2 - 3"),
            binary(
                BinaryOp::Sub,
                binary(
                    BinaryOp::Sub,
                    Expr::Literal(Literal::Int(1)),
                    Expr::Literal(Literal::Int(2)),
                ),
                Expr::Literal(Literal::Int(3)),
            )
        );
    }

    #[test]
    fn ternary_nesting_right_associative() {
        // a ? b : c ? d : e == a ? b : (c ? d : e)
        let parsed = p("a ? b : c ? d : e");
        if let Expr::Ternary { els, .. } = parsed {
            assert!(matches!(*els, Expr::Ternary { .. }));
        } else {
            panic!("expected ternary");
        }
    }

    #[test]
    fn unary_chains() {
        assert_eq!(
            p("!!x"),
            Expr::Unary {
                op: UnaryOp::Not,
                operand: Box::new(Expr::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(Expr::Ident("x".into())),
                }),
            }
        );
        // --19 nests two negations.
        assert!(matches!(
            p("--19"),
            Expr::Unary {
                op: UnaryOp::Neg,
                ..
            }
        ));
    }

    #[test]
    fn in_relop() {
        assert!(matches!(
            p("x in y"),
            Expr::Binary {
                op: BinaryOp::In,
                ..
            }
        ));
    }

    #[test]
    fn list_map_struct() {
        assert_eq!(
            p("[1, 2, 3]"),
            Expr::List(vec![
                Expr::Literal(Literal::Int(1)),
                Expr::Literal(Literal::Int(2)),
                Expr::Literal(Literal::Int(3)),
            ])
        );
        assert!(matches!(p("{'a': 1}"), Expr::Map(_)));
        match p("Foo{bar: 1}") {
            Expr::Struct { type_name, fields } => {
                assert_eq!(type_name, "Foo");
                assert_eq!(fields.len(), 1);
            }
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn dotted_struct_construction() {
        match p("a.b.C{x: 1}") {
            Expr::Struct { type_name, .. } => assert_eq!(type_name, "a.b.C"),
            other => panic!("expected dotted struct, got {other:?}"),
        }
    }

    #[test]
    fn trailing_comma_in_list() {
        assert_eq!(
            p("[1, 2,]"),
            Expr::List(vec![
                Expr::Literal(Literal::Int(1)),
                Expr::Literal(Literal::Int(2)),
            ])
        );
    }

    #[test]
    fn select_index_and_calls() {
        assert!(matches!(
            p("a.b"),
            Expr::Select {
                test_only: false,
                ..
            }
        ));
        assert!(matches!(p("a[0]"), Expr::Index { .. }));
        assert!(matches!(p("f(1, 2)"), Expr::Call { target: None, .. }));
        assert!(matches!(
            p("a.f(1)"),
            Expr::Call {
                target: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn leading_dot_ident() {
        assert_eq!(p(".x"), Expr::Ident(".x".into()));
    }

    #[test]
    fn int_min_literal_folds() {
        // -9223372036854775808 is i64::MIN, folded directly (not Neg of 2^63).
        assert_eq!(
            p("-9223372036854775808"),
            Expr::Literal(Literal::Int(i64::MIN))
        );
        // The bare 2^63 magnitude (no minus) is out of range.
        assert!(parse("9223372036854775808").is_err());
        // i64::MAX still parses normally.
        assert_eq!(
            p("9223372036854775807"),
            Expr::Literal(Literal::Int(i64::MAX))
        );
    }

    #[test]
    fn reserved_word_as_selector_method_field() {
        // selector
        assert!(matches!(p("{'as': 1}.as"), Expr::Select { .. }));
        // receiver method
        assert!(matches!(p("a.break() || true"), Expr::Binary { .. }));
        // struct field name
        match p("Foo{if: true}") {
            Expr::Struct { fields, .. } => assert_eq!(fields[0].0, "if"),
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn reserved_word_as_bare_ident_errors() {
        let err = parse("break").unwrap_err();
        assert!(err.position().is_some());
        assert!(parse("while + 1").is_err());
    }

    #[test]
    fn has_lowers_to_test_only_select() {
        assert_eq!(
            p("has(a.b)"),
            Expr::Select {
                operand: Box::new(Expr::Ident("a".into())),
                field: "b".into(),
                test_only: true,
            }
        );
    }

    #[test]
    fn has_rejects_non_selection() {
        let err = parse("has(a)").unwrap_err();
        assert_eq!(err.message(), "has() requires a field selection argument");
    }

    #[test]
    fn all_lowers_to_comprehension() {
        match p("e.all(x, x > 0)") {
            Expr::Comprehension(c) => {
                assert_eq!(c.iter_var, "x");
                assert_eq!(c.accu_var, "@result");
                assert_eq!(c.accu_init, Expr::Literal(Literal::Bool(true)));
                assert_eq!(c.result, Expr::Ident("@result".into()));
                // loop_step = @result && (x > 0)
                match &c.loop_step {
                    Expr::Binary {
                        op: BinaryOp::And,
                        lhs,
                        ..
                    } => {
                        assert_eq!(**lhs, Expr::Ident("@result".into()));
                    }
                    other => panic!("expected && step, got {other:?}"),
                }
            }
            other => panic!("expected comprehension, got {other:?}"),
        }
    }

    #[test]
    fn exists_one_uses_int_accumulator() {
        match p("e.exists_one(x, x > 0)") {
            Expr::Comprehension(c) => {
                assert_eq!(c.accu_init, Expr::Literal(Literal::Int(0)));
                assert_eq!(
                    c.result,
                    binary(
                        BinaryOp::Eq,
                        Expr::Ident("@result".into()),
                        Expr::Literal(Literal::Int(1))
                    )
                );
            }
            other => panic!("expected comprehension, got {other:?}"),
        }
    }

    #[test]
    fn map_and_filter_build_lists() {
        assert!(matches!(p("e.map(x, x + 1)"), Expr::Comprehension(_)));
        assert!(matches!(
            p("e.map(x, x > 0, x + 1)"),
            Expr::Comprehension(_)
        ));
        assert!(matches!(p("e.filter(x, x > 0)"), Expr::Comprehension(_)));
    }

    #[test]
    fn macro_falls_back_to_call_on_wrong_arity() {
        // map/1 is not a macro shape; stays a method call.
        assert!(matches!(
            p("e.map(x)"),
            Expr::Call {
                target: Some(_),
                ..
            }
        ));
        // a name that isn't a macro stays a call.
        assert!(matches!(
            p("e.foo(x, y)"),
            Expr::Call {
                target: Some(_),
                ..
            }
        ));
    }

    /// A two-variable macro lowers to a comprehension carrying `iter_var2`.
    fn two_var(src: &str) -> Comprehension {
        match p(src) {
            Expr::Comprehension(c) => {
                assert!(c.iter_var2.is_some(), "expected iter_var2 for {src:?}");
                *c
            }
            other => panic!("expected two-var comprehension for {src:?}, got {other:?}"),
        }
    }

    #[test]
    fn two_var_all_exists_existsone_lower() {
        let all = two_var("e.all(i, v, v > i)");
        assert_eq!(all.iter_var, "i");
        assert_eq!(all.iter_var2.as_deref(), Some("v"));
        assert_eq!(all.accu_init, Expr::Literal(Literal::Bool(true)));

        let exists = two_var("e.exists(i, v, v > i)");
        assert_eq!(exists.accu_init, Expr::Literal(Literal::Bool(false)));

        let one = two_var("e.existsOne(i, v, v > i)");
        assert_eq!(one.accu_init, Expr::Literal(Literal::Int(0)));
        assert_eq!(
            one.result,
            binary(
                BinaryOp::Eq,
                Expr::Ident("@result".into()),
                Expr::Literal(Literal::Int(1))
            )
        );
    }

    #[test]
    fn transform_list_lowers_3_and_4_arg() {
        let three = two_var("e.transformList(i, v, v + i)");
        assert_eq!(three.accu_init, Expr::List(Vec::new()));
        // 3-arg step is `@result + [v + i]`.
        assert!(matches!(
            three.loop_step,
            Expr::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));
        let four = two_var("e.transformList(i, v, i > 0, v + i)");
        // 4-arg step is a filter ternary.
        assert!(matches!(four.loop_step, Expr::Ternary { .. }));
    }

    #[test]
    fn transform_map_lowers_to_map_insert() {
        let three = two_var("e.transformMap(k, v, k + v)");
        assert_eq!(three.accu_init, Expr::Map(Vec::new()));
        match &three.loop_step {
            Expr::Call {
                target: None,
                function,
                args,
            } => {
                assert_eq!(function, "@mapInsert");
                assert_eq!(args.len(), 3);
                assert_eq!(args[0], Expr::Ident("@result".into()));
                assert_eq!(args[1], Expr::Ident("k".into()));
            }
            other => panic!("expected @mapInsert step, got {other:?}"),
        }
        // 4-arg form wraps the insert in a filter ternary.
        let four = two_var("e.transformMap(k, v, k != 'x', k + v)");
        assert!(matches!(four.loop_step, Expr::Ternary { .. }));
    }

    #[test]
    fn two_var_iteration_var_must_be_ident() {
        // Non-ident first var.
        assert!(parse("e.all(1, v, v > 0)").is_err());
        // Non-ident second var.
        assert!(parse("e.all(i, 2, i > 0)").is_err());
    }

    #[test]
    fn two_var_wrong_arity_falls_back_to_call() {
        // transformList needs 3 or 4 args; 2 args is an ordinary method call.
        assert!(matches!(
            p("e.transformList(i, v)"),
            Expr::Call {
                target: Some(_),
                ..
            }
        ));
        // existsOne with 4 args is not a defined shape → method call.
        assert!(matches!(
            p("e.existsOne(i, v, p, q)"),
            Expr::Call {
                target: Some(_),
                ..
            }
        ));
        // transformMap with 5 args → method call.
        assert!(matches!(
            p("e.transformMap(k, v, a, b, c)"),
            Expr::Call {
                target: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn single_var_macros_unaffected_by_two_var_addition() {
        // The 2-arg single-variable forms still lower with iter_var2 == None.
        for src in [
            "e.all(x, x > 0)",
            "e.exists(x, x > 0)",
            "e.exists_one(x, x > 0)",
            "e.map(x, x + 1)",
            "e.filter(x, x > 0)",
        ] {
            match p(src) {
                Expr::Comprehension(c) => assert!(c.iter_var2.is_none(), "{src:?}"),
                other => panic!("expected comprehension for {src:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_errors_carry_position() {
        for bad in ["1 +", "(1", "[1, 2", "{1:", "a.", "f(1,"] {
            let err = parse(bad).unwrap_err();
            assert!(err.position().is_some(), "no position for {bad:?}");
        }
    }

    #[test]
    fn unparse_round_trip() {
        let cases = [
            "1 + 2 * 3",
            "a || b && c",
            "a ? b : c ? d : e",
            "!!x",
            "-x",
            "[1, 2, 3]",
            "{'a': 1, 'b': 2}",
            "Foo{bar: 1}",
            "a.b.c",
            "a[0]",
            "f(1, 2)",
            "a.f(1)",
            "has(a.b)",
            "e.all(x, x > 0)",
            "e.exists(x, x > 0)",
            "e.exists_one(x, x > 0)",
            "e.map(x, x + 1)",
            "e.filter(x, x > 0)",
            // Two-variable macros (macros2).
            "e.all(i, v, v > i)",
            "e.exists(i, v, v > i)",
            "e.existsOne(i, v, v > i)",
            "e.transformList(i, v, v + i)",
            "e.transformList(i, v, i > 0, v + i)",
            "e.transformMap(k, v, k + v)",
            "e.transformMap(k, v, k != 'x', k + v)",
            "1.5e-3",
            "7u",
            "'hi'",
            "b'\\x00'",
        ];
        for src in cases {
            let first = parse(src).unwrap();
            let round = parse(&unparse(&first)).unwrap_or_else(|e| {
                panic!("re-parse of {src:?} -> {:?} failed: {e}", unparse(&first))
            });
            assert_eq!(first, round, "round-trip mismatch for {src:?}");
        }
    }
}

//! Formula columns: a small expression language of our own (a Pratt parser and a tree-walking
//! evaluator; no third-party evaluator is used).
//!
//! # The language
//!
//! ```text
//! round([Area] * [Unit Cost], 2)
//! if([Material] = "Steel" and [Length] > 10, roundup([Length] / 6, 0), 0)
//! ceil([Wall Area] / 32) * 1.1
//! ```
//!
//! * **Numbers:** `12`, `0.5`, `.5`, `1e3`, `2.5E-2`. **Text:** `"Steel"` (a quote inside is
//!   written twice). **Truth values:** `true`, `false`.
//! * **Columns** are named by their heading in square brackets, `[Wall Area]`, without regard
//!   to case or surrounding spaces. A user column is found before a built-in one of the same
//!   heading; a user column can also be named by its id.
//! * **Operators**, loosest first: `or` / `||`; `and` / `&&`; `not` / `!` (prefix);
//!   comparisons `=` `==` `<>` `!=` `<` `<=` `>` `>=`; `+` `-`; `*` `/`; unary `-` and `+`;
//!   `^` (power, right-associative). Unary minus binds looser than `^`, as in mathematics:
//!   `-2^2` is `-4`, `2^-1` is `0.5` and `2^3^2` is `2^9`. Everything else is left-associative.
//! * **Functions** (names without regard to case): `round(x[, places])` (half away from
//!   zero), `roundup(x[, places])` (away from zero), `rounddown(x[, places])` (toward zero),
//!   `ceil(x[, step])`, `floor(x[, step])` (to a multiple of `step`, default 1), `min(a, ...)`,
//!   `max(a, ...)`, `abs(x)`, `sqrt(x)` and `if(condition, then, else)`, which evaluates only
//!   the branch it takes.
//!
//! # Values and types
//!
//! Arithmetic is unit-less: a quantity column gives its number in the unit the list shows it
//! in. An empty cell is 0 in arithmetic and false as a condition. Text can be compared (without
//! regard to case, digit runs as numbers) but not added; a truth value can be compared and used
//! as a condition but not added. Anything else is a [`FormulaError::TypeMismatch`].
//!
//! A choice column whose chosen option stands for a number (a material and its unit cost) is
//! that number in arithmetic and beside a number, and the option's text beside text, so both
//! `[Length] * [Material]` and `[Material] = "Steel"` work ([`Scalar::Choice`]). An option
//! without a number is plain text. A formula column's own value is a number (a choice's
//! number), text, a truth value or empty.
//!
//! # Errors
//!
//! Nothing here panics on any input. A problem is a typed [`FormulaError`]; in the list it is
//! the value of the cell (shown by its [`FormulaError::code`]) and it passes on to every
//! formula that uses the cell. Errors are found left to right, so the first one met is the one
//! reported.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::value::natural_cmp;

/// Why a formula has no value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
pub enum FormulaError {
    /// The formula cannot be read. `at` is the character position (from 0) of the problem.
    #[error("formula error at character {at}: {message}")]
    Syntax { at: usize, message: String },
    #[error("there is no function \"{0}\"")]
    UnknownFunction(String),
    #[error("{function} takes {expected} arguments, not {found}")]
    Arity {
        function: String,
        expected: String,
        found: usize,
    },
    /// A bracketed name that is not a column of the list.
    #[error("there is no column \"{0}\"")]
    UnknownColumn(String),
    #[error("division by zero")]
    DivisionByZero,
    /// The formula columns named (in schema order) refer to each other in a loop.
    #[error("these formula columns refer to each other in a loop: {}", .0.join(", "))]
    Cycle(Vec<String>),
    /// An operation was given a value of the wrong type, e.g. text to `+`.
    #[error("{operation} cannot use {found}")]
    TypeMismatch { operation: String, found: String },
    /// The result is not a finite number (an overflow, `sqrt(-1)`, `0^-1`).
    #[error("the result is not a finite number")]
    NotFinite,
}

impl FormulaError {
    /// The short form a cell shows.
    pub fn code(&self) -> &'static str {
        match self {
            FormulaError::Syntax { .. } => "#SYNTAX!",
            FormulaError::UnknownFunction(_) | FormulaError::Arity { .. } => "#NAME?",
            FormulaError::UnknownColumn(_) => "#REF!",
            FormulaError::DivisionByZero => "#DIV/0!",
            FormulaError::Cycle(_) => "#CYCLE!",
            FormulaError::TypeMismatch { .. } => "#VALUE!",
            FormulaError::NotFinite => "#NUM!",
        }
    }

    fn syntax(at: usize, message: impl Into<String>) -> FormulaError {
        FormulaError::Syntax {
            at,
            message: message.into(),
        }
    }
}

/// A value a formula works with.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Scalar {
    #[default]
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    /// An option of a choice column that stands for a number: the number in arithmetic and
    /// in comparisons with numbers, the option's text in comparisons with text.
    Choice {
        text: String,
        number: f64,
    },
}

impl Scalar {
    fn type_name(&self) -> &'static str {
        match self {
            Scalar::Empty => "an empty value",
            Scalar::Number(_) | Scalar::Choice { .. } => "a number",
            Scalar::Text(_) => "text",
            Scalar::Bool(_) => "a truth value",
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Scalar::Number(n) | Scalar::Choice { number: n, .. } => Some(*n),
            _ => None,
        }
    }
}

/// A function of the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Function {
    Round,
    RoundUp,
    RoundDown,
    Ceil,
    Floor,
    Min,
    Max,
    Abs,
    Sqrt,
    If,
}

impl Function {
    pub const ALL: [Function; 10] = [
        Function::Round,
        Function::RoundUp,
        Function::RoundDown,
        Function::Ceil,
        Function::Floor,
        Function::Min,
        Function::Max,
        Function::Abs,
        Function::Sqrt,
        Function::If,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Function::Round => "round",
            Function::RoundUp => "roundup",
            Function::RoundDown => "rounddown",
            Function::Ceil => "ceil",
            Function::Floor => "floor",
            Function::Min => "min",
            Function::Max => "max",
            Function::Abs => "abs",
            Function::Sqrt => "sqrt",
            Function::If => "if",
        }
    }

    pub fn from_name(name: &str) -> Option<Function> {
        Function::ALL
            .into_iter()
            .find(|f| f.name().eq_ignore_ascii_case(name))
    }

    /// The smallest and largest number of arguments (`None`: no limit).
    fn arity(self) -> (usize, Option<usize>) {
        match self {
            Function::Round
            | Function::RoundUp
            | Function::RoundDown
            | Function::Ceil
            | Function::Floor => (1, Some(2)),
            Function::Min | Function::Max => (1, None),
            Function::Abs | Function::Sqrt => (1, Some(1)),
            Function::If => (3, Some(3)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Pow => "^",
            BinaryOp::Eq => "=",
            BinaryOp::Ne => "<>",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "and",
            BinaryOp::Or => "or",
        }
    }

    /// Left and right binding power.
    fn binding(self) -> (u8, u8) {
        match self {
            BinaryOp::Or => (1, 2),
            BinaryOp::And => (3, 4),
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => (7, 8),
            BinaryOp::Add | BinaryOp::Sub => (9, 10),
            BinaryOp::Mul | BinaryOp::Div => (11, 12),
            // Right-associative, and tighter than a unary minus.
            BinaryOp::Pow => (16, 15),
        }
    }
}

/// The binding power of the operand of a prefix `not`.
const NOT_BP: u8 = 5;
/// The binding power of the operand of a prefix `-` or `+`.
const UNARY_BP: u8 = 13;

/// A parsed formula. A column reference is an index into [`Formula::references`].
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Text(String),
    Bool(bool),
    Column(usize),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Call(Function, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Text(String),
    Ident(String),
    Column(String),
    Op(BinaryOp),
    Not,
    Minus,
    Plus,
    LParen,
    RParen,
    Comma,
    End,
}

fn describe(t: &Token) -> String {
    match t {
        Token::Number(n) => format!("the number {n}"),
        Token::Text(_) => "text".to_string(),
        Token::Ident(s) => format!("\"{s}\""),
        Token::Column(s) => format!("[{s}]"),
        Token::Op(op) => format!("\"{}\"", op.symbol()),
        Token::Not => "\"not\"".to_string(),
        Token::Minus => "\"-\"".to_string(),
        Token::Plus => "\"+\"".to_string(),
        Token::LParen => "\"(\"".to_string(),
        Token::RParen => "\")\"".to_string(),
        Token::Comma => "\",\"".to_string(),
        Token::End => "the end of the formula".to_string(),
    }
}

/// Splits a formula into tokens, each with its character position.
fn lex(source: &str) -> Result<Vec<(Token, usize)>, FormulaError> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let at = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let two = |next: char| chars.get(i + 1) == Some(&next);
        let (token, len) = match c {
            '0'..='9' | '.' => {
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == '.' {
                    j += 1;
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                if j < chars.len() && (chars[j] == 'e' || chars[j] == 'E') {
                    let mut k = j + 1;
                    if k < chars.len() && (chars[k] == '+' || chars[k] == '-') {
                        k += 1;
                    }
                    if k < chars.len() && chars[k].is_ascii_digit() {
                        while k < chars.len() && chars[k].is_ascii_digit() {
                            k += 1;
                        }
                        j = k;
                    }
                }
                let text: String = chars[i..j].iter().collect();
                let n: f64 = text
                    .parse()
                    .map_err(|_| FormulaError::syntax(at, format!("\"{text}\" is not a number")))?;
                if !n.is_finite() {
                    return Err(FormulaError::syntax(at, format!("{text} is too large")));
                }
                (Token::Number(n), j - i)
            }
            '"' => {
                let mut s = String::new();
                let mut j = i + 1;
                loop {
                    match chars.get(j) {
                        None => {
                            return Err(FormulaError::syntax(at, "the text has no closing quote"))
                        }
                        Some('"') if chars.get(j + 1) == Some(&'"') => {
                            s.push('"');
                            j += 2;
                        }
                        Some('"') => break,
                        Some(c) => {
                            s.push(*c);
                            j += 1;
                        }
                    }
                }
                (Token::Text(s), j + 1 - i)
            }
            '[' => {
                let Some(end) = chars[i + 1..].iter().position(|c| *c == ']') else {
                    return Err(FormulaError::syntax(
                        at,
                        "the column name has no closing \"]\"",
                    ));
                };
                let name: String = chars[i + 1..i + 1 + end].iter().collect();
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err(FormulaError::syntax(
                        at,
                        "a column name is missing in \"[]\"",
                    ));
                }
                (Token::Column(name), end + 2)
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut j = i;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let word: String = chars[i..j].iter().collect();
                let token = match word.to_ascii_lowercase().as_str() {
                    "and" => Token::Op(BinaryOp::And),
                    "or" => Token::Op(BinaryOp::Or),
                    "not" => Token::Not,
                    _ => Token::Ident(word),
                };
                (token, j - i)
            }
            '+' => (Token::Plus, 1),
            '-' => (Token::Minus, 1),
            '*' => (Token::Op(BinaryOp::Mul), 1),
            '/' => (Token::Op(BinaryOp::Div), 1),
            '^' => (Token::Op(BinaryOp::Pow), 1),
            '(' => (Token::LParen, 1),
            ')' => (Token::RParen, 1),
            ',' => (Token::Comma, 1),
            '=' if two('=') => (Token::Op(BinaryOp::Eq), 2),
            '=' => (Token::Op(BinaryOp::Eq), 1),
            '!' if two('=') => (Token::Op(BinaryOp::Ne), 2),
            '!' => (Token::Not, 1),
            '<' if two('>') => (Token::Op(BinaryOp::Ne), 2),
            '<' if two('=') => (Token::Op(BinaryOp::Le), 2),
            '<' => (Token::Op(BinaryOp::Lt), 1),
            '>' if two('=') => (Token::Op(BinaryOp::Ge), 2),
            '>' => (Token::Op(BinaryOp::Gt), 1),
            '&' if two('&') => (Token::Op(BinaryOp::And), 2),
            '|' if two('|') => (Token::Op(BinaryOp::Or), 2),
            other => {
                return Err(FormulaError::syntax(
                    at,
                    format!("\"{other}\" is not part of the formula language"),
                ))
            }
        };
        out.push((token, at));
        i += len;
    }
    out.push((Token::End, chars.len()));
    Ok(out)
}

/// How deeply expressions may nest; deeper input is refused rather than overflowing the stack.
const MAX_DEPTH: usize = 200;

struct Parser {
    tokens: Vec<(Token, usize)>,
    at: usize,
    references: Vec<String>,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.at].0
    }

    fn pos(&self) -> usize {
        self.tokens[self.at].1
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.at].0.clone();
        if self.at + 1 < self.tokens.len() {
            self.at += 1;
        }
        t
    }

    fn expect(&mut self, want: &Token, what: &str) -> Result<(), FormulaError> {
        if self.peek() == want {
            self.bump();
            Ok(())
        } else {
            Err(FormulaError::syntax(
                self.pos(),
                format!("expected {what} but found {}", describe(self.peek())),
            ))
        }
    }

    fn reference(&mut self, name: String) -> usize {
        let key = name.to_lowercase();
        match self.references.iter().position(|r| r.to_lowercase() == key) {
            Some(i) => i,
            None => {
                self.references.push(name);
                self.references.len() - 1
            }
        }
    }

    fn expr(&mut self, min_bp: u8) -> Result<Expr, FormulaError> {
        self.expr_h(min_bp).map(|(e, _)| e)
    }

    /// An expression and the height of its tree. Nesting is bounded by the depth of this
    /// recursion; a long chain like `1+1+1+...` is parsed in a loop but makes a tree as tall as
    /// it is long, which evaluating (and dropping) walks recursively: its height is bounded too.
    fn expr_h(&mut self, min_bp: u8) -> Result<(Expr, usize), FormulaError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(FormulaError::syntax(
                self.pos(),
                "the formula nests too deeply",
            ));
        }
        let at = self.pos();
        let mut height = 1;
        let mut unary = |p: &mut Parser, op: UnaryOp, bp: u8| -> Result<Expr, FormulaError> {
            let (x, h) = p.expr_h(bp)?;
            height = h + 1;
            Ok(Expr::Unary(op, Box::new(x)))
        };
        let mut lhs = match self.bump() {
            Token::Number(n) => Expr::Number(n),
            Token::Text(s) => Expr::Text(s),
            Token::Column(name) => Expr::Column(self.reference(name)),
            Token::Minus => unary(self, UnaryOp::Neg, UNARY_BP)?,
            Token::Plus => unary(self, UnaryOp::Plus, UNARY_BP)?,
            Token::Not => unary(self, UnaryOp::Not, NOT_BP)?,
            Token::LParen => {
                let (inner, h) = self.expr_h(0)?;
                height = h;
                self.expect(&Token::RParen, "\")\"")?;
                inner
            }
            Token::Ident(word) => {
                let lower = word.to_ascii_lowercase();
                if self.peek() == &Token::LParen {
                    self.bump();
                    let mut args = Vec::new();
                    if self.peek() != &Token::RParen {
                        loop {
                            let (arg, h) = self.expr_h(0)?;
                            height = height.max(h + 1);
                            args.push(arg);
                            if self.peek() == &Token::Comma {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(&Token::RParen, "\",\" or \")\"")?;
                    let f = Function::from_name(&word)
                        .ok_or_else(|| FormulaError::UnknownFunction(word.clone()))?;
                    let (min, max) = f.arity();
                    if args.len() < min || max.is_some_and(|m| args.len() > m) {
                        return Err(FormulaError::Arity {
                            function: f.name().to_string(),
                            expected: match max {
                                Some(m) if m == min => min.to_string(),
                                Some(m) => format!("{min} to {m}"),
                                None => format!("at least {min}"),
                            },
                            found: args.len(),
                        });
                    }
                    Expr::Call(f, args)
                } else if lower == "true" {
                    Expr::Bool(true)
                } else if lower == "false" {
                    Expr::Bool(false)
                } else {
                    return Err(FormulaError::syntax(
                        at,
                        format!("\"{word}\" is not a function; a column name goes in brackets: [{word}]"),
                    ));
                }
            }
            other => {
                return Err(FormulaError::syntax(
                    at,
                    format!("expected a value but found {}", describe(&other)),
                ))
            }
        };
        loop {
            let op = match self.peek() {
                Token::Op(op) => *op,
                Token::Minus => BinaryOp::Sub,
                Token::Plus => BinaryOp::Add,
                _ => break,
            };
            let (lbp, rbp) = op.binding();
            if lbp < min_bp {
                break;
            }
            let at = self.pos();
            self.bump();
            let (rhs, h) = self.expr_h(rbp)?;
            height = height.max(h) + 1;
            if height > MAX_DEPTH {
                return Err(FormulaError::syntax(at, "the formula is too long"));
            }
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        self.depth -= 1;
        Ok((lhs, height))
    }
}

/// A formula, parsed once and evaluated per row.
#[derive(Debug, Clone, PartialEq)]
pub struct Formula {
    pub source: String,
    pub expr: Expr,
    /// The column names it refers to, as first written, without duplicates (case ignored).
    pub references: Vec<String>,
}

impl Formula {
    /// Reads a formula. A leading `=` is allowed, as in a spreadsheet.
    pub fn parse(source: &str) -> Result<Formula, FormulaError> {
        let body = source.trim_start();
        let offset = source.chars().count() - body.chars().count();
        let (body, offset) = match body.strip_prefix('=') {
            Some(rest) => (rest, offset + 1),
            None => (body, offset),
        };
        let mut tokens = lex(body)?;
        for t in &mut tokens {
            t.1 += offset;
        }
        if tokens.len() == 1 {
            return Err(FormulaError::syntax(offset, "the formula is empty"));
        }
        let mut p = Parser {
            tokens,
            at: 0,
            references: Vec::new(),
            depth: 0,
        };
        let expr = p.expr(0)?;
        if p.peek() != &Token::End {
            return Err(FormulaError::syntax(
                p.pos(),
                format!("unexpected {}", describe(p.peek())),
            ));
        }
        Ok(Formula {
            source: source.to_string(),
            expr,
            references: p.references,
        })
    }

    /// Evaluates the formula; `column(i)` gives the value of [`Formula::references`]`[i]`.
    pub fn eval<F>(&self, column: &mut F) -> Result<Scalar, FormulaError>
    where
        F: FnMut(usize) -> Result<Scalar, FormulaError>,
    {
        eval(&self.expr, column)
    }

    /// Evaluates the formula with columns looked up by name (the name as written).
    pub fn eval_named<F>(&self, mut column: F) -> Result<Scalar, FormulaError>
    where
        F: FnMut(&str) -> Result<Scalar, FormulaError>,
    {
        let refs = &self.references;
        eval(&self.expr, &mut |i| column(&refs[i]))
    }
}

impl fmt::Display for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

fn mismatch(operation: &str, found: &Scalar) -> FormulaError {
    FormulaError::TypeMismatch {
        operation: operation.to_string(),
        found: found.type_name().to_string(),
    }
}

fn number(op: &str, v: &Scalar) -> Result<f64, FormulaError> {
    match v {
        Scalar::Number(n) | Scalar::Choice { number: n, .. } => Ok(*n),
        Scalar::Empty => Ok(0.0),
        other => Err(mismatch(op, other)),
    }
}

fn truth(op: &str, v: &Scalar) -> Result<bool, FormulaError> {
    match v {
        Scalar::Bool(b) => Ok(*b),
        Scalar::Number(n) | Scalar::Choice { number: n, .. } => Ok(*n != 0.0),
        Scalar::Empty => Ok(false),
        other => Err(mismatch(op, other)),
    }
}

fn finite(n: f64) -> Result<Scalar, FormulaError> {
    if n.is_finite() {
        // Negative zero is zero.
        Ok(Scalar::Number(n + 0.0))
    } else {
        Err(FormulaError::NotFinite)
    }
}

fn compare(op: BinaryOp, a: &Scalar, b: &Scalar) -> Result<Ordering, FormulaError> {
    let err = |found: &Scalar| mismatch(op.symbol(), found);
    // A choice is its text beside text, its number beside anything else.
    let as_text = |v: &Scalar, other: &Scalar| match (v, other) {
        (Scalar::Choice { text, .. }, Scalar::Text(_) | Scalar::Choice { .. }) => {
            Scalar::Text(text.clone())
        }
        (Scalar::Choice { number, .. }, _) => Scalar::Number(*number),
        _ => v.clone(),
    };
    let (a, b) = (&as_text(a, b), &as_text(b, a));
    match (a, b) {
        (Scalar::Empty, Scalar::Empty) => Ok(Ordering::Equal),
        (Scalar::Number(_) | Scalar::Empty, Scalar::Number(_) | Scalar::Empty) => {
            // Adding zero makes a negative zero equal to zero.
            Ok((number("", a)? + 0.0).total_cmp(&(number("", b)? + 0.0)))
        }
        (Scalar::Text(x), Scalar::Text(y)) => Ok(natural_cmp(x, y)),
        (Scalar::Text(x), Scalar::Empty) => Ok(natural_cmp(x, "")),
        (Scalar::Empty, Scalar::Text(y)) => Ok(natural_cmp("", y)),
        (Scalar::Bool(x), Scalar::Bool(y)) => Ok(x.cmp(y)),
        (Scalar::Bool(x), Scalar::Empty) => Ok(x.cmp(&false)),
        (Scalar::Empty, Scalar::Bool(y)) => Ok(false.cmp(y)),
        (Scalar::Number(_), other) | (other, Scalar::Number(_)) => Err(err(other)),
        (Scalar::Text(_), other) | (other, Scalar::Text(_)) => Err(err(other)),
        (Scalar::Choice { .. }, _) | (_, Scalar::Choice { .. }) => {
            unreachable!("choices were turned into text or numbers above")
        }
    }
}

/// The scale factor for rounding to `places` decimal places (negative: to tens, hundreds...).
fn places_factor(places: Option<f64>) -> Result<f64, FormulaError> {
    let p = places.unwrap_or(0.0).trunc().clamp(-300.0, 300.0);
    Ok(10f64.powi(p as i32))
}

/// A hair added to a scaled value before rounding, so that a decimal like 2.675 (stored as
/// 2.67499999...) rounds the way it is written.
fn fuzz(s: f64) -> f64 {
    s.abs() * 8.0 * f64::EPSILON
}

#[derive(Clone, Copy)]
enum Rounding {
    Nearest,
    Up,
    Down,
}

fn round_to(x: f64, factor: f64, mode: Rounding) -> f64 {
    let s = (x * factor).abs();
    let r = match mode {
        Rounding::Nearest => (s + 0.5 + fuzz(s)).floor(),
        Rounding::Up => (s - fuzz(s)).ceil(),
        Rounding::Down => (s + fuzz(s)).floor(),
    };
    x.signum() * r / factor
}

/// The number of decimal places a step is written with (0.25 has two), when it has at most 15.
fn decimal_places(step: f64) -> Option<i32> {
    (0..=15).find(|k| {
        let s = step * 10f64.powi(*k);
        (s - s.round()).abs() <= 1e-9 * s.abs().max(1.0)
    })
}

/// `x` rounded up (or down) to a multiple of `step`. The sign of `step` is ignored, so the
/// result is never on the wrong side of `x`. A decimal step gives a result on its decimal
/// grid: `ceil(0.3, 0.1)` is 0.3, not 0.30000000000000004.
fn step_to(x: f64, step: f64, up: bool) -> f64 {
    let step = step.abs();
    if step == 0.0 {
        return 0.0;
    }
    let s = x / step;
    let r = if up {
        (s - fuzz(s)).ceil()
    } else {
        (s + fuzz(s)).floor()
    };
    match decimal_places(step) {
        // Whole multiples of the step's digits, scaled once: the nearest double to the
        // decimal result.
        Some(k) if k > 0 => {
            let scale = 10f64.powi(k);
            r * (step * scale).round() / scale
        }
        _ => r * step,
    }
}

fn eval<F>(e: &Expr, column: &mut F) -> Result<Scalar, FormulaError>
where
    F: FnMut(usize) -> Result<Scalar, FormulaError>,
{
    Ok(match e {
        Expr::Number(n) => Scalar::Number(*n),
        Expr::Text(s) => Scalar::Text(s.clone()),
        Expr::Bool(b) => Scalar::Bool(*b),
        Expr::Column(i) => column(*i)?,
        Expr::Unary(op, x) => {
            let v = eval(x, column)?;
            match op {
                UnaryOp::Neg => finite(-number("-", &v)?)?,
                UnaryOp::Plus => finite(number("+", &v)?)?,
                UnaryOp::Not => Scalar::Bool(!truth("not", &v)?),
            }
        }
        Expr::Binary(op, a, b) => {
            match op {
                BinaryOp::And => {
                    let x = truth("and", &eval(a, column)?)?;
                    return Ok(Scalar::Bool(x && truth("and", &eval(b, column)?)?));
                }
                BinaryOp::Or => {
                    let x = truth("or", &eval(a, column)?)?;
                    return Ok(Scalar::Bool(x || truth("or", &eval(b, column)?)?));
                }
                _ => {}
            }
            let x = eval(a, column)?;
            let y = eval(b, column)?;
            let s = op.symbol();
            match op {
                BinaryOp::Add => finite(number(s, &x)? + number(s, &y)?)?,
                BinaryOp::Sub => finite(number(s, &x)? - number(s, &y)?)?,
                BinaryOp::Mul => finite(number(s, &x)? * number(s, &y)?)?,
                BinaryOp::Div => {
                    let (p, q) = (number(s, &x)?, number(s, &y)?);
                    if q == 0.0 {
                        return Err(FormulaError::DivisionByZero);
                    }
                    finite(p / q)?
                }
                BinaryOp::Pow => {
                    let (p, q) = (number(s, &x)?, number(s, &y)?);
                    finite(p.powf(q))?
                }
                BinaryOp::Eq => Scalar::Bool(compare(*op, &x, &y)? == Ordering::Equal),
                BinaryOp::Ne => Scalar::Bool(compare(*op, &x, &y)? != Ordering::Equal),
                BinaryOp::Lt => Scalar::Bool(compare(*op, &x, &y)? == Ordering::Less),
                BinaryOp::Le => Scalar::Bool(compare(*op, &x, &y)? != Ordering::Greater),
                BinaryOp::Gt => Scalar::Bool(compare(*op, &x, &y)? == Ordering::Greater),
                BinaryOp::Ge => Scalar::Bool(compare(*op, &x, &y)? != Ordering::Less),
                BinaryOp::And | BinaryOp::Or => unreachable!("handled above"),
            }
        }
        Expr::Call(f, args) => {
            let name = f.name();
            if *f == Function::If {
                let c = truth(name, &eval(&args[0], column)?)?;
                return eval(&args[if c { 1 } else { 2 }], column);
            }
            let mut nums = Vec::with_capacity(args.len());
            for a in args {
                nums.push(number(name, &eval(a, column)?)?);
            }
            let x = nums[0];
            let second = nums.get(1).copied();
            finite(match f {
                Function::Round => round_to(x, places_factor(second)?, Rounding::Nearest),
                Function::RoundUp => round_to(x, places_factor(second)?, Rounding::Up),
                Function::RoundDown => round_to(x, places_factor(second)?, Rounding::Down),
                Function::Ceil => step_to(x, second.unwrap_or(1.0), true),
                Function::Floor => step_to(x, second.unwrap_or(1.0), false),
                Function::Min => nums.iter().copied().fold(f64::INFINITY, f64::min),
                Function::Max => nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                Function::Abs => x.abs(),
                Function::Sqrt => x.sqrt(),
                Function::If => unreachable!("handled above"),
            })?
        }
    })
}

/// Parses and evaluates a formula in one go, columns looked up by name. For one-off use and
/// tests; the list parses each formula column once ([`Formula::parse`]).
pub fn evaluate<F>(source: &str, column: F) -> Result<Scalar, FormulaError>
where
    F: FnMut(&str) -> Result<Scalar, FormulaError>,
{
    Formula::parse(source)?.eval_named(column)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(s: &str) -> f64 {
        match evaluate(s, |_| Ok(Scalar::Empty)) {
            Ok(Scalar::Number(n)) => n,
            other => panic!("{s}: {other:?}"),
        }
    }

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(num("1 + 2 * 3"), 7.0);
        assert_eq!(num("(1 + 2) * 3"), 9.0);
        assert_eq!(num("10 - 4 - 3"), 3.0);
        assert_eq!(num("100 / 10 / 5"), 2.0);
        assert_eq!(num("2 ^ 3 ^ 2"), 512.0);
        assert_eq!(num("-2 ^ 2"), -4.0);
        assert_eq!(num("(-2) ^ 2"), 4.0);
        assert_eq!(num("2 ^ -1"), 0.5);
        assert_eq!(num("2 * -3"), -6.0);
        assert_eq!(num("- -3"), 3.0);
        assert_eq!(num("+4"), 4.0);
        assert_eq!(num("= .5e1"), 5.0);
        assert_eq!(num("1e-3 * 1000"), 1.0);
    }

    #[test]
    fn lexing_errors_point_at_the_character() {
        let e = Formula::parse("1 + $").unwrap_err();
        assert_eq!(
            e,
            FormulaError::syntax(4, "\"$\" is not part of the formula language")
        );
        assert!(matches!(
            Formula::parse("[Area"),
            Err(FormulaError::Syntax { at: 0, .. })
        ));
        assert!(matches!(
            Formula::parse("(1 + 2"),
            Err(FormulaError::Syntax { at: 6, .. })
        ));
        assert!(matches!(
            Formula::parse("1 2"),
            Err(FormulaError::Syntax { at: 2, .. })
        ));
        assert!(matches!(
            Formula::parse("  "),
            Err(FormulaError::Syntax { .. })
        ));
        assert!(matches!(
            Formula::parse("area * 2"),
            Err(FormulaError::Syntax { at: 0, .. })
        ));
        let deep = "(".repeat(5000) + "1" + &")".repeat(5000);
        assert!(matches!(
            Formula::parse(&deep),
            Err(FormulaError::Syntax { .. })
        ));
    }

    /// A long flat chain is parsed in a loop but makes a tall tree; evaluating one of 100,000
    /// terms would overflow the stack, so it is refused like deep nesting. Run on a thread
    /// with the 1 MB stack of a Windows main thread.
    #[test]
    fn long_chains_are_refused_not_overflowed() {
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(|| {
                // The longest chain allowed evaluates within that stack.
                assert_eq!(num(&vec!["1"; 200].join("+")), 200.0);
                assert_eq!(num(&vec!["2"; 200].join("*")), 2f64.powi(200));
                assert!(Formula::parse(&vec!["1"; 202].join("+")).is_err());
                for op in ["+", "*", "-", " and ", "^"] {
                    let long = vec!["1"; 100_000].join(op);
                    assert!(
                        matches!(Formula::parse(&long), Err(FormulaError::Syntax { .. })),
                        "{op}"
                    );
                }
                // Long but shallow: a sum of many short groups is still fine.
                let groups = vec!["(1+1)"; 150].join("+");
                assert_eq!(num(&groups), 300.0);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn ceil_and_floor_land_on_the_steps_decimal_grid() {
        assert_eq!(num("ceil(0.3, 0.1)"), 0.3);
        assert_eq!(num("ceil(0.7, 0.1)"), 0.7);
        assert_eq!(num("floor(2.3, 0.1)"), 2.3);
        assert_eq!(num("ceil(1.2, 0.2)"), 1.2);
        assert_eq!(num("ceil(1.21, 0.25)"), 1.25);
        assert_eq!(num("floor(-1.21, 0.25)"), -1.25);
        assert_eq!(num("if(ceil(0.25, 0.1) = 0.3, 1, 0)"), 1.0);
        for i in 1..=1000 {
            let x = f64::from(i) / 10.0;
            assert_eq!(num(&format!("ceil({x}, 0.1)")), x, "ceil({x}, 0.1)");
            assert_eq!(num(&format!("floor({x}, 0.1)")), x, "floor({x}, 0.1)");
        }
        // Whole and odd steps as before.
        assert_eq!(num("ceil(7, 5)"), 10.0);
        assert_eq!(num("floor(7, 5)"), 5.0);
        assert_eq!(num("ceil(2.1)"), 3.0);
    }

    #[test]
    fn a_negative_step_rounds_by_its_size() {
        // Never to the wrong side of the value.
        assert_eq!(num("ceil(5, -2)"), 6.0);
        assert_eq!(num("floor(5, -2)"), 4.0);
        assert_eq!(num("ceil(-5, -2)"), -4.0);
        assert_eq!(num("floor(-5, -2)"), -6.0);
        assert_eq!(num("ceil(5, 0)"), 0.0);
    }
}

//! The formula language: a table of expressions with their exact results and typed errors, and
//! property tests that parse randomly generated expressions and compare the result with a
//! separate reference evaluator over the generated tree.

use pl_takeoff::{evaluate, Formula, FormulaError, Scalar};

#[derive(Debug)]
enum Want {
    N(f64),
    B(bool),
    T(&'static str),
    E,
    Fails(&'static str),
}

fn columns(name: &str) -> Result<Scalar, FormulaError> {
    Ok(match name.to_lowercase().as_str() {
        "a" => Scalar::Number(10.0),
        "b" => Scalar::Number(4.0),
        "zero" => Scalar::Number(0.0),
        "wall area" => Scalar::Number(2.5),
        "name" => Scalar::Text("Steel".into()),
        "door" => Scalar::Text("Door 10".into()),
        "empty" => Scalar::Empty,
        "flag" => Scalar::Bool(true),
        "broken" => return Err(FormulaError::DivisionByZero),
        _ => return Err(FormulaError::UnknownColumn(name.to_string())),
    })
}

#[test]
fn the_formula_table() {
    use Want::*;
    let cases: &[(&str, Want)] = &[
        // Precedence and associativity.
        ("1 + 2 * 3", N(7.0)),
        ("(1 + 2) * 3", N(9.0)),
        ("2 * 3 + 4 * 5", N(26.0)),
        ("10 - 4 - 3", N(3.0)),
        ("2 ^ 3 ^ 2", N(512.0)),
        ("(2 ^ 3) ^ 2", N(64.0)),
        ("-2 ^ 2", N(-4.0)),
        ("2 ^ -2", N(0.25)),
        ("-[A] * 2", N(-20.0)),
        ("1 - -1", N(2.0)),
        ("12 / 4 / 3", N(1.0)),
        ("1 + 2 = 3", B(true)),
        ("1 < 2 and 2 < 1", B(false)),
        ("1 < 2 or 2 < 1 and false", B(true)),
        ("not 1 = 2", B(true)),
        ("!(1 = 1)", B(false)),
        ("1 != 2 && 3 <> 3", B(false)),
        ("1 == 1 || x()", Fails("#NAME?")),
        // Columns by name, without regard to case or spaces.
        ("[A] * [B]", N(40.0)),
        ("[ wall AREA ] * 4", N(10.0)),
        ("[a] + [Empty]", N(10.0)),
        ("[Empty] * 3", N(0.0)),
        ("[Name] = \"steel\"", B(true)),
        ("[Door] > \"Door 9\"", B(true)),
        ("if([Name] = \"Steel\", 1, 2)", N(1.0)),
        ("if([Flag], \"yes\", \"no\")", T("yes")),
        ("if([Empty], 1, 2)", N(2.0)),
        ("[Name]", T("Steel")),
        ("[Empty]", E),
        ("\"say \"\"hi\"\"\"", T("say \"hi\"")),
        // Functions.
        ("round(2.675, 2)", N(2.68)),
        ("round(-2.5)", N(-3.0)),
        ("round(1234.5, -2)", N(1200.0)),
        ("roundup(2.001, 2)", N(2.01)),
        ("roundup(-2.001, 2)", N(-2.01)),
        ("roundup(3, 0)", N(3.0)),
        ("rounddown(2.999, 2)", N(2.99)),
        ("rounddown(-2.999)", N(-2.0)),
        ("ceil(2.1)", N(3.0)),
        ("ceil(-2.1)", N(-2.0)),
        ("ceil(33, 32)", N(64.0)),
        ("ceil(0.3 / 0.1)", N(3.0)),
        ("floor(2.9)", N(2.0)),
        ("floor(70, 32)", N(64.0)),
        ("ceil(5, 0)", N(0.0)),
        ("min(3, 1, 2)", N(1.0)),
        ("max(3, [A], 2)", N(10.0)),
        ("MAX(-1)", N(-1.0)),
        ("abs(-[B])", N(4.0)),
        ("sqrt(16)", N(4.0)),
        ("if(1 > 2, 1 / 0, 5)", N(5.0)),
        ("if(1 < 2, 7, [Nope])", N(7.0)),
        // Errors, by code.
        ("[A] / [Zero]", Fails("#DIV/0!")),
        ("1 / (2 - 2)", Fails("#DIV/0!")),
        ("[Nope] + 1", Fails("#REF!")),
        ("[Broken] + 1", Fails("#DIV/0!")),
        ("[Name] + 1", Fails("#VALUE!")),
        ("-[Name]", Fails("#VALUE!")),
        ("[Flag] * 2", Fails("#VALUE!")),
        ("[Name] < 3", Fails("#VALUE!")),
        ("[Flag] = 1", Fails("#VALUE!")),
        ("if([Name], 1, 2)", Fails("#VALUE!")),
        ("not [Name]", Fails("#VALUE!")),
        ("10 ^ 400", Fails("#NUM!")),
        ("0 ^ -1", Fails("#NUM!")),
        ("sqrt(-1)", Fails("#NUM!")),
        ("(-8) ^ (1 / 3)", Fails("#NUM!")),
        ("1e308 * 10", Fails("#NUM!")),
        ("pow(2, 3)", Fails("#NAME?")),
        ("round()", Fails("#NAME?")),
        ("abs(1, 2)", Fails("#NAME?")),
        ("if(1, 2)", Fails("#NAME?")),
        ("1 +", Fails("#SYNTAX!")),
        ("(1", Fails("#SYNTAX!")),
        ("1)", Fails("#SYNTAX!")),
        ("[]", Fails("#SYNTAX!")),
        ("area * 2", Fails("#SYNTAX!")),
        ("\"open", Fails("#SYNTAX!")),
        ("1 # 2", Fails("#SYNTAX!")),
        ("", Fails("#SYNTAX!")),
        ("1e999", Fails("#SYNTAX!")),
        ("min(1,)", Fails("#SYNTAX!")),
    ];
    let mut failures = Vec::new();
    for (expr, want) in cases {
        let got = evaluate(expr, columns);
        let ok = match (want, &got) {
            (N(n), Ok(Scalar::Number(m))) => (n - m).abs() <= 1e-12 * n.abs().max(1.0),
            (B(b), Ok(Scalar::Bool(c))) => b == c,
            (T(t), Ok(Scalar::Text(s))) => t == s,
            (E, Ok(Scalar::Empty)) => true,
            (Fails(code), Err(e)) => e.code() == *code,
            _ => false,
        };
        if !ok {
            failures.push(format!("{expr:?}: wanted {want:?}, got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn errors_are_typed_and_describe_themselves() {
    assert_eq!(
        evaluate("[Nope] * 2", columns),
        Err(FormulaError::UnknownColumn("Nope".into()))
    );
    assert_eq!(
        evaluate("abs(1, 2)", columns),
        Err(FormulaError::Arity {
            function: "abs".into(),
            expected: "1".into(),
            found: 2
        })
    );
    assert_eq!(
        evaluate("frobnicate(1)", columns),
        Err(FormulaError::UnknownFunction("frobnicate".into()))
    );
    assert_eq!(
        evaluate("[Name] * 2", columns),
        Err(FormulaError::TypeMismatch {
            operation: "*".into(),
            found: "text".into()
        })
    );
    let e = Formula::parse("= 3 * (2 + ").unwrap_err();
    assert!(matches!(e, FormulaError::Syntax { at: 11, .. }), "{e:?}");
    assert_eq!(
        FormulaError::Cycle(vec!["A".into(), "B".into()]).to_string(),
        "these formula columns refer to each other in a loop: A, B"
    );
    // The first error met, left to right, is the one reported.
    assert_eq!(evaluate("[Nope] / 0", columns).unwrap_err().code(), "#REF!");
    assert_eq!(
        evaluate("1 / 0 + [Nope]", columns).unwrap_err().code(),
        "#DIV/0!"
    );
    // References are collected once each, case ignored, in order of first use.
    let f = Formula::parse("[Area] * [Rate] + [area] - [RATE] * [Depth]").unwrap();
    assert_eq!(f.references, ["Area", "Rate", "Depth"]);
}

#[test]
fn nothing_panics_on_junk() {
    let mut rng = Rng(99);
    let alphabet: Vec<char> = "0123456789.eE+-*/^()[],\"<>=!&|abcdefinortu _\t"
        .chars()
        .collect();
    for _ in 0..20_000 {
        let len = rng.below(24) as usize;
        let s: String = (0..len)
            .map(|_| alphabet[rng.below(alphabet.len() as u64) as usize])
            .collect();
        // Any outcome but a panic.
        let _ = evaluate(&s, columns);
    }
}

// ---------------------------------------------------------------------------------------------
// Property tests against a reference evaluator.

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Debug, Clone)]
enum E {
    N(f64),
    Col(usize),
    Neg(Box<E>),
    Bin(char, Box<E>, Box<E>),
    Min(Vec<E>),
    Max(Vec<E>),
    Abs(Box<E>),
    /// if(l cmp r, then, else)
    If(&'static str, Box<E>, Box<E>, Box<E>, Box<E>),
}

const COLS: [(&str, f64); 4] = [
    ("Length", 12.5),
    ("Area", 40.0),
    ("Rate", 0.75),
    ("Nil", 0.0),
];

fn gen(rng: &mut Rng, depth: u32) -> E {
    if depth == 0 || rng.below(10) < 3 {
        return match rng.below(4) {
            0 => E::Col(rng.below(COLS.len() as u64) as usize),
            1 => E::N(rng.below(10) as f64),
            2 => E::N(rng.below(1000) as f64 / 8.0),
            _ => E::N((rng.below(5) + 1) as f64 / 2.0),
        };
    }
    let d = depth - 1;
    match rng.below(14) {
        0 => E::Neg(Box::new(gen(rng, d))),
        1 => E::Abs(Box::new(gen(rng, d))),
        2 => E::Min((0..rng.below(3) + 1).map(|_| gen(rng, d)).collect()),
        3 => E::Max((0..rng.below(3) + 1).map(|_| gen(rng, d)).collect()),
        4 => {
            let cmp = ["=", "<>", "<", "<=", ">", ">="][rng.below(6) as usize];
            E::If(
                cmp,
                Box::new(gen(rng, d)),
                Box::new(gen(rng, d)),
                Box::new(gen(rng, d)),
                Box::new(gen(rng, d)),
            )
        }
        5 => E::Bin('^', Box::new(gen(rng, d)), Box::new(gen(rng, d.min(1)))),
        k => {
            let op = ['+', '-', '*', '/'][(k % 4) as usize];
            E::Bin(op, Box::new(gen(rng, d)), Box::new(gen(rng, d)))
        }
    }
}

/// How tightly an expression binds, as the grammar says.
fn prec(e: &E) -> u8 {
    match e {
        E::Bin('+' | '-', ..) => 9,
        E::Bin('*' | '/', ..) => 11,
        E::Neg(_) => 13,
        E::Bin('^', ..) => 15,
        _ => 100,
    }
}

/// Prints with as few parentheses as the grammar needs (plus some redundant ones when
/// `extra` says so).
fn print(e: &E, rng: &mut Rng, extra: bool) -> String {
    let wrap = |s: String, need: bool, rng: &mut Rng| {
        if need || (extra && rng.below(8) == 0) {
            format!("({s})")
        } else {
            s
        }
    };
    match e {
        E::N(n) => format!("{n}"),
        E::Col(i) => {
            // Any case, any padding.
            let name = COLS[*i].0;
            let name = if rng.below(2) == 0 {
                name.to_uppercase()
            } else {
                name.to_string()
            };
            format!("[ {name}]")
        }
        E::Neg(x) => {
            let inner = print(x, rng, extra);
            format!("-{}", wrap(inner, prec(x) < 13, rng))
        }
        E::Bin(op, l, r) => {
            let p = prec(e);
            let ls = print(l, rng, extra);
            let rs = print(r, rng, extra);
            let (lneed, rneed) = if *op == '^' {
                (prec(l) <= p, prec(r) < p)
            } else {
                (prec(l) < p, prec(r) <= p)
            };
            format!("{} {op} {}", wrap(ls, lneed, rng), wrap(rs, rneed, rng))
        }
        E::Min(args) | E::Max(args) => {
            let name = if matches!(e, E::Min(_)) { "min" } else { "MAX" };
            let args: Vec<String> = args.iter().map(|a| print(a, rng, extra)).collect();
            format!("{name}({})", args.join(", "))
        }
        E::Abs(x) => format!("abs({})", print(x, rng, extra)),
        E::If(cmp, l, r, a, b) => format!(
            "if({} {cmp} {}, {}, {})",
            print(l, rng, extra),
            print(r, rng, extra),
            print(a, rng, extra),
            print(b, rng, extra)
        ),
    }
}

#[derive(Debug, PartialEq)]
enum RefErr {
    Div,
    Num,
}

fn check(n: f64) -> Result<f64, RefErr> {
    if n.is_finite() {
        Ok(n)
    } else {
        Err(RefErr::Num)
    }
}

/// The reference: straightforward recursion over the generated tree.
fn reference(e: &E) -> Result<f64, RefErr> {
    match e {
        E::N(n) => Ok(*n),
        E::Col(i) => Ok(COLS[*i].1),
        E::Neg(x) => Ok(-reference(x)?),
        E::Abs(x) => Ok(reference(x)?.abs()),
        E::Bin(op, l, r) => {
            let a = reference(l)?;
            let b = reference(r)?;
            match op {
                '+' => check(a + b),
                '-' => check(a - b),
                '*' => check(a * b),
                '/' if b == 0.0 => Err(RefErr::Div),
                '/' => check(a / b),
                _ => check(a.powf(b)),
            }
        }
        E::Min(args) => {
            let mut m = f64::INFINITY;
            for a in args {
                m = m.min(reference(a)?);
            }
            Ok(m)
        }
        E::Max(args) => {
            let mut m = f64::NEG_INFINITY;
            for a in args {
                m = m.max(reference(a)?);
            }
            Ok(m)
        }
        E::If(cmp, l, r, a, b) => {
            let (x, y) = (reference(l)?, reference(r)?);
            let c = match *cmp {
                "=" => x == y,
                "<>" => x != y,
                "<" => x < y,
                "<=" => x <= y,
                ">" => x > y,
                _ => x >= y,
            };
            reference(if c { a } else { b })
        }
    }
}

fn agree(got: &Result<Scalar, FormulaError>, want: &Result<f64, RefErr>) -> bool {
    match (got, want) {
        (Ok(Scalar::Number(a)), Ok(b)) => a == b || (*a == 0.0 && *b == 0.0),
        (Err(FormulaError::DivisionByZero), Err(RefErr::Div)) => true,
        (Err(FormulaError::NotFinite), Err(RefErr::Num)) => true,
        _ => false,
    }
}

fn lookup(name: &str) -> Result<Scalar, FormulaError> {
    COLS.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name.trim()))
        .map(|(_, v)| Scalar::Number(*v))
        .ok_or_else(|| FormulaError::UnknownColumn(name.into()))
}

#[test]
fn random_expressions_match_the_reference_evaluator() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut errors = 0;
    for case in 0..6000 {
        let e = gen(&mut rng, 6);
        let want = reference(&e);
        if want.is_err() {
            errors += 1;
        }
        for extra in [false, true] {
            let text = print(&e, &mut rng, extra);
            let got = evaluate(&text, lookup);
            assert!(
                agree(&got, &want),
                "case {case}: {text}\n  tree {e:?}\n  got {got:?}\n  reference {want:?}"
            );
        }
    }
    // Both outcomes are exercised.
    assert!(errors > 100 && errors < 5000, "{errors} error cases");
}

#[test]
fn fully_parenthesised_and_minimal_forms_agree() {
    fn full(e: &E) -> String {
        match e {
            E::N(n) => format!("({n})"),
            E::Col(i) => format!("[{}]", COLS[*i].0),
            E::Neg(x) => format!("(-{})", full(x)),
            E::Bin(op, l, r) => format!("({}{op}{})", full(l), full(r)),
            E::Min(a) => format!("min({})", a.iter().map(full).collect::<Vec<_>>().join(",")),
            E::Max(a) => format!("max({})", a.iter().map(full).collect::<Vec<_>>().join(",")),
            E::Abs(x) => format!("abs({})", full(x)),
            E::If(c, l, r, a, b) => {
                format!("if(({}){c}({}),{},{})", full(l), full(r), full(a), full(b))
            }
        }
    }
    let mut rng = Rng(42);
    for _ in 0..3000 {
        let e = gen(&mut rng, 5);
        let a = evaluate(&full(&e), lookup);
        let b = evaluate(&print(&e, &mut rng, false), lookup);
        assert_eq!(a, b, "{}", full(&e));
    }
}

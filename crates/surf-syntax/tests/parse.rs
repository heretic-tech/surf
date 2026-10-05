//! Snapshot tests: the AST of every construct in `docs/language.md` and of
//! the example scripts, plus the rendered diagnostics for common mistakes.
//!
//! Review `tests/snapshots/*.snap` when they change; `cargo insta review`
//! (or `INSTA_UPDATE=always cargo test -p surf-syntax`) rewrites them.

use surf_syntax::ast::{ExprKind, Item, StmtKind};
use surf_syntax::{parse, parse_expr, Program};

fn ok(name: &str, src: &str) -> Program {
    match parse(name, src) {
        Ok(p) => p,
        Err(d) => panic!("{name}: unexpected diagnostics:\n{}", d.render(src, false)),
    }
}

fn errors(name: &str, src: &str) -> String {
    match parse(name, src) {
        Ok(_) => panic!("{name}: expected a parse error"),
        Err(d) => d.render(src, false),
    }
}

macro_rules! ast_snapshot {
    ($name:literal, $src:expr) => {{
        let src: &str = $src;
        let program = ok($name, src);
        insta::assert_yaml_snapshot!($name, program);
    }};
}

macro_rules! error_snapshot {
    ($name:literal, $src:expr) => {{
        let src: &str = $src;
        insta::assert_snapshot!($name, errors(concat!($name, ".surf"), src));
    }};
}

// ---- examples/ --------------------------------------------------------------

#[test]
fn example_hello() {
    ast_snapshot!(
        "example_hello",
        include_str!("../../../examples/hello.surf")
    );
}

#[test]
fn example_login() {
    ast_snapshot!(
        "example_login",
        include_str!("../../../examples/login.surf")
    );
}

#[test]
fn example_two_tabs() {
    ast_snapshot!(
        "example_two_tabs",
        include_str!("../../../examples/two-tabs.surf")
    );
}

#[test]
fn example_scrape_emit() {
    ast_snapshot!(
        "example_scrape_emit",
        include_str!("../../../examples/scrape-emit.surf")
    );
}

#[test]
fn example_parallel_pool() {
    ast_snapshot!(
        "example_parallel_pool",
        include_str!("../../../examples/parallel-pool.surf")
    );
}

#[test]
fn example_supervised() {
    ast_snapshot!(
        "example_supervised",
        include_str!("../../../examples/supervised.surf")
    );
}

// ---- docs/language.md § 1 lexical structure --------------------------------

#[test]
fn lexical_layout() {
    ast_snapshot!(
        "lexical_layout",
        "#!/usr/bin/env surf\r\n\
         # comment-only lines never affect indentation\r\n\
         if ready:\n\
         \x20   click(\"#go\")       # block level 4\n\
         \n\
         \x20   if fast:\n\
         \x20       print(\"fast\")  # level 8\n\
         \x20 # a comment at an odd column is fine\n\
         print(\"done\")          # back to 0\n\
         xs = [\n\
         \x20   1,\n\
         \x20       2,   # indentation ignored inside brackets\n\
         ]\n\
         m = {a: 1,\n\
         \x20    b: 2}\n\
         f(1,\n\
         \x20 2)"
    );
}

#[test]
fn lexical_literals() {
    ast_snapshot!(
        "lexical_literals",
        "ints = [0, 42, -7, 1_000]\n\
         floats = [1.5, 0.25, 2e3, -1.5e-3]\n\
         bools = [true, false, nil]\n\
         durs = [500ms, 2s, 3m, 1h, 1.5s]\n\
         s = \"hello\"\n\
         esc = \"a\\nb\\tc\\r\\\\\\\"\\{x\\}\\u{1F600}\"\n\
         empty = \"\"\n"
    );
}

#[test]
fn lexical_interpolation() {
    ast_snapshot!(
        "lexical_interpolation",
        "name = \"world\"\n\
         print(\"hello {name}, 1+1={1 + 1}\")\n\
         print(\"url: {page.url()}\")\n\
         print(\"nested {\"q{name}\"} and {m[\"k\"]} and {[1, 2][0]}\")\n\
         print(\"{x}\")\n"
    );
}

// ---- § 2 expressions ----------------------------------------------------------

#[test]
fn expr_precedence() {
    ast_snapshot!(
        "expr_precedence",
        "a = 1 + 2 * 3 - 4 / 5 % 6\n\
         b = -x * y\n\
         c = -x.y[0]\n\
         d = not a == b or c and d\n\
         e = a < b and b <= c or a >= b and a != b or a == b and a > b\n\
         f = 1..n + 1\n\
         g = 1..=3\n\
         h = (1 + 2) * 3\n\
         i = nil or 3\n\
         j = a - -b\n\
         k = not not a\n"
    );
}

#[test]
fn expr_collections() {
    ast_snapshot!(
        "expr_collections",
        "xs = [1, 2, 3]\n\
         xs[0]\n\
         xs[1] = 20\n\
         push(xs, 4)\n\
         xs.push(4)\n\
         xs[-1]\n\
         empty = []\n\
         trailing = [1, 2,]\n\
         m = {name: \"a\", \"with space\": 1, [key_expr]: 2}\n\
         m.name\n\
         m[\"name\"]\n\
         m.count = 3\n\
         m.inner.deep[0].x = 1\n\
         e = {}\n\
         t = {a: 1,}\n"
    );
}

#[test]
fn expr_calls() {
    ast_snapshot!(
        "expr_calls",
        "click(\"#a\")\n\
         click(\"#a\", timeout: 5s)\n\
         f(1, 2, mode: \"fast\", retries: 3)\n\
         g()\n\
         page(2).goto(\"https://example.org\")\n\
         page(\"login\").click(\"#x\")\n\
         work.page(2).goto(\"u\")\n\
         h(a)(b)\n\
         x.y.z(1).w\n\
         emit(x)\n"
    );
}

#[test]
fn expr_lambdas() {
    ast_snapshot!(
        "expr_lambdas",
        "double = fn(x): x * 2\n\
         apply = fn(f, v): f(v)\n\
         print(apply(double, 4))\n\
         handler = fn(e):\n\
         \x20   print(e)\n\
         \x20   return e * 2\n\
         zero = fn(): document.cookie\n\
         eval(fn(): document.title)\n\
         withdef = fn(a, b: 2): a + b\n"
    );
}

#[test]
fn expr_block_lambda_in_brackets() {
    // A `):` line inside brackets followed by a deeper line opens a layout
    // block; the bracket may close on the block's last line or on its own
    // line, and further arguments may follow.
    ast_snapshot!(
        "expr_block_lambda_in_brackets",
        "xs.each(fn(x):\n\
         \x20   print(x))\n\
         apply(fn(x):\n\
         \x20   if x > 1:\n\
         \x20       return x\n\
         \x20   return 0\n\
         , 2)\n\
         result = outer(\n\
         \x20   fn(a):\n\
         \x20       inner(fn(b):\n\
         \x20           a + b\n\
         \x20       )\n\
         )\n\
         m = {handler: fn(e):\n\
         \x20   print(e)}\n\
         print(\"done\")\n"
    );
}

#[test]
fn expr_raw_strings() {
    ast_snapshot!(
        "expr_raw_strings",
        "js = r\"document.querySelectorAll('a').map(a => a?.href)\"\n\
         path = r\"C:\\Users\\{name}\\n\"\n\
         empty = r\"\"\n\
         eval(r\"({a: 1}).a\")\n"
    );
}

// ---- § 3 statements ------------------------------------------------------------

#[test]
fn stmt_compound_assign() {
    ast_snapshot!(
        "stmt_compound_assign",
        "x += 1\n\
         x -= 2 * y\n\
         m.count *= 2\n\
         xs[i + 1] /= 4\n\
         n %= 3\n"
    );
}

#[test]
fn stmt_after_block_lambda() {
    // The lambda's block ends the line: `[`, `(` and `-` on the next line
    // start a new statement rather than indexing / calling / subtracting.
    ast_snapshot!(
        "stmt_after_block_lambda",
        "f = fn(x):\n\
         \x20   print(x)\n\
         [1, 2].each(f)\n\
         g = fn():\n\
         \x20   print(1)\n\
         (1 + 2).to_str()\n\
         h = fn():\n\
         \x20   print(2)\n\
         -x\n\
         {a: 1}.keys()\n"
    );
}

#[test]
fn stmt_assign_and_exprs() {
    ast_snapshot!(
        "stmt_assign_and_exprs",
        "x = 1\n\
         m.key = 2\n\
         xs[0] = 3\n\
         x = (y == 1)\n\
         print(a, b, 1)\n\
         sleep(2s)\n\
         text(\"h1\")\n"
    );
}

#[test]
fn stmt_if() {
    ast_snapshot!(
        "stmt_if",
        "if count(\"li\") > 10:\n\
         \x20   print(\"many\")\n\
         elif exists(\".empty\"):\n\
         \x20   print(\"none\")\n\
         elif other:\n\
         \x20   print(\"other\")\n\
         else:\n\
         \x20   print(\"some\")\n\
         if x:\n\
         \x20   y()\n"
    );
}

#[test]
fn stmt_loops() {
    ast_snapshot!(
        "stmt_loops",
        "for a in all(\"a\"):\n\
         \x20   emit {href: a.attr(\"href\")}\n\
         for i in 1..=3:\n\
         \x20   print(i)\n\
         for k in keys(m):\n\
         \x20   print(\"{k} = {m[k]}\")\n\
         while not exists(\".done\"):\n\
         \x20   sleep(200ms)\n\
         loop:\n\
         \x20   msg = receive()\n\
         \x20   if msg == nil:\n\
         \x20       break\n\
         \x20   if msg == 0:\n\
         \x20       continue\n\
         \x20   handle(msg)\n"
    );
}

#[test]
fn stmt_fn_return() {
    ast_snapshot!(
        "stmt_fn_return",
        "fn login(user, pass):\n\
         \x20   type(\"#username\", user)\n\
         \x20   type(\"#password\", pass)\n\
         \x20   click(\"#submit\")\n\
         \x20   return url()\n\
         fn f(a, b: 2):\n\
         \x20   return\n\
         fn outer():\n\
         \x20   fn inner(x):\n\
         \x20       return x\n\
         \x20   return inner\n\
         g = login\n\
         f(1, b: 5)\n"
    );
}

#[test]
fn stmt_try_catch() {
    ast_snapshot!(
        "stmt_try_catch",
        "try:\n\
         \x20   click(\"#maybe\", timeout: 2s)\n\
         catch e:\n\
         \x20   print(\"no button: {e.message}\")\n\
         try:\n\
         \x20   fail(\"x\")\n\
         catch:\n\
         \x20   print(\"ignored\")\n"
    );
}

#[test]
fn stmt_emit_exit() {
    ast_snapshot!(
        "stmt_emit_exit",
        "emit {text: a.text(), href: a.attr(\"href\")}\n\
         emit 42\n\
         emit(x)\n\
         if done:\n\
         \x20   exit\n\
         exit(1)\n"
    );
}

// ---- § 4 configuration blocks -------------------------------------------------

#[test]
fn config_browser() {
    ast_snapshot!(
        "config_browser",
        "browser:\n\
         \x20   path: \"/usr/bin/chromium\"      # optional; auto-discovery otherwise\n\
         \x20   cdp: pipe                      # default | 9222 | \"ws://…\"\n\
         \x20   pool: env(\"BROWSER_POOL_URL\")\n\
         \x20   proxy: \"http://user:pass@host:8080\"\n\
         \x20   proxies: [\"http://a:1\", \"http://b:1\"]\n\
         \x20   headless: false\n\
         \x20   virtual: true\n\
         \x20   size: \"1280x800\"\n\
         \x20   profile: \"./profiles/alice\"\n\
         \x20   flags: [\"--lang=en-US\"]\n\
         \x20   timeout: 30s\n\
         \x20   engine: chrome\n\
         \n\
         browser work:\n\
         \x20   cdp: 9222\n\
         \x20   proxy:\n\
         \x20       server: \"http://host:8080\"\n\
         \x20       user: env(\"PROXY_USER\")\n\
         \n\
         work.page(2).goto(\"https://example.com\")\n\
         browser.goto(\"https://example.com\")\n"
    );
}

// ---- § 6 reactive handlers -------------------------------------------------------

#[test]
fn handlers() {
    ast_snapshot!(
        "handlers",
        "on element_appears(\".checkout\"):\n\
         \x20   click(\".checkout .accept\")\n\
         on navigation(\"*/login*\"):\n\
         \x20   print(\"went to {event.url}\")\n\
         on dialog:\n\
         \x20   event.accept()\n\
         on dialog():\n\
         \x20   event.dismiss()\n\
         on request(\"*/api/*\"):\n\
         \x20   print(event.method, event.url)\n\
         on response(\"*.json\"):\n\
         \x20   print(event.status)\n"
    );
}

// ---- § 7 concurrency -----------------------------------------------------------------

#[test]
fn concurrency_spawn_parallel() {
    ast_snapshot!(
        "concurrency_spawn_parallel",
        "fn crawl(url):\n\
         \x20   goto(url)\n\
         \x20   return title()\n\
         h = spawn crawl(\"https://example.com\")\n\
         print(h.join())\n\
         spawn crawl(\"https://example.org\")\n\
         handles = [spawn crawl(\"a\"), spawn crawl(\"b\")]\n\
         spawn work.fetch(1)\n\
         parallel for url in urls:\n\
         \x20   limit: 4\n\
         \x20   fail_fast: false\n\
         \x20   goto(url)\n\
         \x20   emit {url: url, title: title()}\n\
         parallel for u in [1, 2]:\n\
         \x20   goto(u)\n"
    );
}

#[test]
fn concurrency_task() {
    ast_snapshot!(
        "concurrency_task",
        "task fetch(url):\n\
         \x20   retry: 5\n\
         \x20   on_fail: shift_proxy()\n\
         \x20   timeout: 60s\n\
         \x20   fresh: true\n\
         \x20   goto(url)\n\
         \x20   return html(\"body\")\n\
         task plain(a, b: 1):\n\
         \x20   return a + b\n\
         fetch(u)\n\
         spawn fetch(u)\n"
    );
}

#[test]
fn concurrency_actors() {
    ast_snapshot!(
        "concurrency_actors",
        "actor Scout():\n\
         \x20   goto(\"https://example.com\")\n\
         \x20   broadcast({kind: \"token\", value: eval(\"document.title\")})\n\
         actor Worker(n):\n\
         \x20   timeout: 30s\n\
         \x20   msg = receive(timeout: 30s)       # nil on timeout\n\
         \x20   if msg == nil:\n\
         \x20       return\n\
         \x20   goto(\"https://example.com/?t={msg.value}\")\n\
         \x20   emit {worker: n, id: self.id}\n\
         actor Listener():\n\
         \x20   on message:\n\
         \x20       print(event)\n\
         \x20   wait_for_message()\n\
         send(ref, {a: 1})\n\
         broadcast(msg)\n"
    );
}

#[test]
fn concurrency_supervisor() {
    ast_snapshot!(
        "concurrency_supervisor",
        "supervisor Crew:\n\
         \x20   strategy: one_for_one      # or one_for_all\n\
         \x20   max_restarts: 3\n\
         \x20   within: 60s\n\
         \x20   spawn Scout()\n\
         \x20   parallel for n in 1..=3:\n\
         \x20       limit: 2\n\
         \x20       spawn Worker(n)\n\
         supervisor Bare:\n\
         \x20   spawn One()\n\
         Crew.stop()\n\
         Crew.join()\n"
    );
}

// ---- structural checks (not snapshots) ----------------------------------------------

#[test]
fn spans_cover_statements() {
    let src = "x = 1\nif x:\n    print(x)\n";
    let p = ok("spans", src);
    let Item::Stmt(s0) = &p.items[0] else {
        panic!()
    };
    assert_eq!(&src[s0.span.range()], "x = 1");
    let Item::Stmt(s1) = &p.items[1] else {
        panic!()
    };
    assert_eq!(&src[s1.span.range()], "if x:\n    print(x)");
    let StmtKind::If { branches, .. } = &s1.kind else {
        panic!()
    };
    assert_eq!(&src[branches[0].1.span.range()], "print(x)");
    assert_eq!(p.span.range(), 0..src.len());
}

#[test]
fn interpolation_spans_point_into_the_string() {
    let src = "print(\"a {x + 1} b\")\n";
    let p = ok("interp", src);
    let Item::Stmt(s) = &p.items[0] else { panic!() };
    let StmtKind::Expr(call) = &s.kind else {
        panic!()
    };
    let ExprKind::Call { args, .. } = &call.kind else {
        panic!()
    };
    let ExprKind::Str(segs) = &args[0].value.kind else {
        panic!()
    };
    let surf_syntax::ast::StrSegment::Expr(e) = &segs[1] else {
        panic!()
    };
    assert_eq!(&src[e.span.range()], "x + 1");
}

#[test]
fn lambda_body_span_is_verbatim_source() {
    let src = "eval(fn(): document.cookie)\n";
    let p = ok("lambda", src);
    let Item::Stmt(s) = &p.items[0] else { panic!() };
    let StmtKind::Expr(call) = &s.kind else {
        panic!()
    };
    let ExprKind::Call { args, .. } = &call.kind else {
        panic!()
    };
    let ExprKind::Lambda { body, .. } = &args[0].value.kind else {
        panic!()
    };
    let surf_syntax::ast::LambdaBody::Expr(e) = body else {
        panic!()
    };
    assert_eq!(&src[e.span.range()], "document.cookie");
}

/// Compact S-expression form of an expression, for precedence checks.
fn sexpr(e: &surf_syntax::ast::Expr) -> String {
    use surf_syntax::ast::{BinaryOp as B, UnaryOp as U};
    match &e.kind {
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Ident(id) => id.name.clone(),
        ExprKind::Unary { op, expr } => {
            let op = match op {
                U::Neg => "neg",
                U::Not => "not",
            };
            format!("({op} {})", sexpr(expr))
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let op = match op {
                B::Or => "or",
                B::And => "and",
                B::Eq => "==",
                B::NotEq => "!=",
                B::Lt => "<",
                B::LtEq => "<=",
                B::Gt => ">",
                B::GtEq => ">=",
                B::Add => "+",
                B::Sub => "-",
                B::Mul => "*",
                B::Div => "/",
                B::Rem => "%",
            };
            format!("({op} {} {})", sexpr(lhs), sexpr(rhs))
        }
        ExprKind::Range {
            start,
            end,
            inclusive,
        } => format!(
            "({} {} {})",
            if *inclusive { "..=" } else { ".." },
            sexpr(start),
            sexpr(end)
        ),
        ExprKind::Field { receiver, name } => format!("(. {} {})", sexpr(receiver), name.name),
        ExprKind::Index { receiver, index } => format!("([] {} {})", sexpr(receiver), sexpr(index)),
        ExprKind::Call { callee, args } => format!(
            "({} {})",
            sexpr(callee),
            args.iter()
                .map(|a| sexpr(&a.value))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        ExprKind::Spawn(inner) => format!("(spawn {})", sexpr(inner)),
        other => format!("{other:?}"),
    }
}

#[test]
fn precedence_table() {
    let cases = [
        ("1 + 2 * 3 - 4 / 5 % 6", "(- (+ 1 (* 2 3)) (% (/ 4 5) 6))"),
        ("-x * y", "(* (neg x) y)"),
        ("-x.y[0]", "(neg ([] (. x y) 0))"),
        ("-2 * 3", "(* -2 3)"),
        ("not a == b or c and d", "(or (not (== a b)) (and c d))"),
        ("a or b and c", "(or a (and b c))"),
        ("a and b or c", "(or (and a b) c)"),
        ("1..n + 1", "(.. 1 (+ n 1))"),
        ("a < b + 1", "(< a (+ b 1))"),
        ("a == 1..3", "(== a (.. 1 3))"),
        ("(1 + 2) * 3", "(* (+ 1 2) 3)"),
        ("a - -b", "(- a (neg b))"),
        ("not not a", "(not (not a))"),
        ("f(x)(y).z[0]", "([] (. ((f x) y) z) 0)"),
        ("spawn f(x)", "(spawn (f x))"),
        ("a - b - c", "(- (- a b) c)"),
        ("a / b * c", "(* (/ a b) c)"),
    ];
    for (src, want) in cases {
        let e =
            parse_expr("<prec>", src).unwrap_or_else(|d| panic!("{src}: {}", d.render(src, false)));
        assert_eq!(sexpr(&e), want, "source: {src}");
    }
}

#[test]
fn parse_expr_api() {
    let e = parse_expr("<repl>", "1 + 2 * 3").unwrap();
    assert!(matches!(e.kind, ExprKind::Binary { .. }));
    assert!(parse_expr("<repl>", "1 +").is_err());
    assert!(parse_expr("<repl>", "1 2").is_err());
}

#[test]
fn no_trailing_newline_and_crlf() {
    ok("eof", "x = 1");
    ok("eof-block", "if x:\n    y()");
    ok("crlf", "if x:\r\n    y()\r\nz()\r\n");
    ok("empty", "");
    ok("comments-only", "# just\n# comments\n");
}

#[test]
fn all_errors_are_reported_together() {
    let src = "x = @\ny = $\n";
    let d = parse("multi", src).unwrap_err();
    assert_eq!(d.len(), 2);
    // Parser errors: one per broken line, recovery continues after each.
    let src = "x = = 2\nif:\n    y()\nz = 3 +\nok()\n";
    let d = parse("multi2", src).unwrap_err();
    assert_eq!(d.len(), 3, "{}", d.render(src, false));
}

// ---- error rendering ---------------------------------------------------------------------

#[test]
fn err_compound_assign_target() {
    error_snapshot!("err_compound_assign_target", "f(x) += 1\n");
}

#[test]
fn err_raw_string_unclosed() {
    error_snapshot!("err_raw_string_unclosed", "x = r\"abc\ny = 1\n");
}

#[test]
fn err_block_lambda_in_brackets_dedent() {
    // The body dedents to a level that is deeper than the header but
    // matches no block level inside it.
    error_snapshot!(
        "err_block_lambda_in_brackets_dedent",
        "apply(fn(x):\n\
         \x20       print(x)\n\
         \x20     print(1)\n\
         )\n"
    );
}

#[test]
fn err_tab_indent() {
    error_snapshot!("err_tab_indent", "if x:\n\tclick(\"#a\")\n\tprint(1)\n");
}

#[test]
fn err_mixed_dedent() {
    error_snapshot!("err_mixed_dedent", "if x:\n    if y:\n        a()\n  b()\n");
}

#[test]
fn err_unexpected_indent() {
    error_snapshot!("err_unexpected_indent", "a()\n    b()\n");
}

#[test]
fn err_missing_block() {
    error_snapshot!("err_missing_block", "if x:\nprint(1)\n");
}

#[test]
fn err_missing_block_eof() {
    error_snapshot!("err_missing_block_eof", "fn f():");
}

#[test]
fn err_unclosed_string() {
    error_snapshot!("err_unclosed_string", "click(\"#submit)\nprint(1)\n");
}

#[test]
fn err_property_after_statement() {
    error_snapshot!(
        "err_property_after_statement",
        "task fetch(url):\n    retry: 3\n    goto(url)\n    timeout: 5s\n    return 1\n"
    );
}

#[test]
fn err_property_at_top_level() {
    error_snapshot!("err_property_at_top_level", "headless: true\nx: 1\n");
}

#[test]
fn err_unknown_token() {
    error_snapshot!("err_unknown_token", "x = 1 @ 2\ny = a && b\nz = 'c'\n");
}

#[test]
fn err_unknown_browser_option() {
    error_snapshot!(
        "err_unknown_browser_option",
        "browser:\n    headles: true\n    proxy: \"x\"\n    proxy: \"y\"\n"
    );
}

#[test]
fn err_misspelled_keyword() {
    error_snapshot!("err_misspelled_keyword", "whlie x:\n    y()\n");
}

#[test]
fn err_else_if() {
    error_snapshot!("err_else_if", "if a:\n    b()\nelse if c:\n    d()\n");
}

#[test]
fn err_did_you_mean_page_call() {
    error_snapshot!(
        "err_did_you_mean_page_call",
        "page 1\nclick \"#a\"\npage.1.goto(\"x\")\n"
    );
}

#[test]
fn err_chained_comparison() {
    error_snapshot!("err_chained_comparison", "ok = a < b < c\n");
}

#[test]
fn err_positional_after_keyword() {
    error_snapshot!(
        "err_positional_after_keyword",
        "click(\"#a\", timeout: 5s, 2)\n"
    );
}

#[test]
fn err_unclosed_bracket() {
    error_snapshot!("err_unclosed_bracket", "xs = [1, 2\nprint(xs)\n");
}

#[test]
fn err_mismatched_bracket() {
    error_snapshot!("err_mismatched_bracket", "f(1, [2, 3)\n");
}

#[test]
fn err_supervisor_body() {
    error_snapshot!(
        "err_supervisor_body",
        "supervisor Crew:\n    strategy: one_for_one\n    spawn A()\n    print(1)\n    parallel for n in 1..3:\n        goto(n)\n"
    );
}

#[test]
fn err_unknown_event() {
    error_snapshot!(
        "err_unknown_event",
        "on elementappears(\".x\"):\n    click(\".x\")\non message:\n    print(event)\n"
    );
}

#[test]
fn err_interpolation() {
    error_snapshot!(
        "err_interpolation",
        "print(\"a {x + } b\")\nprint(\"open {x\")\nprint(\"{}\")\n"
    );
}

#[test]
fn err_spawn_not_call() {
    error_snapshot!("err_spawn_not_call", "spawn worker\n");
}

#[test]
fn err_invalid_assignment() {
    error_snapshot!("err_invalid_assignment", "f(x) = 1\n1 = 2\n");
}

#[test]
fn err_duplicate_browser() {
    error_snapshot!(
        "err_duplicate_browser",
        "browser:\n    headless: true\nbrowser:\n    headless: false\n"
    );
}

#[test]
fn err_keyword_as_name() {
    error_snapshot!("err_keyword_as_name", "task = 1\nfn for():\n    return\n");
}

#[test]
fn err_bad_duration_and_escape() {
    error_snapshot!("err_bad_duration_and_escape", "sleep(10sec)\nx = \"\\q\"\n");
}

#[test]
fn err_missing_catch() {
    error_snapshot!("err_missing_catch", "try:\n    a()\nb()\n");
}

#[test]
fn err_inline_block() {
    error_snapshot!("err_inline_block", "if x: print(1)\n");
}

#[test]
fn err_expected_expression() {
    error_snapshot!("err_expected_expression", "x = \ny = 1 +\nz = * 2\n");
}

#[test]
fn err_tab_then_spaces_only_reports_once() {
    let src = "if x:\n\ta()\n\tb()\nc()\n";
    let d = parse("tabs", src).unwrap_err();
    assert_eq!(d.len(), 1, "{}", d.render(src, false));
}

#[test]
fn err_runtime_shape() {
    // The runtime builds its diagnostics through the same type.
    let src = "browser:\n    virtual: true\n\npage.goto(\"https://example.com/login\")\ntype(\"#username\", \"user\")\ntype(\"#password\", \"pass\")\nclick(\"#submit\")\n";
    let line = src.lines().take(6).map(|l| l.len() + 1).sum::<usize>();
    let span = surf_syntax::Span::new(line as u32, (line + "click(\"#submit\")".len()) as u32);
    let d = surf_syntax::Diagnostic::runtime(
        "element not visible after 30s",
        Some(span),
        Some("#submit"),
        Some("DOM.getContentQuads"),
    );
    insta::assert_snapshot!(
        "err_runtime_shape",
        d.render("examples/login.surf", src, false)
    );
}

/// `cargo test -p surf-syntax --release -- --ignored --nocapture perf` —
/// prints the parse throughput of the example scripts.
#[test]
#[ignore]
fn perf_parse_examples() {
    let sources = [
        include_str!("../../../examples/hello.surf"),
        include_str!("../../../examples/login.surf"),
        include_str!("../../../examples/two-tabs.surf"),
        include_str!("../../../examples/scrape-emit.surf"),
        include_str!("../../../examples/parallel-pool.surf"),
        include_str!("../../../examples/supervised.surf"),
    ];
    let bytes: usize = sources.iter().map(|s| s.len()).sum();
    let iters: usize = 2000;
    let start = std::time::Instant::now();
    for _ in 0..iters {
        for src in sources {
            std::hint::black_box(parse("perf", src).unwrap());
        }
    }
    let elapsed = start.elapsed();
    let per_file = elapsed / (iters * sources.len()) as u32;
    let mb_s = (bytes * iters) as f64 / elapsed.as_secs_f64() / 1e6;
    println!(
        "parsed {} example files {iters}× in {elapsed:?}: {per_file:?}/file, {mb_s:.1} MB/s",
        sources.len()
    );
}

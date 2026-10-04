# The Surf language

Surf is a small, indentation-based scripting language for driving one browser
(usually one tab) over raw CDP with zero boilerplate. This document is the
authoritative reference the lexer, parser (`surf-syntax`) and VM (`surf-vm`)
are built to. Where it is silent, the examples in `examples/` win.

Design goals, in order: the 90 % case (one browser, one tab) needs **no**
setup code; everything is a value; blocking is invisible (no `await`); errors
carry the selector and the CDP method that failed.

```
page.goto("https://example.com")
print(text("h1"))
```

---

## 1. Lexical structure

### 1.1 Source

UTF-8 text. Line endings `\n` (a `\r\n` pair is treated as `\n`). A leading
`#!` line (shebang) is a comment.

### 1.2 Comments

`#` to end of line. Comments never affect indentation (a comment-only line is
a blank line).

### 1.3 Indentation and blocks

- Indentation uses **spaces only**. A tab in leading whitespace is a syntax
  error (`tabs are not allowed for indentation`).
- A line that ends in `:` (ignoring trailing comments/whitespace) opens a
  block. The next non-blank line **must** be indented deeper than the
  header, and that depth becomes the block's level. Every line of the block
  is at exactly that level; a deeper line not preceded by a `:` header is an
  error; a shallower line closes the block (and any enclosing blocks down to
  that level — the level must match one on the stack).
- Blank lines and comment-only lines are ignored for indentation.
- The lexer emits `INDENT` / `DEDENT` tokens; the top level is depth 0.
- Inside `(…)`, `[…]`, `{…}` newlines and indentation are ignored (implicit
  line continuation), so long argument lists and literals may span lines.
- Each logical line is one statement. There are no semicolons.

```
if ready:
    click("#go")       # block level 4
    if fast:
        print("fast")  # level 8
print("done")          # back to 0
```

### 1.4 Identifiers and keywords

Identifiers: `[A-Za-z_][A-Za-z0-9_]*`. Case-sensitive.

Reserved keywords (cannot be used as names):

```
if elif else for in while loop break continue fn return try catch
and or not true false nil emit exit spawn parallel task actor supervisor on
```

`print`, `sleep`, `page`, `browser`, `env`, `self`, `send`, `broadcast`,
`receive` are **not** keywords; they are globals provided by the runtime.

### 1.5 Literals

| kind     | examples                                   | notes |
|----------|--------------------------------------------|-------|
| int      | `0`, `42`, `-7`, `1_000`                   | 64-bit signed; `_` separators allowed |
| float    | `1.5`, `0.25`, `2e3`                       | 64-bit; a `.` must be followed by a digit (so `1..5` is a range) |
| bool     | `true`, `false`                            | |
| nil      | `nil`                                      | |
| string   | `"hello"`, `"a {x} b"`                     | see § 1.6 |
| duration | `500ms`, `2s`, `3m`, `1h`, `1.5s`          | see § 1.7 |

### 1.6 Strings

Double quotes only. Escapes: `\n \t \r \\ \" \{ \}` and `\u{1F600}`.

`{expr}` inside a string **interpolates**: the expression is lexed and parsed
with the full grammar (nested strings and braces allowed) and its value is
converted with the same rules as `print`. `\{` yields a literal brace.

```
name = "world"
print("hello {name}, 1+1={1 + 1}")      # hello world, 1+1=2
print("url: {page.url()}")
```

Strings are immutable. Indexing `s[i]` yields a one-character string;
`len(s)` counts characters.

### 1.7 Durations

A number immediately followed by a unit: `ms`, `s`, `m`, `h` (no space).
Durations are a distinct type, usable wherever a timeout is expected, in
`sleep`, and in arithmetic with each other (`2s + 500ms`) or scaled by a
number (`2 * 1s`). Comparing durations works; adding a duration to an int is
an error.

```
sleep(500ms)
click("#a", timeout: 5s)
```

### 1.8 Operators and punctuation

```
+  -  *  /  %     ==  !=  <  <=  >  >=     and  or  not
=  :  ,  .  ..  ..=  (  )  [  ]  {  }
```

---

## 2. Expressions

Precedence, lowest to highest (all binary operators are left-associative):

1. `or`
2. `and`
3. `not` (prefix)
4. `==` `!=` `<` `<=` `>` `>=` (non-associative — `a < b < c` is an error)
5. `..` `..=` (range; non-associative)
6. `+` `-`
7. `*` `/` `%`
8. unary `-`
9. postfix: call `f(…)`, method `x.m(…)`, field `x.f`, index `x[i]`

`and` / `or` short-circuit and return the deciding operand (`nil or 3` → `3`).

### 2.1 Arithmetic and comparison

- `int op int` → int (`/` truncates toward zero; `/ 0` is a runtime error).
- Mixed int/float → float.
- `+` concatenates two strings or two lists. Mixing string and non-string
  with `+` is an error — use interpolation.
- `==` / `!=` compare by value for `nil`, bools, numbers, strings, durations,
  lists and maps (deep); by identity for functions and native objects.
  `1 == 1.0` is `true`.
- `<` etc. work on numbers, strings (lexicographic), durations.

### 2.2 Truthiness

`nil`, `false`, `0`, `0.0`, `""`, `[]`, `{}` are falsy; everything else is
truthy. `if x:` uses truthiness; `==` does not.

### 2.3 Lists

```
xs = [1, 2, 3]
xs[0]            # 1       (0-based; negative indexes count from the end)
xs[1] = 20
push(xs, 4)      # xs.push(4) also works
len(xs)          # 4
xs + [5]         # new list
```

Lists are mutable and shared by reference.

### 2.4 Maps

Insertion-ordered, string keys.

```
m = {name: "a", "with space": 1, [key_expr]: 2}
m.name           # "a"
m["name"]        # "a"
m.missing        # nil   (no error)
m.count = 3      # insert / overwrite
keys(m)          # ["name", "with space", …]
```

Bare identifier keys in a literal are strings (`{a: 1}` ≡ `{"a": 1}`).

### 2.5 Ranges

`a..b` is half-open, `a..=b` inclusive. Ranges are iterable and lazy;
`1..=3` in a `for` yields `1 2 3`. `len(1..50)` is `49`.

### 2.6 Calls, keyword arguments

```
click("#a")
click("#a", timeout: 5s)
f(1, 2, mode: "fast", retries: 3)
```

Positional arguments come first; `name: value` pairs follow (any order among
themselves). Positional after keyword is a syntax error. Unknown keyword
arguments to a builtin are a runtime error naming the argument.

### 2.7 Methods and fields

`x.name(args)` calls a method; `x.name` reads a field/property. On maps,
`m.k` is `m["k"]`. On native objects (pages, elements, actor refs) the host
defines what exists; an unknown name is a runtime error listing the
available names.

### 2.8 Lambdas

```
double = fn(x): x * 2
apply = fn(f, v): f(v)
print(apply(double, 4))          # 8

handler = fn(e):
    print(e)
    return e * 2
```

`fn(params): expr` is a single-expression lambda (the expression ends at the
end of the line). `fn(params):` followed by an indented block is a block
lambda. Lambdas capture variables by reference (closures).

---

## 3. Statements

### 3.1 Assignment

```
x = 1
m.key = 2
xs[0] = 3
```

Plain `name = value` **creates** a variable in the current function scope the
first time, and updates it after. Blocks (`if`, `for`, …) do not introduce a
new scope; functions do. Assigning to a name captured from an enclosing
function updates the captured variable (closures share). There is no `let`,
`const`, `var`.

Compound assignment (`+=`) does not exist in v0.1.

### 3.2 Expression statements

Any expression on its own line; its value is discarded. Typically a call.

### 3.3 `if / elif / else`

```
if count("li") > 10:
    print("many")
elif exists(".empty"):
    print("none")
else:
    print("some")
```

### 3.4 `for`

```
for a in all("a"):
    emit {href: a.attr("href")}

for i in 1..=3:
    print(i)

for k in keys(m):
    print("{k} = {m[k]}")
```

Iterates lists, ranges, strings (characters), maps (keys). Mutating a list
while iterating it is undefined behaviour (do not).

### 3.5 `while`, `loop`, `break`, `continue`

```
while not exists(".done"):
    sleep(200ms)

loop:
    msg = receive()
    if msg == nil:
        break
    handle(msg)
```

`break` / `continue` apply to the innermost loop; outside a loop they are a
compile error.

### 3.6 `fn` and `return`

```
fn login(user, pass):
    type("#username", user)
    type("#password", pass)
    click("#submit")
    return url()
```

Parameters may have defaults: `fn f(a, b: 2):`. Callers may pass keywords by
parameter name: `f(1, b: 5)`. Missing required parameter → runtime error.
`return` without a value returns `nil`; falling off the end returns `nil`.
Functions are values (`g = login`). Top-level `fn`s are hoisted: they may be
called from statements that appear earlier in the file.

### 3.7 `try / catch`

```
try:
    click("#maybe", timeout: 2s)
catch e:
    print("no button: {e.message}")
```

`catch` is required; the binding is optional (`catch:`). The caught value is
an error map with `message`, `selector` (or nil), `cdp_method` (or nil),
`line`. Errors are values; there is no `throw` — call `fail("message")` to
raise one. `exit` is not catchable.

### 3.8 `print`

`print(a, b, …)` writes the arguments separated by spaces plus `\n` to
stdout. Strings print raw; other values print as in `emit` (JSON-ish).

### 3.9 `emit`

```
emit {text: a.text(), href: a.attr("href")}
emit 42
```

Writes exactly one JSON line to stdout. Maps → objects, lists → arrays,
durations → milliseconds, functions → `null`, native objects → their JSON
form (elements → `{"selector": …}`, pages → `{"index": n, "url": …}`).
`emit` is a statement (no parentheses needed; `emit(x)` also parses because
`(x)` is an expression).

### 3.10 `exit`

```
exit        # code 0
exit(1)
```

Stops every task and handler, closes every browser Surf launched (profiles
given via `profile:` are kept; temp profiles are deleted), flushes stdout,
and exits the process with the code. Not catchable.

### 3.11 `sleep`

`sleep(2s)` — pauses the current task only.

---

## 4. Configuration blocks

A config block is declarative. It is read before anything runs and
**nothing launches until the first action that needs a page**. It may appear
only at the top level and is conventionally at the top of the file.

```
browser:
    path: "/usr/bin/chromium"      # optional; auto-discovery otherwise
    cdp: pipe                      # default | 9222 | "ws://…" (attach to running)
    pool: env("BROWSER_POOL_URL")  # remote provider (wss://) — same script, no code change
    proxy: "http://user:pass@host:8080"
    proxies: ["http://a:1", "http://b:1"]   # rotation list for shift_proxy()
    headless: false                # default: headed if a display exists
    virtual: true                  # Linux: Xvfb managed automatically
    size: "1280x800"
    profile: "./profiles/alice"    # persistent user-data-dir
    flags: ["--lang=en-US"]
    timeout: 30s                   # default auto-wait timeout for actions
    engine: chrome                 # `apostate` reserved (clear "not yet" error)
```

Rules:

- Body lines are `key: value` property lines. Values are full expressions
  evaluated once, at declaration time, in a context where only builtins
  (`env`, string functions, …) are available — not browser actions, not
  user functions.
- Bare identifiers that are not variables are accepted as **symbols** for
  the keys `cdp` (`pipe`) and `engine` (`chrome`, `apostate`); they are
  equivalent to the same word in quotes.
- Unknown keys are a `surf check` error.
- `browser work:` declares a **named** browser `work`; it is a value:
  `work.page(2).goto(…)`, `work.close()`. The unnamed `browser:` block
  configures the default browser (the one bare actions use). At most one
  unnamed block per file.
- `cdp: 9222` opens a listening port (websocket). `cdp: "ws://…"` attaches
  to an already-running browser (nothing is launched, nothing is closed on
  exit). `pool:` is a remote websocket provider; when set, `path`,
  `headless`, `virtual`, `profile`, `flags` are ignored.
- `headless` default: headed when a display exists (`DISPLAY` on Linux,
  always on macOS/Windows), otherwise headless. `virtual: true` on Linux
  starts an Xvfb and runs headed inside it; on other platforms it is a
  no-op with a warning.
- `proxy:` sets a browser-level proxy; credentials are stripped from the
  flag and supplied via CDP auth. `proxies:` is the list `shift_proxy()`
  rotates through.

---

## 5. Pages and implicit resolution

A **page** is a tab. The runtime maintains, per browser, the pages in
creation order (1-based) plus optional names.

- `page` is the sole page of the default browser, auto-created (and the
  browser auto-launched) on first use.
- `page(2)` is the second page; it is auto-created (as are any lower
  indexes that do not exist yet). `page("login")` is the page named
  `login`; names map to indices in creation order, so the first new name
  after `page(1)` exists becomes page 2.
- **Bare actions** (`goto click type fill press hover check select scroll
  text html attr value exists count all wait wait_gone wait_text wait_url
  eval screenshot pdf url title back reload cookies set_cookie …`) and
  `browser.goto(…)` resolve to the sole page. If several pages are open,
  the error is:

  ```
  several pages are open (1, 2, "login") — say which: page(2).click(…)
  ```

- `page.close()` closes a tab; indices of the remaining pages do not shift.
- Inside a spawned task, an actor, a `parallel for` body, or a handler
  body, the *first* bare action creates a page **private to that task** (so
  concurrent tasks never fight over one tab). Explicit `page(n)` always
  refers to the shared registry.

```
page(1).goto("https://example.com")
page(2).goto("https://example.org")
page(1).type("input", "surf")
```

### 5.1 Selectors

CSS is the default. Prefixes: `text=Buy now` (visible text, trimmed,
substring match, case-sensitive), `xpath=//a` or a selector beginning with
`//` (XPath). Every selector-taking action waits for the element to be
attached, visible, stable (not animating) and enabled, up to `timeout`
(per-call keyword or the browser default). Failure is a runtime error with
`selector` and the state that was not reached.

### 5.2 Actions (summary)

| action | returns | notes |
|--------|---------|-------|
| `goto(url, wait_until: "load")` | nil | `"load"`, `"domcontentloaded"`, `"networkidle"` |
| `click(sel, button: "left", count: 1)` | nil | real mouse events at element centre |
| `type(sel, text, delay: 0ms)` | nil | key-by-key |
| `fill(sel, text)` | nil | select-all + insert |
| `press(key)` / `press(sel, key)` | nil | `"Enter"`, `"Control+a"` |
| `hover(sel)` `check(sel)` `select(sel, value)` `scroll(sel)` | nil | |
| `text(sel)` `html(sel)` `attr(sel, name)` `value(sel)` | string | |
| `exists(sel)` | bool | no wait |
| `count(sel)` | int | no wait |
| `all(sel)` | list of elements | element methods: `text() html() attr(n) value() click() type(t) …` |
| `wait(sel)` `wait_gone(sel)` `wait_text(sel, t)` `wait_url(pattern)` | nil | |
| `eval(js)` / `eval(fn(): expr)` | JSON value | runs in the isolated world |
| `screenshot(path, full: false)` `pdf(path)` | nil | |
| `url()` `title()` | string | |
| `back()` `reload()` | nil | |
| `cookies()` | list of maps | `set_cookie({name, value, domain, …})` |

`eval(fn(): document.cookie)` — a zero-arg lambda whose body is translated
to JavaScript source text verbatim (the lambda body is **not** Surf; it is
sent as-is). Use `eval("…")` for anything but the simplest expressions.

---

## 6. Reactive handlers

```
on element_appears(".checkout"):
    click(".checkout .accept")

on navigation("*/login*"):
    print("went to {event.url}")

on dialog:
    event.accept()

on request("*/api/*"):
    print(event.method, event.url)

on response("*.json"):
    print(event.status)
```

- `on <event>(<args>):` at the top level (or `on message:` inside an actor).
  Parentheses are optional when there are no arguments.
- The handler body runs on **its own task** each time the event fires; the
  payload is bound to `event`. Handlers on the default browser observe the
  sole page (or every page if several exist).
- Handlers are registered before the first statement runs (they are hoisted
  declarations), and stay registered for the life of the program.
- `element_appears` is backed by an isolated-world MutationObserver and
  fires once per newly matching element.
- `on message:` inside an actor body receives each mailbox message as
  `event` (alternative to calling `receive()` in a loop).

### 6.1 Program lifetime

A program stays alive while any handler is registered or any task is
running. When the main body finishes and nothing is pending, the browser is
closed and the process exits 0. `exit` ends it immediately.

---

## 7. Concurrency

The runtime is single-threaded; tasks interleave at IO points.

### 7.1 `spawn`

```
fn crawl(url):
    goto(url)
    return title()

h = spawn crawl("https://example.com")
print(h.join())      # waits; re-raises the task's error
```

`spawn <callee>(<args>)` runs a `fn`, `task` or `actor` concurrently and
returns a handle (`join()`, `id`, `cancel()`). A spawned callee that uses
bare actions gets its own page.

### 7.2 `parallel for`

```
parallel for url in urls:
    limit: 4
    fail_fast: false
    goto(url)
    emit {url: url, title: title()}
```

Runs the body once per item, concurrently, at most `limit` at a time
(default: unlimited). Leading `key: value` lines are options (`limit`,
`fail_fast`); after the first statement they are a syntax error. The
statement completes when all items have finished. With `fail_fast: true`
(default `false`) the first error cancels the rest and is re-raised; with
`false` the errors are collected and raised together at the end. Each body
invocation gets its own page.

### 7.3 `task`

```
task fetch(url):
    retry: 5
    on_fail: shift_proxy()
    timeout: 60s
    fresh: true
    goto(url)
    return html("body")
```

A `task` is a `fn` with properties. Leading `key: value` lines are
properties; after the first statement they are a syntax error
(properties-before-statements rule).

| property | meaning |
|----------|---------|
| `retry: N` | re-run the body up to N more times on error |
| `on_fail: expr` | evaluated **lazily**, once per failure, before the retry (`shift_proxy()`) |
| `timeout: d` | whole-body timeout per attempt |
| `fresh: true` | run each attempt in a fresh browser context (new cookies/storage) |

Tasks are called like functions (`fetch(u)`) or spawned (`spawn fetch(u)`).

### 7.4 Actors

```
actor Scout():
    goto("https://example.com")
    broadcast({kind: "token", value: eval("document.title")})

actor Worker(n):
    msg = receive(timeout: 30s)       # nil on timeout
    if msg == nil:
        return
    goto("https://example.com/?t={msg.value}")
    emit {worker: n, id: self.id}
```

An actor runs with its own page and a mailbox. Inside an actor body:
`self.id` (int), `receive()` / `wait_for_message()` (block until a message;
`receive(timeout: d)` returns `nil` on timeout), `on message:` handler
form. Anywhere: `send(ref, msg)` (ref = spawn handle or id),
`broadcast(msg)` (every live actor except the sender). Messages are values
(deep-copied if mutable). An actor ends when its body returns.

### 7.5 Supervisors

```
supervisor Crew:
    strategy: one_for_one      # or one_for_all
    max_restarts: 3
    within: 60s
    spawn Scout()
    parallel for n in 1..=3:
        spawn Worker(n)
```

The body may contain only `spawn …` lines and `parallel for … :` whose body
is `spawn …` lines. A child that errors is restarted (`one_for_one`: just
it; `one_for_all`: every child) on a fresh page with cookies/storage
migrated. More than `max_restarts` restarts within `within` → the
supervisor fails and the error propagates. `supervisor` declarations are
hoisted and **start automatically** once all declarations are registered,
before the first statement runs (so the example above needs no start call).
`Crew.stop()` cancels its children; `Crew.join()` waits for all of them.

---

## 8. Builtin functions (host-independent)

`len keys values push pop join split trim upper lower contains starts_with
ends_with replace json parse_json int float str type range now random
read_file write_file append_file env fail sleep print`

`now()` returns milliseconds since the epoch. `random()` returns a float in
`[0, 1)`; `random(n)` an int in `[0, n)`.

---

## 9. Grammar (EBNF-ish)

```
program     := item*
item        := config | fn_decl | task_decl | actor_decl | supervisor_decl
             | handler | stmt

config      := "browser" IDENT? ":" NEWLINE INDENT prop+ DEDENT
prop        := IDENT ":" expr NEWLINE

fn_decl     := "fn" IDENT "(" params? ")" ":" block
task_decl   := "task" IDENT "(" params? ")" ":" NEWLINE INDENT prop* stmt+ DEDENT
actor_decl  := "actor" IDENT "(" params? ")" ":" NEWLINE INDENT prop* (handler | stmt)+ DEDENT
supervisor_decl := "supervisor" IDENT ":" NEWLINE INDENT prop* sup_stmt+ DEDENT
sup_stmt    := "spawn" postfix NEWLINE
             | "parallel" "for" IDENT "in" expr ":" NEWLINE INDENT prop* sup_stmt+ DEDENT
handler     := "on" IDENT ("(" args? ")")? ":" block
params      := param ("," param)*            param := IDENT (":" expr)?

block       := NEWLINE INDENT stmt+ DEDENT
stmt        := simple NEWLINE | compound
simple      := assign | "return" expr? | "break" | "continue"
             | "emit" expr | "exit" ("(" expr ")")? | "spawn" postfix | expr
assign      := place "=" expr               place := IDENT | postfix "." IDENT | postfix "[" expr "]"
compound    := "if" expr ":" block ("elif" expr ":" block)* ("else" ":" block)?
             | "for" IDENT "in" expr ":" block
             | "parallel" "for" IDENT "in" expr ":" NEWLINE INDENT prop* stmt+ DEDENT
             | "while" expr ":" block
             | "loop" ":" block
             | "try" ":" block "catch" IDENT? ":" block
             | fn_decl | handler

expr        := or
or          := and ("or" and)*
and         := not ("and" not)*
not         := "not" not | cmp
cmp         := range (("==" | "!=" | "<" | "<=" | ">" | ">=") range)?
range       := add ((".." | "..=") add)?
add         := mul (("+" | "-") mul)*
mul         := unary (("*" | "/" | "%") unary)*
unary       := "-" unary | postfix
postfix     := primary ( "(" args? ")" | "." IDENT ("(" args? ")")? | "[" expr "]" )*
args        := arg ("," arg)* ","?           arg := (IDENT ":")? expr
primary     := INT | FLOAT | STRING | DURATION | "true" | "false" | "nil"
             | IDENT | "(" expr ")" | list | map | lambda
list        := "[" (expr ("," expr)* ","?)? "]"
map         := "{" (entry ("," entry)* ","?)? "}"
entry       := (IDENT | STRING | "[" expr "]") ":" expr
lambda      := "fn" "(" params? ")" ":" (expr | block)
```

Notes for the parser:

- `prop` lines are distinguished from statements by the shape `IDENT ":"
  …` at the start of a line **where a property is permitted** (config
  bodies; the leading region of task/actor/supervisor/`parallel for`
  bodies). An `IDENT ":"` line after the first statement in such a body is
  the error `property lines must come before statements`. At the top
  level and in other blocks `x: 1` is a syntax error.
- `emit`, `exit`, `spawn` are statements, not expressions.
- A `:` at the end of a line always opens a block, except inside brackets.

---

## 10. Errors

Runtime errors carry: `message`, `span` (file:line:col), `selector`,
`cdp_method`, and a cause chain. The CLI prints:

```
error: element not visible after 30s (selector: #submit) [DOM.getContentQuads]
  --> examples/login.surf:7:1
   |
 7 | click("#submit")
   | ^^^^^^^^^^^^^^^^
```

Exit codes: `0` ok, `1` runtime error, `2` syntax error, `3` no browser
found, or whatever `exit(n)` said.

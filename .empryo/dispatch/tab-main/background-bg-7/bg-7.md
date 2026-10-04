# Agent: bg-7 (code)
Task: # SURF — shared brief for every node (read fully before acting)

Repo: /Users/chaser/surf. Host: macOS arm64, cargo 1.97 stable, Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, Playwright Chromium builds under ~/Library/Caches/ms-playwright, Apostate (a Chromium fork

## Tool Calls
[read] 
[read] 
[read] 
[read] 
[grep] /surf_vm::|Globals::|Op::|DeclKind|UpvalueDesc|CompiledProgram|Vm::new|\.fork\(\)/
[grep] /^futures|^indexmap|^serde_json|captures|Closure::new|Closure \{/
[edit_file] crates/surf-vm/src/bytecode.rs
[edit_file] crates/surf-vm/src/value.rs
[multi_edit] crates/surf-vm/src/error.rs
[edit_file] crates/surf-vm/src/host.rs
[edit_file] crates/surf-vm/src/compiler.rs
[edit_file] crates/surf-vm/src/vm.rs
[edit_file] crates/surf-vm/src/stdlib.rs
[multi_edit] crates/surf-vm/src/compiler.rs
[multi_edit] crates/surf-vm/src/lib.rs
[edit_file] crates/surf-vm/src/vm.rs
[shell]
[multi_edit] crates/surf-vm/src/vm.rs
[edit_file] crates/surf-vm/tests/fixtures.rs
[shell]
[shell]
[multi_edit] crates/surf-vm/src/compiler.rs
[shell]
[shell]
[shell]
[shell]
[edit_file] crates/surf-vm/tests/fixtures.rs
[shell]
[multi_edit] crates/surf-vm/tests/fixtures.rs
[shell]
[shell]
[read] 
[multi_edit] docs/architecture.md
[multi_edit] docs/language.md
[multi_edit] TASKS.md
[edit_file] crates/surf-runtime/src/lib.rs
[shell]
[shell]

## Findings
**surf-vm implemented (task 6) — API notes for runtime implementers:**
crates/surf-vm is no longer a stub (commits cfc3527, 8f2032c, a038767). Contract-relevant additions:
- `surf_vm::compile_with_source(name, source, program)` keeps a line table so `catch e: e.line` works; `surf-runtime/src/lib.rs::run_source` already uses it. `compile()` still exists (line = nil).
- Global resolution for a bare call `f(...)`: Host::resolve_global(f) → stdlib Globals → Host::call_global(f). So user fns/tasks/actors must be served by `resolve_global` / `call_global` (the VM only delivers `Declaration`s via `Host::declare` and stores nothing itself). `Host::resolve_global` is called BEFORE the stdlib for every unknown name — keep it side-effect free for names like `len`/`print`.
- New defaulted Host method `spawn_method(vm, receiver, name, args)` for `spawn work.fetch(1)`; `spawn name(args)` → `Host::spawn`.
- `Closure.captures` is now `Vec<Rc<RefCell<Value>>>` (shared cells). `Declaration::Task/Actor.params` come from `body.func.params`.
- `RuntimeError::exit(code)` / `exit_code()` / `is_cancelled()` / `is_catchable()` / `to_diagnostic()` / `to_value(line)`. `exit` propagates as an Err with an `ExitRequest` cause — the runtime must check `err.exit_code()` on the result of `vm.run`.
- `NativeObject` gained defaulted `as_any()` and `iter_items()`; calling a native value invokes `call_method(vm, "__call__", args)`. `surf_vm::Range` is a native (lazy ranges).
- Stdlib: `Globals::stdlib()` registers the §8 builtins + `json`/`fs` objects (fs routes through Host::fs). `print` → Host::print, `sleep` → Host::sleep, `env` → Host::env.
- Test host pattern: crates/surf-vm/tests/fixtures.rs `TestHost` shows how to run task bodies with retry/on_fail (`Prop::resolve(vm)`), spawn via `vm.call`, parallel_for sequentially.
- Known parser quirk logged in TASKS.md: a statement starting with `[` (e.g. `[1,2].each(f)`) fails to parse.

## Agent Summary
surf-vm is implemented, tested, documented and committed (3 commits: `cfc3527` feat(vm), `8f2032c` test(vm), `a038767` docs(vm)). `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings` and `cargo test --workspace` are green; the `AutomationControlled` switch in launch.rs was not touched.

**Built**
- `crates/surf-vm/src/bytecode.rs` — final `Copy` op set (`CallGlobal`, `CallMethod`, `Spawn`/`SpawnMethod`, `ParallelFor`, `TryBegin/TryEnd`, `IterStart/Next/End`, `ParamDefault`, `Declare{kind,name,alias,props}`), `Function` with line table.
- `compiler.rs` — AST → bytecode: function-scoped locals via a pre-scan of assigned names, upvalues (boxed-on-capture cells), jump patching, for-in over list/map/range/string, while/loop/break/continue (with try/iterator unwinding), try/catch, interpolation, trailing-kwargs call convention, hoisted `fn/task/actor/supervisor/on/browser:` compiled as closures of `<main>` and delivered via `Op::Declare` → `Host::declare` before the first statement; config symbols (`cdp: pipe`) and prop symbols (`strategy: one_for_one`) become strings; `Prop::Lazy` thunks for non-literal props; `compile_with_source` keeps lines for `e.line`.
- `vm.rs` — single-loop stack interpreter; script calls push frames, natives are awaited boxed futures; `Vm::call` re-entrant from natives; handler-stack try/catch scoped per call; `exit`/cancel as uncatchable marker causes; cancellation at back-edges and before natives; `call_value`; global order host → stdlib → `Host::call_global`.
- `value.rs` — equality/compare/deep_clone, print/emit formatting, `format_duration`, `Range` native, `NativeObject::as_any/iter_items`, `Args::require/check_kwargs`.
- `stdlib.rs` — all §8 builtins + `assert`, str/list/map/duration methods, `json`/`fs` objects, `BuiltinFn` values, xorshift `random`.
- `error.rs` — `exit()/cancelled()/exit_code()/is_catchable()/to_diagnostic()/to_value()`.
- `host.rs` — defaulted `spawn_method`. `surf-runtime` now calls `compile_with_source`.

**Tests** (`tests/fixtures.rs` + 11 fixtures/.out): arith, strings, lists/maps, control flow, functions/closures/recursion/defaults/kwargs, errors (selector/cdp_method/line, nested, through natives), durations, json/emit, declarations (config/task retry+lazy on_fail/actor/supervisor/handlers/spawn/parallel for), builtins, exit; plus cold-start (<20 ms debug, measured ~1 ms), uncatchable exit, cancel token, compile-error diagnostic, runtime→Diagnostic rendering.

**Deferred** (TASKS.md): parser rejects statements starting with `[`; `spawn` of local function values; >64 params defaults; `now()/random()` use SystemTime (no `Host::now`); read-before-assign yields nil. `wasm32-unknown-unknown` check could not run here (target std not installed) — the crate carries no tokio/fs/process deps.

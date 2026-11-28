//! flow-strip.rs — makes Flow-typed JavaScript parseable. NOT a twin of a JS module: the JS lane's
//! acorn never parsed Flow either (it skipped `@flow` files outright), and `oxc_parser` has no Flow
//! mode. 2026-10-01: the X-ray of the React repo listed 1160 files as "flow syntax - not parsed" —
//! `packages/shared`, `react-reconciler`, `react-dom` — the whole core. Every one of them had no
//! symbols and no import edges, so "who imports shared/getComponentNameFromType" answered "nobody".
//!
//! ## The route: normalise the Flow-only syntax, let oxc's TypeScript grammar do the rest
//! Flow and TypeScript share the bulk of their annotation grammar (`x: T`, `T[]`, `Array<T>`,
//! `import type`, `export type`, `a?: T`, generics, class `implements`), and oxc parses TypeScript.
//! So the pre-pass does NOT try to find where a type ends (that is a whole parser); it rewrites only
//! the handful of constructs TypeScript cannot read, and `parseJs` then parses the result as TSX:
//!
//! | Flow | becomes |
//! |---|---|
//! | `?T` (maybe) | `T` |
//! | `{\| a: T \|}` (exact) | `{ a: T }` |
//! | `(x: T)` (type cast) | `(x   )` — blanked up to the `)` |
//! | `import typeof X` / `{typeof X}` | `import type X` / `{type X}` |
//! | `opaque type T = U` | `type T = U` |
//! | `declare export …` | `export …` |
//! | `declare module.exports: T;` | blank |
//! | `%checks` / `%checks(expr)` | blank |
//! | `+x: T` / `-x: T` (variance) | `x: T` |
//! | `<T: Bound>` | `<T extends Bound>` |
//! | `<T>(x) => …` in expression position | `<T,>(x) => …` (TSX would read `<T>` as an element) |
//! | `...A` inside an exact/inexact object TYPE | `A` |
//!
//! ## What is preserved, and the one rule that keeps the graph honest
//! **Every edit keeps the newlines.** `parseJs` derives each node's `start_line`/`end_line` by counting
//! `\n` up to a byte offset, so a rewrite that moved a newline would shift every symbol below it.
//! Casts and sigils are blanked with spaces (`\n` kept); the few rewrites that change a token's
//! length (`typeof` → `type  `, `:` → ` extends`) never touch a line break.
//!
//! The pass is a tokenizer (comments, strings, template literals, regexes are skipped, so a `?` in a
//! URL or a `(a: b)` in a comment is never rewritten) plus a paren matcher. It is deliberately
//! tolerant: anything it does not understand is left alone, and the caller reads oxc's own error
//! list afterwards — a file that still does not parse is reported `degraded`/`fail`, never "ok".
//!
//! ## Hostile input (audit 2026-10-01, F-06)
//! The source is whatever a repository holds, so the pass must terminate on any bytes. `skip_template` is iterative with a nesting cap (`MAX_TEMPLATE_DEPTH`; it used to
//! recurse once per `${` and a 100 000-deep one overflowed the stack, which aborts the whole process), the bracket matcher is linear, regex scanning has a total budget,
//! and every scan that looked ahead to "the end of something" has a window AND is charged to a step budget (`Ctx::spend`, plus a wall-clock deadline). Input that blows a
//! budget, nests absurdly or is larger than `MAX_INPUT_BYTES` comes back UNSTRIPPED with the `budget` flag (`stripFlowChecked`): `parseJsFlow` reports it as a failure instead of
//! handing oxc a file it would have to read as TypeScript.
//!
//! Known gaps, left as they are (each costs a `degraded` health row, not a crash): Flow `match`
//! expressions (`match (x) { 'a' => … }`, 6 compiler fixtures), unnamed parameters of a METHOD type in an
//! object type (`writeFile(A | B, data: C): T`), a class property whose annotation is a function type over
//! an object (`prop: ({a: T}) => void = …`), an arrow whose return type is a parenthesised function type.
//! (2026-10-01, facebook/react: 1177 Flow files, 1139 clean · 13 degraded · 25 unreadable became
//! 1166 clean · 0 degraded · 11 unreadable: the 6 match fixtures, 2 libdefs, 3 ambiguous annotations.)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum K {
    Ident,
    Punct,
    Str,
    Tmpl,
    Regex,
    Num,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    k: K,
    s: usize,
    e: usize,
    /// a line break sits between the previous token and this one
    nl: bool,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80
}
fn is_ident_part(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

/// Keywords after which a `/` opens a regex and a `(` opens an expression (a cast candidate).
const EXPR_KEYWORDS: &[&str] = &[
    "return", "typeof", "instanceof", "in", "of", "new", "delete", "void", "throw", "case", "do", "else", "yield", "await",
    "default",
];

/// Multi-character punctuators. `<` and `>` are deliberately NEVER merged (`>>`, `>=`, `<=`, `<<`):
/// the generic-list scanner counts them one at a time, and `Array<Array<T>>` must close twice.
const PUNCTS: &[&str] = &[
    "...", "===", "!==", "**=", "&&=", "||=", "??=", "=>", "==", "!=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=",
    "&=", "|=", "^=", "**",
];

/// How many `${ … }` interpolations may nest inside one another before the file is called hostile (real code stops at 3 or 4).
const MAX_TEMPLATE_DEPTH: usize = 256;
/// How many brackets may be open at once before the file is called hostile: oxc's parser recurses once per level, and no real file goes past a few dozen.
const MAX_BRACKET_DEPTH: usize = 2_000;
/// The largest source the pass will look at (the atlas lane caps a file at 1 MiB; the sync lane now does too).
pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
/// Look-ahead windows, in tokens: a `(…)` longer than `CAST_WINDOW` is not a type cast, a parameter list or a function type longer than `LIST_WINDOW` is left alone, and a scan
/// that hunts for the end of a statement gives up after `STMT_WINDOW`.
const CAST_WINDOW: usize = 512;
const LIST_WINDOW: usize = 3_000;
const STMT_WINDOW: usize = 2_000;

fn skip_string(b: &[u8], mut i: usize) -> usize {
    let q = b[i];
    i += 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == q => return i + 1,
            // a JS string cannot span lines: an unterminated one (an apostrophe in JSX text) must not eat the file
            b'\n' => return i,
            _ => i += 1,
        }
    }
    b.len()
}

/// `i` is at the opening backtick; returns the index after the closing one. `${ … }` is skipped by
/// brace depth, with nested strings/templates, so a backtick inside an interpolation does not end it.
/// ITERATIVE (audit 2026-10-01, F-06: the recursion made `${${${…` 100 000 deep a stack overflow). `too_deep` is set, and the rest of the
/// file consumed, when the templates nest more than `MAX_TEMPLATE_DEPTH` deep.
fn skip_template(b: &[u8], start: usize, too_deep: &mut bool) -> usize {
    // one frame per open template: `None` = in its text, `Some(d)` = inside a `${` at brace depth d
    let mut stack: Vec<Option<usize>> = vec![None];
    let mut i = start + 1;
    while i < b.len() {
        match stack.last().copied().flatten() {
            None => match b[i] {
                b'\\' => i += 2,
                b'`' => {
                    stack.pop();
                    i += 1;
                    if stack.is_empty() {
                        return i;
                    }
                }
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    if stack.len() >= MAX_TEMPLATE_DEPTH {
                        *too_deep = true;
                        return b.len();
                    }
                    stack.push(Some(1));
                    i += 2;
                }
                _ => i += 1,
            },
            Some(d) => match b[i] {
                b'{' => {
                    *stack.last_mut().unwrap() = Some(d + 1);
                    i += 1;
                }
                b'}' => {
                    if d <= 1 {
                        stack.pop();
                    } else {
                        *stack.last_mut().unwrap() = Some(d - 1);
                    }
                    i += 1;
                }
                b'\'' | b'"' => i = skip_string(b, i),
                b'`' => {
                    if stack.len() >= MAX_TEMPLATE_DEPTH {
                        *too_deep = true;
                        return b.len();
                    }
                    stack.push(None);
                    i += 1;
                }
                _ => i += 1,
            },
        }
    }
    b.len()
}

/// `i` is at the opening `/`. A regex ends at the next unescaped `/` outside a class on the SAME line;
/// otherwise this was a division and `None` is returned.
fn skip_regex(b: &[u8], mut i: usize, budget: &mut usize) -> Option<usize> {
    i += 1;
    let mut in_class = false;
    while i < b.len() {
        // audit 2026-10-01, F-06: each `/` rescanned its own line (`/[/[/[…`: quadratic); the scans now share one budget, and a `/` the budget cannot pay for is a division
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        match b[i] {
            b'\\' => i += 1,
            b'\n' => return None,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => {
                i += 1;
                while i < b.len() && is_ident_part(b[i]) {
                    i += 1;
                }
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The tokens, and whether the text nests templates absurdly deep (hostile).
fn lex(src: &str) -> (Vec<Tok>, bool) {
    let b = src.as_bytes();
    let mut too_deep = false;
    let mut regex_budget: usize = 16 * b.len() + 4_096;
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0usize;
    let mut nl = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            nl = true;
            i += 1;
        } else if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                if b[i] == b'\n' {
                    nl = true;
                }
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else if c == b'\'' || c == b'"' {
            let e = skip_string(b, i);
            toks.push(Tok { k: K::Str, s: i, e, nl });
            nl = false;
            i = e;
        } else if c == b'`' {
            let e = skip_template(b, i, &mut too_deep);
            // a template can hold line breaks; none of the rewrites look inside one
            toks.push(Tok { k: K::Tmpl, s: i, e, nl });
            nl = false;
            i = e;
        } else if is_ident_start(c) || (c == b'#' && b.get(i + 1).map_or(false, |&n| is_ident_start(n))) {
            let s = i;
            i += 1;
            while i < b.len() && is_ident_part(b[i]) {
                i += 1;
            }
            toks.push(Tok { k: K::Ident, s, e: i, nl });
            nl = false;
        } else if c.is_ascii_digit() {
            let s = i;
            while i < b.len() && (is_ident_part(b[i]) || b[i] == b'.') {
                i += 1;
            }
            toks.push(Tok { k: K::Num, s, e: i, nl });
            nl = false;
        } else {
            // a `/` is a regex where an expression can start, a division where one has just ended
            if c == b'/' {
                let regex_ok = match toks.last() {
                    None => true,
                    Some(p) => match p.k {
                        K::Punct => match &src[p.s..p.e] {
                            ")" | "]" => false,
                            "}" => nl,
                            // `</div>` — a closing JSX tag, not a regex
                            "<" => false,
                            _ => true,
                        },
                        K::Ident => EXPR_KEYWORDS.contains(&&src[p.s..p.e]),
                        _ => false,
                    },
                };
                if regex_ok {
                    if let Some(e) = skip_regex(b, i, &mut regex_budget) {
                        toks.push(Tok { k: K::Regex, s: i, e, nl });
                        nl = false;
                        i = e;
                        continue;
                    }
                }
            }
            let rest = &src[i..];
            let len = PUNCTS
                .iter()
                .find(|p| rest.starts_with(**p) && !(**p == "?." && rest.as_bytes().get(2).map_or(false, |d| d.is_ascii_digit())))
                .map_or(1, |p| p.len());
            // a non-ASCII byte that is not an identifier start cannot reach here (>= 0x80 is ident), so `len` is a char boundary
            toks.push(Tok { k: K::Punct, s: i, e: i + len, nl });
            nl = false;
            i += len;
        }
    }
    (toks, too_deep)
}

struct Ctx<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    /// for an opening bracket token: the index of its closing partner
    close: Vec<Option<usize>>,
    /// the work the look-ahead scans may still do (see `spend`)
    budget: std::cell::Cell<u64>,
    spent: std::cell::Cell<u64>,
    deadline: std::time::Instant,
    blown: std::cell::Cell<bool>,
}

/// The wall-clock ceiling of one pass. A real file takes milliseconds; this only ever fires on input built to make the pass spin.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

impl<'a> Ctx<'a> {
    /// Charge `n` steps of look-ahead to the budget; false (and the pass is abandoned) once it is spent or the deadline has passed.
    fn spend(&self, n: usize) -> bool {
        if self.blown.get() {
            return false;
        }
        let spent = self.spent.get() + n as u64;
        self.spent.set(spent);
        if spent > self.budget.get() || (spent & 0xFFFF < n as u64 && std::time::Instant::now() > self.deadline) {
            self.blown.set(true);
            return false;
        }
        true
    }
    fn blown(&self) -> bool {
        self.blown.get()
    }
    fn text(&self, i: usize) -> &'a str {
        let t = self.toks[i];
        &self.src[t.s..t.e]
    }
    fn is(&self, i: usize, s: &str) -> bool {
        i < self.toks.len() && self.toks[i].k != K::Str && self.toks[i].k != K::Tmpl && self.text(i) == s
    }
    fn ident(&self, i: usize, s: &str) -> bool {
        i < self.toks.len() && self.toks[i].k == K::Ident && self.text(i) == s
    }
    fn adjacent(&self, a: usize, b: usize) -> bool {
        self.toks[a].e == self.toks[b].s
    }
}

/// Pair the brackets. Linear: a stray closer pops to the nearest opener OF ITS KIND, which used to be a scan of the whole stack per closer (`rposition`) - quadratic on a
/// file of stray `)` over a deep stack (audit 2026-10-01, F-06). The second value is the deepest nesting seen (hostile past `MAX_BRACKET_DEPTH`).
fn match_brackets(src: &str, toks: &[Tok]) -> (Vec<Option<usize>>, usize) {
    let mut close = vec![None; toks.len()];
    // (token index, kind) of every open bracket, innermost last
    let mut stack: Vec<(usize, u8)> = Vec::new();
    // per kind: (position in `stack`, token index) of its openers; entries the stack has since been cut below are stale and dropped when met
    let mut by_kind: [Vec<(usize, usize)>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut deepest = 0usize;
    for (i, t) in toks.iter().enumerate() {
        if t.k != K::Punct || t.e - t.s != 1 {
            continue;
        }
        match src.as_bytes()[t.s] {
            c @ (b'(' | b'[' | b'{') => {
                let kind = match c {
                    b'(' => 0,
                    b'[' => 1,
                    _ => 2,
                };
                by_kind[kind].push((stack.len(), i));
                stack.push((i, c));
                deepest = deepest.max(stack.len());
            }
            c @ (b')' | b']' | b'}') => {
                let kind = match c {
                    b')' => 0,
                    b']' => 1,
                    _ => 2,
                };
                // tolerate a stray closer (JSX text, an unterminated string): pop to the matching opener or ignore
                while let Some(&(pos, ti)) = by_kind[kind].last() {
                    by_kind[kind].pop();
                    if stack.get(pos).map_or(false, |&(si, _)| si == ti) {
                        close[ti] = Some(i);
                        stack.truncate(pos);
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    (close, deepest)
}

/// An edit replaces `src[s..e]` with `text`. Blanking keeps every `\n` in the range.
struct Edit {
    s: usize,
    e: usize,
    text: String,
}

fn blank(src: &str, s: usize, e: usize) -> Edit {
    let text: String = src.as_bytes()[s..e].iter().map(|&c| if c == b'\n' { '\n' } else { ' ' }).collect();
    Edit { s, e, text }
}

/// `(` at `i` is the start of a parenthesised EXPRESSION (so `(x: T)` can be a cast), not a call's
/// arguments or a declaration's parameters.
fn opens_expression(cx: &Ctx, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let p = i - 1;
    match cx.toks[p].k {
        K::Punct => !matches!(cx.text(p), ")" | "]" | "}" | ">"),
        K::Ident => EXPR_KEYWORDS.contains(&cx.text(p)),
        _ => false,
    }
}

/// After the `)` at `j`: is this group an arrow function's parameter list (`) =>` or `): T =>`)?
fn followed_by_arrow(cx: &Ctx, j: usize) -> bool {
    if cx.is(j + 1, "=>") {
        return true;
    }
    if !cx.is(j + 1, ":") {
        return false;
    }
    let mut depth = 0i32;
    let mut k = j + 2;
    while k < cx.toks.len() && k < j + 90 {
        match cx.text(k) {
            "(" | "[" | "{" if cx.toks[k].k == K::Punct => depth += 1,
            ")" | "]" | "}" if cx.toks[k].k == K::Punct => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            "=>" if depth == 0 && cx.toks[k].k == K::Punct => return true,
            ";" | "," | "=" if depth == 0 && cx.toks[k].k == K::Punct => return false,
            _ => {}
        }
        k += 1;
    }
    false
}

/// The index of the colon that makes `( … )` a type cast: the first `:` at depth 0 with no pending
/// `?` (a ternary's own colon) and no earlier top-level comma (that is a parameter list).
fn cast_colon(cx: &Ctx, i: usize, j: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut pending_q = 0i32;
    // a cast's colon sits near its opening paren: the scan looks at `CAST_WINDOW` tokens at most (audit 2026-10-01, F-06: it ran to the closing paren for every `(`, and
    // `((((…))))` made that quadratic) and is charged to the budget
    let hi = j.min(i + 1 + CAST_WINDOW);
    if !cx.spend(hi.saturating_sub(i)) {
        return None;
    }
    for k in i + 1..hi {
        let t = cx.toks[k];
        if t.k != K::Punct {
            continue;
        }
        match cx.text(k) {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            "?" if depth == 0 => pending_q += 1,
            "," if depth == 0 => return None,
            // `<` right after an operator or an opening paren starts a JSX element, not a comparison: the group is
            // JSX (`return (<p>Name: {x}</p>)`) and its colons are text, never a cast
            "<" if depth == 0 && k + 1 < j && (cx.toks[k + 1].k == K::Ident || cx.is(k + 1, ">")) && (k == i + 1 || (cx.toks[k - 1].k == K::Punct && !matches!(cx.text(k - 1), ")" | "]" | "}"))) => return None,
            ":" if depth == 0 => {
                if pending_q > 0 {
                    pending_q -= 1;
                } else {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// `<` at `i` opens a generic parameter list whose entries carry Flow bounds/variance (`<T: Bound>`,
/// `<+T, -U>`). Returns the index of its closing `>`.
fn generic_list_end(cx: &Ctx, i: usize) -> Option<usize> {
    if !cx.spend(160.min(cx.toks.len().saturating_sub(i))) {
        return None;
    }
    let mut depth = 0i32;
    let mut paren = 0i32;
    for k in i..cx.toks.len().min(i + 160) {
        if cx.toks[k].k != K::Punct {
            continue;
        }
        match cx.text(k) {
            "<" if paren == 0 => depth += 1,
            ">" if paren == 0 => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            "(" | "[" | "{" => paren += 1,
            ")" | "]" | "}" => {
                paren -= 1;
                if paren < 0 {
                    return None;
                }
            }
            ";" if paren == 0 => return None,
            _ => {}
        }
    }
    None
}

fn is_sigil(cx: &Ctx, i: usize) -> bool {
    cx.toks[i].k == K::Punct && matches!(cx.text(i), "+" | "-")
}

/// Token `i` sits inside the RETURN type of an arrow function — `(x): A | null => body`, where `null =>` and
/// `(A) =>` are the end of the type and the arrow itself, not Flow's unparenthesised function type. Scans back
/// over the type to the `:` and asks whether a `)` precedes it.
fn in_return_type(cx: &Ctx, i: usize) -> bool {
    let mut depth = 0i32;
    let mut k = i;
    while k > 0 && i - k < 40 {
        k -= 1;
        if cx.toks[k].k != K::Punct {
            continue;
        }
        match cx.text(k) {
            ")" | "]" | "}" | ">" => depth += 1,
            "(" | "[" | "{" | "<" => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            ":" if depth == 0 => return k > 0 && cx.is(k - 1, ")"),
            ";" | "=" | "," | "=>" if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// `(` at `i` opens the parameters of `declare function name(`, `declare export function name(` — where a Flow
/// libdef writes `(string, number): void` with no parameter names.
fn declare_function_params(cx: &Ctx, i: usize) -> bool {
    if i < 3 || cx.toks[i - 1].k != K::Ident || !cx.ident(i - 2, "function") {
        return false;
    }
    cx.ident(i - 3, "declare") || (i >= 4 && (cx.ident(i - 3, "export") || cx.ident(i - 3, "default")) && (cx.ident(i - 4, "declare") || (i >= 5 && cx.ident(i - 5, "declare"))))
}

/// A function type's entry that TypeScript could not take for a parameter — anything but `name`, `name: T`,
/// `name?: T`, `name = v`, a destructuring pattern, each with an optional leading `...` — becomes `_pN: <entry>`
/// (`...Array<T>` becomes `..._rest: Array<T>`). Entries that are already names are left alone, so an
/// ordinary arrow function's parameters are never touched.
fn name_unnamed_params(cx: &Ctx, open: usize, close: usize, edits: &mut Vec<Edit>) {
    // a parameter list is short; a group of thousands of tokens is not one, and `(((…)=>)=>)=>` would rescan it at every level
    if close.saturating_sub(open) > LIST_WINDOW || !cx.spend(close.saturating_sub(open) + 1) {
        return;
    }
    const RESERVED: &[&str] = &["void", "null", "true", "false", "this", "typeof", "new", "delete", "in", "of"];
    let mut entry_start = open + 1;
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut index = 0usize;
    let mut k = open + 1;
    while k <= close {
        let at_end = k == close;
        let mut split = at_end;
        if !at_end && cx.toks[k].k == K::Punct {
            match cx.text(k) {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                "<" => angle += 1,
                ">" => angle = (angle - 1).max(0),
                "," if depth == 0 && angle == 0 => split = true,
                _ => {}
            }
        }
        if split {
            let mut a = entry_start;
            let rest = a < k && cx.is(a, "...");
            if rest {
                a += 1;
            }
            // a maybe-type's `?` is blanked by another edit but is still a token here
            while a < k && cx.is(a, "?") {
                a += 1;
            }
            if a < k {
                let first = cx.toks[a];
                let named = match first.k {
                    K::Ident => a + 1 == k || matches!(cx.text(a + 1), ":" | "=") || (cx.is(a + 1, "?") && cx.is(a + 2, ":")) ,
                    K::Punct if matches!(cx.text(a), "{" | "[") => cx.close[a].map_or(false, |c| c + 1 == k || matches!(cx.text(c + 1), ":" | "=" | "?")),
                    _ => false,
                };
                let reserved = first.k == K::Ident && a + 1 == k && RESERVED.contains(&cx.text(a));
                if !named || reserved {
                    let text = if rest { format!("_r{index}: ") } else { format!("_p{index}: ") };
                    edits.push(Edit { s: cx.toks[entry_start + usize::from(rest)].s, e: cx.toks[entry_start + usize::from(rest)].s, text });
                }
            }
            index += 1;
            entry_start = k + 1;
        }
        k += 1;
    }
}

/// Rewrites the Flow-only syntax of `src` into something oxc's TSX grammar reads, newline-for-newline.
/// Pure and total: input it does not understand comes back unchanged.
pub fn stripFlow(src: &str) -> String {
    stripFlowChecked(src).0
}

/// `stripFlow`, and whether the pass gave up (`true` = the `budget` flag): the text is then the INPUT, unstripped - it is too big, nests absurdly deep, or would have made a scan spin
/// (see the module doc, "Hostile input"). A caller that parses the result as TypeScript should treat the flag as a failure to read the file, not as a file with nothing in it.
pub fn stripFlowChecked(src: &str) -> (String, bool) {
    if src.len() > MAX_INPUT_BYTES {
        return (src.to_string(), true);
    }
    let (toks, too_deep) = lex(src);
    if too_deep {
        return (src.to_string(), true);
    }
    let (close, deepest) = match_brackets(src, &toks);
    if deepest > MAX_BRACKET_DEPTH {
        return (src.to_string(), true);
    }
    let n_toks = toks.len() as u64;
    let cx = Ctx {
        src,
        toks,
        close,
        // a real file spends 1 to 17 steps per token (measured 2026-10-01 on csstype's 345 KB libdef, lightningcss' ast.js.flow and a small module); this is 100 per token
        // plus a floor - several times above that, and about a second of work in a debug build at the size cap
        budget: std::cell::Cell::new(100 * n_toks + 200_000),
        spent: std::cell::Cell::new(0),
        deadline: std::time::Instant::now() + DEADLINE,
        blown: std::cell::Cell::new(false),
    };
    let out = strip_with(&cx);
    if cx.blown() {
        return (src.to_string(), true);
    }
    (out, false)
}

fn strip_with(cx: &Ctx) -> String {
    let src = cx.src;
    let n = cx.toks.len();
    let mut edits: Vec<Edit> = Vec::new();

    for i in 0..n {
        if cx.blown() {
            return String::new();
        }
        let t = cx.toks[i];
        match t.k {
            K::Punct => match cx.text(i) {
                // ?T  — a maybe type. `?` can only start a type where an expression could not start at all
                "?" => {
                    let after_open = i > 0 && cx.toks[i - 1].k == K::Punct && matches!(cx.text(i - 1), ":" | "<" | "," | "(" | "|" | "&" | "=" | "=>" | "[");
                    if after_open && i + 1 < n && !matches!(cx.text(i + 1), ":" | "." | "=") {
                        edits.push(blank(src, t.s, t.e));
                    }
                }
                // {| a: T |}  and  {||}
                "|" | "||" => {
                    let opens = i > 0 && cx.is(i - 1, "{") && cx.adjacent(i - 1, i);
                    let closes = cx.text(i) == "|" && i + 1 < n && cx.is(i + 1, "}") && cx.adjacent(i, i + 1);
                    if opens || closes {
                        edits.push(blank(src, t.s, t.e));
                    }
                }
                // (x: T)
                "(" => {
                    if let Some(j) = cx.close[i] {
                        if opens_expression(cx, i) && !followed_by_arrow(cx, j) {
                            if let Some(c) = cast_colon(cx, i, j) {
                                edits.push(blank(src, cx.toks[c].s, cx.toks[j].s));
                            }
                        }
                        // `(Error, Array<T>) => string`: a Flow function type names no parameters; TypeScript needs names
                        // …and `<T>(?() => T, ?T) => void` as a RETURN type: the parameters of a generic function type name nothing either (named entries are left alone, so a generic arrow is untouched)
                        let generic_fn_type = i > 0 && cx.is(i - 1, ">") && cx.is(j + 1, "=>");
                        if (cx.is(j + 1, "=>") && !in_return_type(cx, i)) || declare_function_params(cx, i) || generic_fn_type {
                            name_unnamed_params(cx, i, j, &mut edits);
                        }
                    }
                }
                // %checks  /  %checks(expr)
                "%" => {
                    if cx.ident(i + 1, "checks") && cx.adjacent(i, i + 1) {
                        let mut end = cx.toks[i + 1].e;
                        if cx.is(i + 2, "(") {
                            if let Some(j) = cx.close[i + 2] {
                                end = cx.toks[j].e;
                            }
                        }
                        edits.push(blank(src, t.s, end));
                    }
                }
                // +x: T / -x: T  (variance)
                "+" | "-" => {
                    let before_ok = i == 0
                        || (cx.toks[i - 1].k == K::Punct && matches!(cx.text(i - 1), "{" | "," | ";" | "}"))
                        || cx.ident(i - 1, "static")
                        // `{| +a: T |}` — the exact-object opener is a token of its own
                        || (cx.is(i - 1, "|") && i >= 2 && cx.is(i - 2, "{"));
                    if before_ok && i + 1 < n && cx.adjacent(i, i + 1) {
                        let named = cx.toks[i + 1].k == K::Ident && (cx.is(i + 2, ":") || (cx.is(i + 2, "?") && cx.is(i + 3, ":")));
                        let indexer = cx.is(i + 1, "[") && cx.close[i + 1].map_or(false, |c| cx.is(c + 1, ":"));
                        if named || indexer {
                            edits.push(blank(src, t.s, t.e));
                        }
                    }
                }
                "<" => {
                    // <T: Bound>, <A, B: Bound>, <+T>, and `<T>(x) =>` in expression position
                    let prev_is_decl_kw = i >= 2 && cx.toks[i - 2].k == K::Ident && matches!(cx.text(i - 2), "function" | "class" | "type" | "interface" | "opaque");
                    let name_or_expr = i == 0 || cx.toks[i - 1].k == K::Ident || matches!(cx.text(i - 1), "=" | "(" | "," | ":" | "=>" | "?" | "&&" | "||" | "[" | "{" | ";" | ">");
                    let sigil = i + 1 < n && is_sigil(cx, i + 1);
                    let mut rewrote = false;
                    if name_or_expr && i + 2 < n && (cx.toks[i + 1].k == K::Ident || sigil) {
                        if let Some(end) = generic_list_end(cx, i) {
                            let decl_like = prev_is_decl_kw || cx.is(end + 1, "(") || cx.is(end + 1, "=") || cx.is(end + 1, "{");
                            let mut local: Vec<Edit> = Vec::new();
                            let mut depth = 0i32;
                            let mut paren = 0i32;
                            let mut expect_name = true;
                            for k in i..=end {
                                if cx.toks[k].k != K::Punct {
                                    expect_name = false;
                                    continue;
                                }
                                match cx.text(k) {
                                    "<" => {
                                        depth += 1;
                                        expect_name = depth == 1;
                                    }
                                    ">" => depth -= 1,
                                    "(" | "[" | "{" => paren += 1,
                                    ")" | "]" | "}" => paren -= 1,
                                    "," if depth == 1 && paren == 0 => expect_name = true,
                                    "+" | "-" if depth == 1 && paren == 0 && expect_name => local.push(blank(src, cx.toks[k].s, cx.toks[k].e)),
                                    ":" if depth == 1 && paren == 0 && k > 0 && cx.toks[k - 1].k == K::Ident => {
                                        local.push(Edit { s: cx.toks[k].s, e: cx.toks[k].e, text: " extends".to_string() });
                                    }
                                    _ => {}
                                }
                            }
                            if decl_like && !local.is_empty() {
                                edits.extend(local);
                                rewrote = true;
                            }
                        }
                    }
                    if !rewrote
                        && i + 2 < n
                        && cx.toks[i + 1].k == K::Ident
                        && cx.is(i + 2, ">")
                        && cx.is(i + 3, "(")
                        && (i == 0 || (cx.toks[i - 1].k == K::Punct && matches!(cx.text(i - 1), "=" | "(" | "," | ":" | "=>" | "?" | "&&" | "||" | "[" | ";")) || cx.ident(i - 1, "return"))
                    {
                        // a generic ARROW (`<T>(x: T) => x`) — TSX would open a JSX element at `<T>`; `<T,>` is unambiguous
                        if let Some(j) = cx.close[i + 3] {
                            if cx.is(j + 1, "=>") || cx.is(j + 1, ":") {
                                edits.push(Edit { s: cx.toks[i + 1].e, e: cx.toks[i + 1].e, text: ",".to_string() });
                            }
                        }
                    }
                }
                // `*` as Flow's existential type (`cb: () => *`) — only where a type can stand and a multiplication cannot
                "*" => {
                    let type_before = i > 0 && cx.toks[i - 1].k == K::Punct && matches!(cx.text(i - 1), ":" | "<" | "," | "|" | "&" | "(" | "=>" | "=");
                    let type_after = i + 1 < n && cx.toks[i + 1].k == K::Punct && matches!(cx.text(i + 1), "," | ")" | ">" | ";" | "|" | "}" | "&" | "=>");
                    if type_before && type_after {
                        edits.push(Edit { s: t.s, e: t.e, text: "any".to_string() });
                    }
                }
                // `@@iterator()` — Flow's name for a well-known symbol method in a libdef
                "@" => {
                    if i + 2 < n && cx.is(i + 1, "@") && cx.adjacent(i, i + 1) && cx.toks[i + 2].k == K::Ident && cx.adjacent(i + 1, i + 2) {
                        edits.push(blank(src, t.s, cx.toks[i + 1].e));
                    }
                }
                // `{ a: T, ... }` — an inexact object type. A bare `...` before `}` / `|` is never valid JavaScript.
                "..." => {
                    if i + 1 < n && (cx.is(i + 1, "}") || cx.is(i + 1, "|")) {
                        edits.push(blank(src, t.s, t.e));
                    }
                }
                // `{ ...A, b: T }` / `{ a: T, ... }` inside a type: handled with the object, below
                _ => {}
            },
            K::Ident => {
                // `T => U` is Flow's unparenthesised function type (`then(resolve: T => mixed)`); TypeScript needs
                // `(T) => U`. For an expression arrow `x => y` the parentheses change nothing, so no context is needed —
                // except where the identifier is the RETURN type of an arrow (`(x): T => body`).
                if cx.is(i + 1, "=>") && !(i > 0 && cx.is(i - 1, ".")) {
                    if !in_return_type(cx, i) {
                        // `void => mixed`: `void` cannot name a parameter
                        let open = if matches!(cx.text(i), "void" | "null" | "true" | "false" | "this") { "(_p0: " } else { "(" };
                        edits.push(Edit { s: t.s, e: t.s, text: open.to_string() });
                        edits.push(Edit { s: t.e, e: t.e, text: ")".to_string() });
                    }
                }
                match cx.text(i) {
                "typeof" => {
                    // import typeof X from '…'  /  import typeof * as X from '…'  /  import {typeof X} from '…'  → `type`, same length
                    let direct = i > 0 && cx.ident(i - 1, "import");
                    let in_braces = || {
                        if !(i + 1 < n && cx.toks[i + 1].k == K::Ident) {
                            return false;
                        }
                        let mut k = i;
                        while k > 0 && i - k < 60 {
                            k -= 1;
                            if cx.ident(k, "import") {
                                return true;
                            }
                            if cx.is(k, ";") || cx.ident(k, "from") {
                                return false;
                            }
                        }
                        false
                    };
                    if direct || in_braces() {
                        edits.push(Edit { s: t.s, e: t.e, text: "type  ".to_string() });
                    }
                }
                // `declare class Navigator mixins NavigatorID, NavigatorLanguage {` - a libdef's mixin list has no TypeScript form (the members still come from the body)
                "mixins" => {
                    let in_header = (i.saturating_sub(14)..i).any(|k| cx.ident(k, "class"));
                    if in_header && i > 0 && !cx.is(i - 1, ".") && !cx.is(i + 1, "(") && !cx.is(i + 1, ":") {
                        if let Some(b) = (i + 1..n.min(i + 60)).find(|&b| cx.is(b, "{") || cx.is(b, ";")) {
                            if cx.is(b, "{") { edits.push(blank(src, t.s, cx.toks[b].s)); }
                        }
                    }
                }
                // `interface {…}` / `interface extends A, B {…}` as a TYPE (Flow's inline interface; `T: interface {}` bounds, `type P = interface extends Promise<any> {…}`): an
                // object type, with the base types intersected in. A declaration has a name after the word, so only these two shapes are touched.
                "interface" => {
                    if !(i > 0 && cx.is(i - 1, ".")) {
                        if cx.is(i + 1, "{") {
                            edits.push(blank(src, t.s, t.e));
                        } else if cx.ident(i + 1, "extends") {
                            let (mut k, mut angle, mut commas) = (i + 2, 0i32, Vec::new());
                            let mut brace = None;
                            while k < n && k < i + 60 {
                                if cx.toks[k].k == K::Punct {
                                    match cx.text(k) {
                                        "<" => angle += 1,
                                        ">" => angle = (angle - 1).max(0),
                                        "," if angle == 0 => commas.push(k),
                                        "{" if angle == 0 => { brace = Some(k); break; }
                                        ";" | "=" | ")" => break,
                                        _ => {}
                                    }
                                }
                                k += 1;
                            }
                            if let Some(b) = brace {
                                edits.push(blank(src, t.s, cx.toks[i + 1].e));
                                for c in commas { edits.push(Edit { s: cx.toks[c].s, e: cx.toks[c].e, text: "&".to_string() }); }
                                edits.push(Edit { s: cx.toks[b].s, e: cx.toks[b].s, text: "& ".to_string() });
                            }
                        }
                    }
                }
                // set name(value: T): void { … } — Flow allows a return type on a setter, TypeScript does not
                "set" => {
                    if i + 2 < n && cx.toks[i + 1].k == K::Ident && cx.is(i + 2, "(") {
                        if let Some(j) = cx.close[i + 2] {
                            if cx.is(j + 1, ":") {
                                if let Some(m) = (j + 2..n.min(j + 40)).find(|&m| cx.is(m, "{") || cx.is(m, ";")) {
                                    edits.push(blank(src, cx.toks[j + 1].s, cx.toks[m].s));
                                }
                            }
                        }
                    }
                }
                "opaque" => {
                    if cx.ident(i + 1, "type") {
                        edits.push(blank(src, t.s, t.e));
                        // opaque type Name<…>: Supertype = T  — the supertype bound has no TypeScript form
                        let mut k = i + 3;
                        if cx.is(k, "<") {
                            if let Some(e) = generic_list_end(cx, k) {
                                k = e + 1;
                            }
                        }
                        if cx.is(k, ":") {
                            let mut m = k + 1;
                            while m < n && !cx.is(m, "=") && !cx.is(m, ";") && m < k + 40 {
                                m += 1;
                            }
                            if m < n {
                                edits.push(blank(src, cx.toks[k].s, cx.toks[m].s));
                                if cx.is(m, ";") {
                                    edits.push(Edit { s: cx.toks[m].s, e: cx.toks[m].s, text: " = any".to_string() });
                                }
                            }
                        } else if cx.is(k, ";") {
                            // `declare opaque type T;` — a type with nothing behind it
                            edits.push(Edit { s: cx.toks[k].s, e: cx.toks[k].s, text: " = any".to_string() });
                        }
                    }
                }
                // Flow component syntax. A type, `component(...props: P)`, has no TypeScript form: it is `any`.
                // A declaration, `component Name(…) renders T {` / `hook useName(…) {`, is a function.
                "component" | "hook" => {
                    let word = cx.text(i);
                    let after_ident = i + 1 < n && cx.toks[i + 1].k == K::Ident && (cx.is(i + 2, "(") || cx.is(i + 2, "<"));
                    if after_ident && !(i > 0 && cx.is(i - 1, ".")) {
                        edits.push(Edit { s: t.s, e: t.e, text: if word == "component" { "function ".to_string() } else { "function".to_string() } });
                        // the optional `renders T` clause between the parameters and the body
                        let open = if cx.is(i + 2, "(") { Some(i + 2) } else { generic_list_end(cx, i + 2).filter(|&e| cx.is(e + 1, "(")).map(|e| e + 1) };
                        if let Some(j) = open.and_then(|o| cx.close[o]) {
                            if cx.ident(j + 1, "renders") {
                                let mut m = j + 1;
                                while m < n && !cx.is(m, "{") && m < j + 40 {
                                    m += 1;
                                }
                                if m < n {
                                    edits.push(blank(src, cx.toks[j + 1].s, cx.toks[m].s));
                                }
                            }
                        }
                    } else if word == "component" && cx.is(i + 1, "(") && !(i > 0 && (cx.is(i - 1, ".") || cx.toks[i - 1].k == K::Ident && !cx.ident(i - 1, "as"))) {
                        let type_pos = i == 0 || cx.toks[i - 1].k == K::Ident || matches!(cx.text(i - 1), ":" | "<" | "|" | "&" | "?" | "," | "=" | "(" | "=>");
                        if let (true, Some(j)) = (type_pos, cx.close[i + 1]) {
                            let mut e = blank(src, t.s, cx.toks[j].e);
                            e.text.replace_range(0..3, "any");
                            edits.push(e);
                        }
                    }
                }
                "declare" => {
                    if cx.ident(i + 1, "export") {
                        edits.push(blank(src, t.s, t.e));
                    } else if (cx.ident(i + 1, "module") || cx.ident(i + 1, "namespace")) && !cx.is(i + 2, ".") {
                        // `declare module 'x' { declare function … }` — TypeScript: "'declare' modifier cannot be used in an already ambient context"
                        if let Some(b) = (i + 2..n.min(i + 5)).find(|&b| cx.is(b, "{")) {
                            if let Some(e) = cx.close[b].filter(|&e| cx.spend(e - b)) {
                                for m in b + 1..e {
                                    if cx.ident(m, "declare") {
                                        edits.push(blank(src, cx.toks[m].s, cx.toks[m].e));
                                    }
                                }
                            }
                        }
                    } else if cx.ident(i + 1, "module") && cx.is(i + 2, ".") {
                        // declare module.exports: T;   (the type may span lines: `declare module.exports: (\n argv: A,\n) => B;` - flow-typed/npm/minimist, 2026-10-01)
                        let mut k = i;
                        // only a stray closer moves `depth`: a bracketed group is stepped over WHOLE (its partner is already known), so the scan costs the statement's own top-level
                        // tokens, not the size of a big `declare module.exports: { …300 members… };`. It is one statement, so it gives up after STMT_WINDOW of them (it used to run to
                        // the end of the file when nothing ended it, from every occurrence) and every step is charged to the budget.
                        let mut depth = 0i32;
                        let mut steps = 0usize;
                        while k < n && steps < STMT_WINDOW && !(depth == 0 && cx.is(k, ";")) && !(k > i + 3 && depth == 0 && cx.toks[k].nl && !(cx.toks[k - 1].k == K::Punct && matches!(cx.text(k - 1), ":" | "=>" | "|" | "&" | "," | "(" | "{" | "[" | "<"))) {
                            steps += 1;
                            if !cx.spend(1) {
                                break;
                            }
                            if cx.toks[k].k == K::Punct {
                                match cx.text(k) {
                                    "(" | "[" | "{" => match cx.close[k] {
                                        Some(c) => {
                                            k = c + 1;
                                            continue;
                                        }
                                        None => depth += 1,
                                    },
                                    ")" | "]" | "}" => depth -= 1,
                                    _ => {}
                                }
                            }
                            k += 1;
                        }
                        let end = if k < n && cx.is(k, ";") { cx.toks[k].e } else if k < n { cx.toks[k].s } else { src.len() };
                        edits.push(blank(src, t.s, end));
                    }
                }
                _ => {}
                }
            }
            _ => {}
        }
    }

    let mut type_objs: std::collections::HashSet<usize> = std::collections::HashSet::new();
    late_rules(cx, &mut edits, &mut type_objs);

    // ── spreads inside object TYPES: `type P = {...A, b: T}` / `{| ...A |}` ───────────────────────
    // TypeScript has no spread in a type literal. A `{` is a type object when it follows `type X =`,
    // `type X<…> =`, `: ` in an annotation, or `<`/`|`/`&` in a type position; of those the only
    // one that is cheap and certain is the alias form, plus an object whose braces were exact (`{|`).
    for i in 0..n {
        if cx.blown() {
            return String::new();
        }
        if !cx.is(i, "{") {
            continue;
        }
        let exact = i + 1 < n && (cx.is(i + 1, "|") || cx.is(i + 1, "||")) && cx.adjacent(i, i + 1);
        let alias = {
            // `type Name =` / `type Name<…> =` / `export type Name =` immediately before the brace
            let mut k = i;
            let mut found = false;
            if k > 0 && cx.is(k - 1, "=") {
                k -= 1;
                // walk back over an optional <…> to the name
                if k > 0 && cx.is(k - 1, ">") {
                    let mut depth = 0i32;
                    // a type alias's own parameter list is short: the walk back is windowed (it ran to the start of the file on `> = { > = {…`)
                    let floor = k.saturating_sub(STMT_WINDOW);
                    cx.spend(k - floor);
                    while k > floor {
                        k -= 1;
                        match cx.text(k) {
                            ">" if cx.toks[k].k == K::Punct => depth += 1,
                            "<" if cx.toks[k].k == K::Punct => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                found = k >= 2 && cx.toks[k - 1].k == K::Ident && cx.ident(k - 2, "type");
            }
            found
        };
        // …and the object type of a parameter's annotation (`}: {...Position, +source: ?string}`), found by `late_rules`
        if !(exact || alias || type_objs.contains(&i)) {
            continue;
        }
        let Some(end) = cx.close[i] else { continue };
        if !cx.spend(end - i) {
            return String::new();
        }
        let mut depth = 0i32;
        for k in i + 1..end {
            if cx.toks[k].k != K::Punct {
                continue;
            }
            match cx.text(k) {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                "..." if depth == 0 => {
                    // `...A,` → ` A,` (a member named A, which TypeScript reads); a bare `...` (inexact) → nothing.
                    // `...$Exact<P>,` / `...Foo.Bar<T>,` has no member form at all: the whole entry goes (up to its comma).
                    let mut m = k + 1;
                    let mut angle = 0i32;
                    let mut generic = false;
                    while m < end {
                        if cx.toks[m].k == K::Punct {
                            match cx.text(m) {
                                "<" => { angle += 1; generic = true; }
                                ">" => angle = (angle - 1).max(0),
                                "," | ";" | "|" if angle == 0 => break,
                                "(" | "[" | "{" => { if let Some(c) = cx.close[m] { m = c; } }
                                _ => {}
                            }
                        }
                        m += 1;
                    }
                    if generic { let last = if m < end && cx.is(m, ",") { m } else { m.saturating_sub(1).max(k) }; edits.push(blank(src, cx.toks[k].s, cx.toks[last].e)); }
                    else { edits.push(blank(src, cx.toks[k].s, cx.toks[k].e)); }
                }
                _ => {}
            }
        }
    }

    apply(src, edits)
}

/// The rules that need a whole parameter list or object in view (2026-10-01, from the React survey's last 38 files):
///  - `x?: T = v`, `...rest?: T` (TypeScript: "a parameter cannot have a question mark and an initializer", "a rest parameter cannot be optional") - the `?` goes;
///  - `a?: T, b: U` (an optional parameter before a required one) - the earlier `?` goes;
///  - `Foo<>` (Flow: all type arguments defaulted; TypeScript: "type argument list cannot be empty") - becomes `Foo<any>`;
///  - a mapped type's trailing comma (`{ [K in keyof O]: T, }` - TypeScript allows no comma after the one member) - the comma goes;
///  - the object type of a parameter's annotation is recorded in `type_objs`, so a spread in it is removed like one in `type P = {...A}`.
fn late_rules(cx: &Ctx, edits: &mut Vec<Edit>, type_objs: &mut std::collections::HashSet<usize>) {
    let n = cx.toks.len();
    let src = cx.src;
    // ── parameter lists ──────────────────────────────────────────────────────────────────────────
    struct Entry { rest: bool, opt: Option<usize>, default: bool, required: bool }
    for open in 0..n {
        if cx.blown() { return }
        if !cx.is(open, "(") { continue }
        let Some(close) = cx.close[open] else { continue };
        // a parameter list is short: a `( … )` of thousands of tokens is a call or an IIFE, and every nested level of `(((…)))` would rescan it (audit 2026-10-01, F-06)
        if close - open > LIST_WINDOW { continue }
        if !cx.spend(2 * (close - open) + 1) { return }
        let (mut depth, mut angle, mut start) = (0i32, 0i32, open + 1);
        let mut bounds: Vec<(usize, usize)> = Vec::new();
        for k in open + 1..=close {
            let end_of_list = k == close;
            let mut split = end_of_list;
            if !end_of_list && cx.toks[k].k == K::Punct {
                match cx.text(k) {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    "<" => angle += 1,
                    ">" => angle = (angle - 1).max(0),
                    "," if depth == 0 && angle == 0 => split = true,
                    _ => {}
                }
            }
            if split { if start < k { bounds.push((start, k)); } start = k + 1; }
        }
        let mut entries: Vec<Option<Entry>> = Vec::new();
        for &(a, b) in &bounds {
            let mut p = a;
            let rest = cx.is(p, "...");
            if rest { p += 1 }
            let name_end = if p < b && cx.toks[p].k == K::Ident { p + 1 } else if p < b && (cx.is(p, "{") || cx.is(p, "[")) { match cx.close[p] { Some(c) => c + 1, None => { entries.push(None); continue } } } else { entries.push(None); continue };
            let opt = if cx.is(name_end, "?") && cx.is(name_end + 1, ":") && cx.adjacent(name_end, name_end + 1) && name_end < b { Some(name_end) } else { None };
            let colon = if opt.is_some() { Some(name_end + 1) } else if cx.is(name_end, ":") && name_end < b { Some(name_end) } else { None };
            // a `=` at the entry's top level is a default value (`=>` is a token of its own)
            let (mut d, mut an, mut default) = (0i32, 0i32, false);
            for k in name_end..b {
                if cx.toks[k].k != K::Punct { continue }
                match cx.text(k) {
                    "(" | "[" | "{" => d += 1,
                    ")" | "]" | "}" => d -= 1,
                    "<" => an += 1,
                    ">" => an = (an - 1).max(0),
                    "=" if d == 0 && an == 0 => { default = true; break }
                    _ => {}
                }
            }
            // an annotation that starts with an object type: remember the brace
            if let Some(c) = colon {
                let mut q = c + 1;
                while q < b && cx.is(q, "?") { q += 1 }
                if q < b && cx.is(q, "{") { type_objs.insert(q); }
            }
            let shaped = name_end == b || colon.is_some() || default;
            entries.push(Some(Entry { rest, opt, default, required: shaped && !rest && opt.is_none() && !default }));
        }
        let last_required = entries.iter().rposition(|e| e.as_ref().map_or(false, |e| e.required));
        for (idx, e) in entries.iter().enumerate() {
            let Some(e) = e else { continue };
            let Some(q) = e.opt else { continue };
            if e.rest || e.default || last_required.map_or(false, |r| r > idx) {
                edits.push(blank(src, cx.toks[q].s, cx.toks[q].e));
            }
        }
    }
    // ── `Foo<>` ───────────────────────────────────────────────────────────────────────────────────
    for i in 1..n.saturating_sub(1) {
        if cx.is(i, "<") && cx.is(i + 1, ">") && cx.adjacent(i, i + 1) && cx.toks[i - 1].k == K::Ident && cx.adjacent(i - 1, i)
            && !EXPR_KEYWORDS.contains(&cx.text(i - 1)) && !matches!(cx.text(i - 1), "extends" | "implements" | "export" | "import" | "class" | "function")
        {
            edits.push(Edit { s: cx.toks[i].e, e: cx.toks[i].e, text: "any".to_string() });
        }
    }
    // ── a mapped type with a trailing comma ───────────────────────────────────────────────────────
    for i in 0..n.saturating_sub(3) {
        if !(cx.is(i, "[") && cx.toks[i + 1].k == K::Ident && cx.ident(i + 2, "in")) { continue }
        let Some(j) = cx.close[i] else { continue };
        if !(cx.is(j + 1, ":") || (cx.is(j + 1, "?") && cx.is(j + 2, ":"))) { continue }
        let (mut depth, mut angle) = (0i32, 0i32);
        // one member, so one statement: windowed, and charged (it ran to the end of the file for every `[K in …]`)
        let stop = n.min(j + 2 + STMT_WINDOW);
        if !cx.spend(stop.saturating_sub(j + 2)) { return }
        for k in j + 2..stop {
            if cx.toks[k].k != K::Punct { continue }
            match cx.text(k) {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => { if depth == 0 { break } depth -= 1 }
                "<" => angle += 1,
                ">" => angle = (angle - 1).max(0),
                "," if depth == 0 && angle == 0 => { if cx.is(k + 1, "}") { edits.push(blank(src, cx.toks[k].s, cx.toks[k].e)); } break }
                ";" if depth == 0 => break,
                _ => {}
            }
        }
    }
}

/// Flow files whose JSX text holds a bare `>` or `}` (`Extension -> Link Handling`; Babel and Flow read it, oxc refuses it: "Did you mean `{'>'}` or `&gt;`?"). The text of a JSX
/// element is never indexed, so the character is swapped for a space at the offsets oxc names, and the file is read again (a handful of rounds: each pass can reveal the next).
/// Takes the already-stripped text; returns it unchanged when oxc names no such character. The caller asks only when its own first error was this one.
pub fn repair_jsx_text(stripped: &str) -> String {
    use oxc_allocator::Allocator;
    use oxc_parser::{ParseOptions, Parser};
    use oxc_span::SourceType;
    let mut cur = stripped.to_string();
    for _ in 0..12 {
        let offsets: Vec<usize> = {
            let allocator = Allocator::default();
            let ret = Parser::new(&allocator, &cur, SourceType::tsx()).with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() }).parse();
            ret.diagnostics.errors()
                .filter(|d| d.message.contains("Did you mean `{'"))
                .filter_map(|d| d.labels.as_slice().first().map(|l| l.offset() as usize))
                .filter(|&o| matches!(cur.as_bytes().get(o), Some(b'>') | Some(b'}')))
                .collect()
        };
        if offsets.is_empty() { break }
        let mut bytes = cur.into_bytes();
        for o in offsets { bytes[o] = b' '; }
        cur = String::from_utf8(bytes).unwrap_or_default();
    }
    cur
}

fn apply(src: &str, mut edits: Vec<Edit>) -> String {
    // at one start: an insertion first, then the WIDER range (so a statement blanked whole wins over a token inside it)
    edits.sort_by(|a, b| a.s.cmp(&b.s).then((a.e != a.s).cmp(&(b.e != b.s))).then(b.e.cmp(&a.e)));
    let mut out = String::with_capacity(src.len() + 32);
    let mut at = 0usize;
    for e in edits {
        // two rules claimed the same bytes (a cast inside a cast's blanked tail): the earlier one wins
        if e.s < at {
            continue;
        }
        out.push_str(&src[at..e.s]);
        out.push_str(&e.text);
        at = e.e;
    }
    out.push_str(&src[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> usize {
        s.matches('\n').count()
    }

    #[test]
    fn maybe_type_loses_its_question_mark_but_a_ternary_keeps_it() {
        let out = stripFlow("function f(a: ?string, b?: number): ?Array<?T> { return a ? b : c; }");
        assert!(!out.contains("?string") && !out.contains("?Array") && !out.contains("?T>"), "{out}");
        assert!(out.contains("b?: number"), "optional param untouched: {out}");
        assert!(out.contains("a ? b : c"), "ternary untouched: {out}");
    }

    #[test]
    fn exact_object_types_become_plain_ones() {
        let out = stripFlow("type P = {| a: number, b: string |};\ntype E = {||};\nconst x = a || {};");
        assert!(out.contains("{  a: number, b: string  }"), "{out}");
        assert!(out.contains("type E = {  };") || out.contains("type E = {   }"), "{out}");
        assert!(out.contains("a || {}"), "logical or untouched: {out}");
    }

    #[test]
    fn a_type_cast_is_blanked_to_the_closing_paren_and_arrow_params_are_not() {
        let src = "const a = (x: any);\nconst b = ((y: any): T);\nconst f = (z: number): string => String(z);\nconst g = (p: T) => p;\nfoo((q: Q), 1);\nconst c = cond ? (u) : v;";
        let out = stripFlow(src);
        assert!(out.contains("const a = (x     );"), "{out}");
        assert!(out.contains("const b = ((y     )   );"), "{out}");
        assert!(out.contains("(z: number): string =>"), "arrow params kept: {out}");
        assert!(out.contains("(p: T) => p"), "arrow params kept: {out}");
        assert!(out.contains("foo((q   ), 1)"), "cast as an argument: {out}");
        assert!(out.contains("cond ? (u) : v"), "ternary untouched: {out}");
    }

    #[test]
    fn call_arguments_and_declarations_are_never_read_as_casts() {
        let src = "function f(a: number) {}\nclass A { m(x: T) {} }\nfoo(a ? b : c);\nif (x) { y(); }\nasync (v: V) => v;";
        assert_eq!(stripFlow(src), src);
    }

    #[test]
    fn import_typeof_becomes_import_type_with_the_same_length() {
        let src = "import typeof TFoo from './foo';\nimport {typeof Bar, type Baz, qux} from './bar';";
        let out = stripFlow(src);
        assert_eq!(out.len(), src.len());
        assert!(out.contains("import type   TFoo") || out.contains("import type  TFoo"), "{out}");
        assert!(out.contains("{type   Bar") || out.contains("{type  Bar"), "{out}");
    }

    #[test]
    fn opaque_declare_and_checks_are_erased() {
        let src = "opaque type ID = string;\nexport opaque type Tok = number;\ndeclare export function f(x: mixed): boolean %checks(typeof x === 'string');\ndeclare module.exports: { foo: number };\nfunction g(x: mixed): boolean %checks { return !!x; }";
        let out = stripFlow(src);
        assert!(!out.contains("opaque"), "{out}");
        assert!(!out.contains("declare"), "{out}");
        assert!(!out.contains("%checks"), "{out}");
        assert!(!out.contains("module.exports"), "{out}");
        assert_eq!(lines(&out), lines(src));
    }

    #[test]
    fn variance_sigils_and_bounded_generics_are_rewritten() {
        let src = "type O = { +a: number, -b: string, +[k: string]: T };\nclass C<+T, -U: Base> { +x: T; static +y: number = 1; }\nfunction f<T: Object = {}>(x: T): T { return x; }";
        let out = stripFlow(src);
        assert!(out.contains("{  a: number,  b: string,  [k: string]: T }"), "{out}");
        assert!(out.contains("class C< T,  U extends Base>"), "{out}");
        assert!(out.contains("function f<T extends Object = {}>"), "{out}");
        assert!(out.contains("  x: T;") && out.contains("static  y: number"), "{out}");
    }

    #[test]
    fn a_generic_arrow_gets_a_comma_so_tsx_does_not_read_a_jsx_element() {
        let out = stripFlow("const id = <T>(x: T): T => x;\nconst el = <b>(hi)</b>;");
        assert!(out.contains("<T,>(x: T)"), "{out}");
        assert!(out.contains("<b>(hi)</b>"), "JSX untouched: {out}");
    }

    #[test]
    fn spread_inside_an_object_type_is_dropped_but_not_in_an_object_literal() {
        let out = stripFlow("type P = {...A, b: number, ...};\nconst o = {...a, b: 1};");
        assert!(!out.contains("...A") && !out.contains(", ..."), "{out}");
        assert!(out.contains("{...a, b: 1}"), "object literal untouched: {out}");
    }

    #[test]
    fn strings_comments_templates_and_regexes_are_never_rewritten() {
        let src = "const u = 'http://x/?a=1: ?b (c: d)';\n// (x: T) ?y\n/* {| a |} */\nconst t = `${a ? b : c} (k: v) ?z`;\nconst r = /(a: ?b)/.test(s);";
        assert_eq!(stripFlow(src), src);
    }

    #[test]
    fn line_count_is_exactly_preserved() {
        let src = "const a = (\n  x\n  : any\n);\ntype P = {|\n  a: ?number,\n|};\nfoo(1);\n";
        let out = stripFlow(src);
        assert_eq!(lines(&out), lines(src));
    }

    /// Dev tool, not a check: `FLOW_ROOT=/path/to/repo cargo test -p antiloki-core-rust --lib flow_survey -- --ignored --nocapture`
    /// parses every Flow-typed `.js` under the root the way the indexer does and prints how many read cleanly,
    /// how many recovered with errors and how many oxc gave up on — with the most common first error of each,
    /// which is where the next rewrite rule belongs. `FLOW_OUT=file` writes every non-clean path with its first
/// error; `FLOW_SHOW=path` prints that file's stripped text instead.
    #[test]
    #[ignore]
    fn flow_survey() {
        use std::collections::BTreeMap;
        let Ok(root) = std::env::var("FLOW_ROOT") else { return };
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if p.is_dir() {
                    if name != "node_modules" && !name.starts_with('.') { walk(&p, out); }
                } else if name.ends_with(".js") || name.ends_with(".mjs") || name.ends_with(".jsx") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(std::path::Path::new(&root), &mut files);
        files.sort();
        if let Ok(show) = std::env::var("FLOW_SHOW") {
            let src = std::fs::read_to_string(&show).unwrap();
            println!("{}", stripFlow(&src));
            return;
        }
        let (mut clean, mut degraded, mut failed, mut total) = (0, 0, 0, 0);
        let mut firsts: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else { continue };
            let rel = f.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
            if !(crate::codebase_atlas_ingest::isFlow(&rel, &src) || crate::codebase_atlas_ingest::looksFlowTyped(&src)) { continue; }
            total += 1;
            match crate::codebase_parse_js::parseJsFlow(&src, &rel) {
                Ok(v) if v.get("has_error").and_then(|x| x.as_bool()) == Some(true) => {
                    degraded += 1;
                    firsts.entry(format!("degraded: {}", v["first_error"].as_str().unwrap_or(""))).or_default().push(rel);
                }
                Ok(_) => clean += 1,
                Err(e) => {
                    failed += 1;
                    firsts.entry(format!("FAIL: {e}")).or_default().push(rel);
                }
            }
        }
        println!("flow files {total}: clean {clean} · degraded {degraded} · failed {failed}");
        if let Ok(out) = std::env::var("FLOW_OUT") {
            let lines: Vec<String> = firsts.iter().flat_map(|(m, v)| v.iter().map(move |f| format!("{f}\t{m}"))).collect();
            let _ = std::fs::write(out, lines.join("\n"));
        }
        let mut by_n: Vec<_> = firsts.into_iter().collect();
        by_n.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
        for (msg, v) in by_n.iter().take(25) {
            println!("{:>5}  {msg}   e.g. {}", v.len(), v.iter().take(2).cloned().collect::<Vec<_>>().join(", "));
        }
    }

    // ── the gaps left by the React survey (2026-10-01: 1177 Flow files, 1139 clean · 13 degraded · 25 unreadable) ────────────────────────────
    // Each fixture is the shape found in facebook/react, small enough to read; `reads` asks the indexer's own door (strip, then oxc), so a rule that rewrites the text but leaves a file
    // oxc still refuses does not pass.
    fn reads(src: &str) -> bool {
        match crate::codebase_parse_js::parseJsFlow(src, "x.js") { Ok(v) => v.get("has_error").and_then(|x| x.as_bool()) != Some(true), Err(_) => false }
    }

    #[test]
    fn an_optional_parameter_with_an_initializer_or_before_a_required_one_or_as_a_rest_reads() {
        assert!(reads("// @flow\nfunction f(x?: number = 1, y: string = 'a') {}\n"), "x?: T = v");
        assert!(reads("// @flow\nfunction g(a: string, flags?: number, cb: (e: Error) => void): void {}\n"), "an optional before a required parameter");
        assert!(reads("// @flow\ntype C = { timeLog(label?: string, ...data?: Array<any>): void };\n"), "an optional rest");
        // the ordinary cases are left exactly alone
        let ok = "function h(a: number, b?: string, c = 3, ...rest: Array<number>) {}\nconst t = cond ? (x) : y;";
        assert_eq!(stripFlow(ok), ok);
        let out = stripFlow("function f(x?: number = 1) {}");
        assert!(out.contains("x : number = 1") || out.contains("x : number= 1") || !out.contains("x?"), "{out}");
    }

    #[test]
    fn empty_type_arguments_get_a_placeholder_but_a_jsx_fragment_does_not() {
        assert!(reads("// @flow\ntype H = (event: MessageEvent<>) => mixed;\nfunction f(e: MessageEvent<>) {}\n"));
        let jsx = "function A() { return <>hi</>; }\nconst b = <></>;";
        assert_eq!(stripFlow(jsx), jsx, "a fragment is not an empty type-argument list");
    }

    #[test]
    fn an_inline_interface_type_is_an_object_type_with_its_bases_intersected() {
        assert!(reads("// @flow\ntype P = interface extends Promise<any> {\n  _debugInfo?: number,\n};\n"));
        assert!(reads("// @flow\nexport type Container =\n  | interface extends Element {_root?: number}\n  | interface extends Document {_root?: number};\n"));
        assert!(reads("// @flow\nfunction useMap<T: interface {}, U>(m: T => U): U { return m; }\n"));
        let out = stripFlow("type P = interface extends A, B<C> { x: number };");
        assert!(out.contains("A&") && out.contains("B<C>") && out.contains("& {"), "{out}");
        // a declaration keeps its word
        let decl = "interface Foo extends Bar { x: number }\n";
        assert_eq!(stripFlow(decl), decl);
        assert_eq!(lines(&stripFlow("type P = interface extends\n  Promise<any> {\n x: 1 };")), lines("type P = interface extends\n  Promise<any> {\n x: 1 };"));
    }

    #[test]
    fn a_mapped_type_may_end_in_a_comma_and_a_spread_may_sit_in_a_parameters_object_type() {
        assert!(reads("// @flow\ntype E<Events: Object> = {\n  [Event in keyof Events]: A<Events[Event]>,\n};\n"));
        assert!(reads("// @flow\ntype X<Props: {...}> = {\n  ...$Exact<Props>,\n  nux: number,\n};\n"), "a generic spread has no member form: the entry goes");
        assert!(reads("// @flow\nexport function f({source}: {\n  ...Position,\n  +source: ?string,\n}): ?string { return source; }\n"));
        assert!(reads("// @flow\nexport function g(init?: {\n  ...Event$Init,\n  message?: string,\n}) {}\n"));
        // an object LITERAL after a colon is still a literal
        let lit = "const o = f(a ? b : {...c, d: 1});\nconst p = { k: {...q} };";
        assert_eq!(stripFlow(lit), lit);
    }

    #[test]
    fn a_generic_function_type_as_a_return_type_names_its_parameters_and_a_multiline_module_exports_is_blanked() {
        assert!(reads("// @flow\nexport function useCacheRefresh(): <T>(?() => T, ?T) => void {\n  return unsupported;\n}\n"));
        let src = "declare module.exports: (\n  argv: Array<string>,\n  opts?: Options\n) => Output;\nconst x = 1;";
        let out = stripFlow(src);
        assert!(!out.contains("module.exports") && !out.contains("Output") && out.contains("const x = 1;"), "{out}");
        assert_eq!(lines(&out), lines(src));
        // an ordinary generic arrow keeps its parameters
        assert!(stripFlow("const id = <T>(x: T): T => x;").contains("(x: T)"));
    }

    #[test]
    fn a_libdef_mixin_list_is_dropped_from_the_class_header() {
        assert!(reads("// @flow\ndeclare class Navigator\n  mixins\n    NavigatorID,\n    NavigatorLanguage,\n  {\n  geolocation: number;\n}\n"));
        let plain = "class A { mixins(x) { return x; } }\nconst o = { mixins: 1 };";
        assert_eq!(stripFlow(plain), plain, "a member or key called mixins is not a header");
    }

    #[test]
    fn a_bare_arrow_or_brace_in_jsx_text_is_read() {
        assert!(reads("// @flow\nexport function A() {\n  return (\n    <div>\n      Extension -> Link Handling.\n    </div>\n  );\n}\n"), "a bare >");
        assert!(reads("// @flow\nexport function B() { return <p>a } b > c</p>; }\n"), "a bare closing brace and >");
        // markup that is fine is untouched, and a real error is still reported
        assert!(reads("export function C() { return <div className=\"a\">{x > 1 ? 'y' : 'z'}</div>; }\n"));
        assert!(!reads("// @flow\nexport function D( { return 1;\n"), "a genuine syntax error stays one");
        let fixed = repair_jsx_text("const a = <i>x -> y</i>;");
        assert_eq!(fixed.len(), "const a = <i>x -> y</i>;".len());
        assert!(!fixed.contains("->"), "{fixed}");
        assert_eq!(repair_jsx_text("const a = 1;"), "const a = 1;");
    }

    #[test]
    fn plain_javascript_comes_back_unchanged() {
        let src = "import a from 'a';\nexport function f(x, y) {\n  return x ? y : [x, y].map((v) => v * 2);\n}\n";
        assert_eq!(stripFlow(src), src);
    }

    // ── hostile input (audit 2026-10-01, F-06) ─────────────────────────────────────────────────────────────────────────────────────────────────────
    // Each runs on a thread with a 2 MB stack (the main test threads have 2 MB too, but a stack overflow ABORTS the whole test binary, so the cap has to be proven where it is small)
    // and must finish in seconds, in a debug build.
    fn hostile(name: &str, src: String) -> (String, bool) {
        let t0 = std::time::Instant::now();
        let name_owned = name.to_string();
        let out = std::thread::Builder::new().stack_size(2 * 1024 * 1024).spawn(move || stripFlowChecked(&src))
            .unwrap().join().unwrap_or_else(|_| panic!("{name_owned}: the pass panicked or overflowed its stack"));
        assert!(t0.elapsed() < std::time::Duration::from_secs(8), "{name}: took {:?}", t0.elapsed());
        out
    }

    #[test]
    fn templates_nested_a_hundred_thousand_deep_are_refused_not_a_stack_overflow() {
        let src = "`${".repeat(100_000);
        let (out, budget) = hostile("100k nested ${", src.clone());
        assert!(budget, "the pass says it gave up");
        assert_eq!(out, src, "and hands the input back unstripped");
        // nesting that real code reaches (and a few hundred levels more) is read
        let ok = format!("{}1{}", "`a${".repeat(40), "}b`".repeat(40));
        let (out, budget) = hostile("40 nested", ok.clone());
        assert!(!budget && out == ok, "{budget}");
        assert_eq!(skip_template(b"`a${`b${`c`}`}d` tail", 0, &mut false), 16, "the iterative skipper ends where the recursive one did");
    }

    #[test]
    fn a_megabyte_of_stray_closers_is_linear() {
        let (out, budget) = hostile("1 MB of )", ")".repeat(1 << 20));
        assert!(!budget); assert_eq!(out.len(), 1 << 20);
        // ...over a deep (but sane) stack of openers, where every closer used to scan the whole stack
        let src = format!("{}{}", "[".repeat(1_900), ")".repeat(1 << 20));
        let (_, budget) = hostile("1900 [ then 1 MB of )", src);
        assert!(!budget);
    }

    #[test]
    fn a_megabyte_of_slashes_is_linear_even_when_every_slash_could_open_a_regex() {
        let (_, budget) = hostile("1 MB of /", "/".repeat(1 << 20));
        assert!(!budget);
        // `/[` opens a character class that never closes on the line: every `/` used to rescan the rest of the line
        let _ = hostile("500k of /[", "/[".repeat(500_000));   // finishing in time is the assertion
        let (_, _) = hostile("/ and ( alternating", "x = (/[/ ".repeat(100_000));
    }

    #[test]
    fn a_file_of_openers_is_refused_before_anything_recurses() {
        let (out, budget) = hostile("200 KB of (", "(".repeat(200 * 1024));
        assert!(budget, "200 000 open brackets is not a source file"); assert_eq!(out.len(), 200 * 1024);
        let (_, budget) = hostile("100k ( then 100k )", format!("{}{}", "(".repeat(100_000), ")".repeat(100_000)));
        assert!(budget);
        // nested groups that stay under the depth cap are cheap now: each `(` looks a bounded window ahead
        let one = format!("{}x{}", "(".repeat(1_500), ")".repeat(1_500));
        let (_, _) = hostile("1500-deep groups x60", one.repeat(60));
    }

    #[test]
    fn scans_that_hunt_for_the_end_of_a_statement_are_windowed() {
        // `declare module.exports: )` never closes at depth 0: the scan ran to the end of the file, from every occurrence
        let (_, _) = hostile("declare module.exports x30k", "declare module.exports: )\n".repeat(30_000));
        // a mapped type with nothing after it
        let (_, _) = hostile("[K in X]: A x60k", "[K in X]: A\n".repeat(60_000));
        // `> = {` chains walked back to the start of the file
        let (_, _) = hostile("> = { x40k", "> = {\n".repeat(40_000));
        // nested exact objects, each re-scanned by its parent
        let (_, _) = hostile("{| x1500", format!("{}{}", "{|".repeat(1_500), "|}".repeat(1_500)));
    }

    #[test]
    fn input_over_the_cap_is_handed_back_unstripped_with_the_flag() {
        let src = "a;".repeat(MAX_INPUT_BYTES / 2 + 1);
        let (out, budget) = stripFlowChecked(&src);
        assert!(budget && out.len() == src.len());
        // and the indexer's own door reports it as a failure rather than an empty clean file
        let e = crate::codebase_parse_js::parseJsFlow(&format!("// @flow\n{}", "`${".repeat(100_000)), "x.js").unwrap_err();
        assert!(e.starts_with("flow:") && e.contains("gave up"), "{e}");
    }
}

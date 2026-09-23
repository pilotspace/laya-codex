//! Reference extraction for `Chunk.refs`: callee names of call sites, used type names and
//! imported names. Rules are per-language node kinds, looked up through the dense per-language
//! kind table, and applied during the same tree walk that collects definitions.

use laya_core::Lang;
use tree_sitter::Node;

/// Maximum number of refs recorded per chunk.
pub const MAX_REFS: usize = 48;

/// Names never recorded as refs: receivers/keywords, builtin types and ubiquitous std/runtime
/// names that would link every chunk to every other. Matched case-sensitively. Tune here.
pub const REF_STOPLIST: &[&str] = &[
    // receivers / keywords / literals
    "self",
    "Self",
    "this",
    "super",
    "cls",
    "crate",
    "true",
    "false",
    "null",
    "nil",
    "None",
    "undefined",
    // constructors, conversions, containers
    "new",
    "default",
    "clone",
    "unwrap",
    "expect",
    "into",
    "from",
    "iter",
    "map",
    "len",
    "push",
    "get",
    "set",
    "insert",
    "remove",
    "to_string",
    "to_owned",
    "as_ref",
    "ok",
    "err",
    "some",
    "none",
    "Some",
    "Ok",
    "Err",
    "Option",
    "Result",
    "Box",
    "Arc",
    "Rc",
    "Vec",
    "String",
    "vec",
    "str",
    "int",
    "bool",
    "range",
    "isinstance",
    "append",
    "string",
    "number",
    "boolean",
    "object",
    "array",
    "promise",
    "Promise",
    "Object",
    "Array",
    "Error",
    "Map",
    "Set",
    "JSON",
    "Math",
    "Integer",
    "Boolean",
    "Double",
    "Long",
    "Float",
    "void",
    "any",
    "error",
    "byte",
    "rune",
    "uint",
    "int64",
    "int32",
    "uint64",
    "uint32",
    "uint8",
    "float64",
    "float32",
    "size_t",
    "char",
    "make",
    "cap",
    "panic",
    // output / logging / formatting / assertions
    "format",
    "println",
    "print",
    "printf",
    "sprintf",
    "fprintf",
    "eprintln",
    "write",
    "writeln",
    "Errorf",
    "Sprintf",
    "Printf",
    "Println",
    "debug",
    "info",
    "warn",
    "trace",
    "log",
    "console",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "todo",
    "unimplemented",
    "unreachable",
    "dbg",
    // runtime / framework ubiquity
    "main",
    "require",
    "toString",
    "equals",
    "hashCode",
    "then",
    "catch",
    "forEach",
    "filter",
    "reduce",
    "keys",
    "values",
    "items",
    "useState",
    "useEffect",
    "useRef",
    "useMemo",
    "useCallback",
    // Rust std methods (Option/Result/iterators/slices/strings)
    "is_empty",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "map_err",
    "and_then",
    "or_else",
    "ok_or",
    "ok_or_else",
    "as_str",
    "as_slice",
    "as_mut",
    "as_bytes",
    "get_mut",
    "iter_mut",
    "into_iter",
    "enumerate",
    "collect",
    "cloned",
    "copied",
    "to_vec",
    "extend",
    "contains",
    "contains_key",
    "with_capacity",
    "chars",
    "bytes",
    "trim",
    "starts_with",
    "ends_with",
    "to_lowercase",
    "to_uppercase",
    "eq_ignore_ascii_case",
    "is_finite",
    "clamp",
    "min",
    "max",
    "abs",
    "sum",
    "count",
    "first",
    "last",
    "next",
    "take",
    "skip",
    "zip",
    "rev",
    "sort",
    "sort_by",
    "sort_unstable",
    "dedup",
    "retain",
    "drain",
    "any",
    "all",
    "find",
    "position",
    "chain",
    "flatten",
    "flat_map",
    "filter_map",
    "fold",
    "borrow",
    "borrow_mut",
    "lock",
    "context",
    "with_context",
    "entry",
    "or_insert",
    "or_default",
    "saturating_sub",
    "wrapping_add",
    "wrapping_mul",
    "checked_add",
    "checked_sub",
    // Python / JS / Go / Java builtins and ubiquitous methods
    "join",
    "split",
    "strip",
    "replace",
    "sorted",
    "list",
    "dict",
    "tuple",
    "slice",
    "splice",
    "concat",
    "includes",
    "indexOf",
    "toLowerCase",
    "toUpperCase",
    "parseInt",
    "stringify",
    "setTimeout",
    "size",
    "add",
    "put",
    "stream",
    "isEmpty",
    "valueOf",
    "length",
    "substring",
    "Lock",
    "Unlock",
    "RLock",
    "RUnlock",
];

/// How a node contributes reference names.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RefRule {
    /// Call site: callee in `field` (empty = first named child); record its last identifier.
    Call(&'static str),
    /// The node itself names a type (`type_identifier`, Ruby `constant`).
    TypeIdent,
    /// A type wrapper (`user_type`, `named_type`): record its last identifier.
    TypeTail,
    /// Python identifier inside a `type` annotation or a class's base list.
    PyTypeIdent,
    /// JS/TS identifier or member expression in an `extends` clause.
    JsHeritage,
    /// JSX element naming a component (capitalized; lowercase names are HTML tags).
    JsxComponent,
    /// C# identifier in a type position.
    CsTypeIdent,
    /// Rust `use` tree leaves.
    RustUse,
    /// Python `import` / `from .. import` names.
    PyImport,
    /// JS/TS `import` clause names.
    JsImport,
    /// Go import path: last path segment.
    GoImport,
    /// Import of a single qualified name: its last identifier.
    TailImport,
}

use RefRule::*;

pub(crate) fn rule_for(lang: Lang, kind: &str) -> Option<RefRule> {
    use Lang::*;
    Some(match (lang, kind) {
        (Text, _) => return None,
        (Rust, "call_expression") => Call("function"),
        (Rust, "macro_invocation") => Call("macro"),
        (Rust, "use_declaration") => RustUse,
        (Python, "call") => Call("function"),
        (Python, "identifier") => PyTypeIdent,
        (Python, "import_statement" | "import_from_statement") => PyImport,
        (TypeScript | Tsx | JavaScript, "call_expression") => Call("function"),
        (TypeScript | Tsx | JavaScript, "new_expression") => Call("constructor"),
        (TypeScript | Tsx | JavaScript, "identifier" | "member_expression") => JsHeritage,
        (Tsx | JavaScript, "jsx_opening_element" | "jsx_self_closing_element") => JsxComponent,
        (TypeScript | Tsx | JavaScript, "import_statement") => JsImport,
        (Go, "call_expression") => Call("function"),
        (Go, "import_spec") => GoImport,
        (Java, "method_invocation") => Call("name"),
        (Java | CSharp, "object_creation_expression") => Call("type"),
        (Java, "import_declaration") => TailImport,
        (C | Cpp, "call_expression") => Call("function"),
        (Cpp, "using_declaration") => TailImport,
        (CSharp, "invocation_expression") => Call("function"),
        (CSharp, "identifier") => CsTypeIdent,
        (CSharp, "using_directive") => TailImport,
        (Ruby, "call") => Call("method"),
        (Ruby, "constant") => TypeIdent,
        (Php, "function_call_expression") => Call("function"),
        (
            Php,
            "member_call_expression" | "scoped_call_expression" | "nullsafe_member_call_expression",
        ) => Call("name"),
        (Php, "object_creation_expression") => Call(""),
        (Php, "named_type") => TypeTail,
        (Php, "namespace_use_clause") => TailImport,
        (Kotlin, "call_expression") => Call(""),
        (Kotlin, "user_type") => TypeTail,
        (Kotlin, "import") => TailImport,
        (Swift, "call_expression") => Call(""),
        (Swift, "import_declaration") => TailImport,
        (_, "type_identifier") => TypeIdent,
        _ => return None,
    })
}

/// A referenced name at a source position.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RefOcc<'s> {
    pub byte: usize,
    pub row: usize,
    pub name: &'s str,
}

/// Context of the visited node, maintained by the tree walk (avoids O(depth) `Node::parent`).
pub(crate) struct Ctx<'a, 't> {
    pub ancestors: &'a [Node<'t>],
    pub field: Option<&'t str>,
}

fn is_leaf_ident(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "field_identifier"
            | "property_identifier"
            | "type_identifier"
            | "simple_identifier"
            | "constant"
            | "name"
            | "private_property_identifier"
            | "package_identifier"
    )
}

fn last_named_child(n: Node<'_>) -> Option<Node<'_>> {
    let count = n.named_child_count();
    n.named_child(u32::try_from(count.checked_sub(1)?).ok()?)
}

/// Last identifier of a (possibly qualified / member / generic) name expression:
/// `a.b.foo` -> foo, `Foo::bar` -> bar, `self.x` -> x, `Vec<T>` -> Vec.
fn tail_ident(node: Node<'_>) -> Option<Node<'_>> {
    let mut n = node;
    for _ in 0..12 {
        if is_leaf_ident(n.kind()) {
            return Some(n);
        }
        n = match n.kind() {
            "generic_type" | "generic_name" | "generic_function" | "template_function"
            | "template_type" => n.named_child(0)?,
            _ => ["name", "field", "property", "attribute", "method", "suffix"]
                .iter()
                .find_map(|f| n.child_by_field_name(f))
                .or_else(|| last_named_child(n))?,
        };
    }
    None
}

fn push<'s>(out: &mut Vec<RefOcc<'s>>, node: Option<Node<'_>>, src: &'s str) {
    if let Some(n) = node
        && let Some(name) = src.get(n.byte_range())
    {
        out.push(RefOcc {
            byte: n.start_byte(),
            row: n.start_position().row,
            name,
        });
    }
}

fn rust_use<'s>(node: Node<'_>, src: &'s str, out: &mut Vec<RefOcc<'s>>, depth: usize) {
    if depth > 16 {
        return;
    }
    match node.kind() {
        "identifier" => push(out, Some(node), src),
        "scoped_identifier" => push(out, node.child_by_field_name("name"), src),
        "use_as_clause" => {
            if let Some(p) = node.child_by_field_name("path") {
                rust_use(p, src, out, depth + 1);
            }
        }
        "scoped_use_list" => {
            if let Some(l) = node.child_by_field_name("list") {
                rust_use(l, src, out, depth + 1);
            }
        }
        "use_list" => {
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                rust_use(child, src, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Append the refs contributed by `node` under `rule`.
pub(crate) fn extract<'s>(
    rule: RefRule,
    node: Node<'_>,
    ctx: &Ctx<'_, '_>,
    src: &'s str,
    out: &mut Vec<RefOcc<'s>>,
) {
    let n_anc = ctx.ancestors.len();
    let parent_kind = ctx.ancestors.last().map_or("", |p| p.kind());
    match rule {
        Call(field) => {
            let callee = if field.is_empty() {
                node.named_child(0)
            } else {
                node.child_by_field_name(field)
            };
            push(out, callee.and_then(tail_ident), src);
        }
        TypeIdent => push(out, Some(node), src),
        TypeTail => {
            let mut c = node.walk();
            let last = node
                .named_children(&mut c)
                .filter(|n| is_leaf_ident(n.kind()))
                .last();
            push(out, last.or_else(|| tail_ident(node)), src);
        }
        PyTypeIdent => {
            let in_type = ctx.ancestors[n_anc.saturating_sub(3)..]
                .iter()
                .any(|a| a.kind() == "type");
            let is_base = parent_kind == "argument_list"
                && n_anc >= 2
                && ctx.ancestors[n_anc - 2].kind() == "class_definition";
            if in_type || is_base {
                push(out, Some(node), src);
            }
        }
        JsHeritage => {
            if matches!(parent_kind, "extends_clause" | "class_heritage") {
                push(out, tail_ident(node), src);
            }
        }
        JsxComponent => {
            let name = node.child_by_field_name("name").and_then(tail_ident);
            let is_component = name
                .and_then(|n| src.get(n.byte_range()))
                .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_uppercase()));
            if is_component {
                push(out, name, src);
            }
        }
        CsTypeIdent => {
            let type_pos = matches!(ctx.field, Some("type" | "returns"))
                || matches!(
                    parent_kind,
                    "base_list"
                        | "type_argument_list"
                        | "nullable_type"
                        | "array_type"
                        | "generic_name"
                        | "type_parameter_constraint"
                );
            if type_pos {
                push(out, Some(node), src);
            }
        }
        RustUse => {
            if let Some(arg) = node.child_by_field_name("argument") {
                rust_use(arg, src, out, 0);
            }
        }
        PyImport => {
            let mut c = node.walk();
            for n in node.children_by_field_name("name", &mut c) {
                let dotted = if n.kind() == "aliased_import" {
                    n.child_by_field_name("name")
                } else {
                    Some(n)
                };
                push(out, dotted.and_then(tail_ident), src);
            }
        }
        JsImport => {
            let mut c = node.walk();
            let Some(clause) = node
                .named_children(&mut c)
                .find(|n| n.kind() == "import_clause")
            else {
                return;
            };
            let mut c2 = clause.walk();
            for part in clause.named_children(&mut c2) {
                match part.kind() {
                    "identifier" => push(out, Some(part), src),
                    "named_imports" => {
                        let mut c3 = part.walk();
                        for spec in part.named_children(&mut c3) {
                            let name = spec
                                .child_by_field_name("name")
                                .filter(|n| n.kind() == "identifier");
                            push(out, name, src);
                        }
                    }
                    _ => {}
                }
            }
        }
        GoImport => {
            if let Some(p) = node.child_by_field_name("path")
                && let Some(raw) = src.get(p.byte_range())
            {
                let path = raw.trim_matches(['"', '`']);
                let last = path.rsplit('/').next().unwrap_or(path);
                let lead = raw.len() - raw.trim_start_matches(['"', '`']).len();
                let byte = p.start_byte() + lead + (path.len() - last.len());
                out.push(RefOcc {
                    byte,
                    row: p.start_position().row,
                    name: last,
                });
            }
        }
        TailImport => {
            let mut c = node.walk();
            if node
                .children(&mut c)
                .any(|n| matches!(n.kind(), "asterisk" | "*" | "wildcard_import"))
            {
                return;
            }
            let mut c = node.walk();
            let target = node
                .named_children(&mut c)
                .filter(|n| {
                    !matches!(
                        n.kind(),
                        "modifiers" | "comment" | "import_alias" | "namespace_aliasing_clause"
                    )
                })
                .last();
            push(out, target.and_then(tail_ident), src);
        }
    }
}

/// Can `name` be recorded as a ref at all (identifier-shaped, length, stoplist)?
pub(crate) fn admissible(name: &str) -> bool {
    name.chars().count() >= 3
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !REF_STOPLIST.contains(&name)
}

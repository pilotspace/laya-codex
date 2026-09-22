//! Per-language definition tables: which node kinds define a named symbol, how to label
//! them in a symbol path (`impl Store for MoonStore`, `class Foo`, `def bar`) and how to
//! extract the defined identifier. Tables are dense `kind_id -> rule` arrays built once per
//! language, so the per-node check on the hot path is an array lookup.

use std::sync::OnceLock;

use laya_core::Lang;
use tree_sitter::{Language, Node};

use crate::lang::{LANG_COUNT, lang_index};

/// How to find the defined name of a node.
#[derive(Debug, Clone, Copy)]
enum NameRule {
    /// `child_by_field_name(field)`, falling back to the first identifier-like child.
    Field(&'static str),
    /// C/C++ declarator chain (`declarator` field until an identifier).
    Declarator,
    /// C/C++ `struct`/`union`/`enum`/`class` specifier: only a definition when it has a body.
    Tagged,
    /// C++ member declaration: only when the declarator is a function declarator.
    MemberFn,
    /// Rust `impl [Trait for] Type`: labelled, defines nothing.
    RustImpl,
    /// JS/TS `variable_declarator`: function/class values anywhere, any value at top level.
    JsDeclarator,
    /// Python module-level `NAME = ...` with an all-caps name.
    PyConst,
    /// Ruby `CONST = ...`.
    RubyConst,
    /// Kotlin top-level `val`/`var`.
    KotlinProperty,
    /// Swift `class`/`struct`/`enum`/`extension`/`actor`: prefix comes from the source.
    SwiftType,
    /// Swift top-level `let`/`var`.
    SwiftProperty,
    /// No name (constructors, anonymous namespaces): the label is the prefix alone.
    Anonymous,
}

#[derive(Debug, Clone, Copy)]
struct DefRule {
    prefix: &'static str,
    name: NameRule,
}

const fn r(prefix: &'static str, name: NameRule) -> Option<DefRule> {
    Some(DefRule { prefix, name })
}

use NameRule::*;

fn rule_for(lang: Lang, kind: &str) -> Option<DefRule> {
    let name = Field("name");
    match lang {
        Lang::Rust => match kind {
            "function_item" | "function_signature_item" => r("fn", name),
            "struct_item" => r("struct", name),
            "enum_item" => r("enum", name),
            "union_item" => r("union", name),
            "trait_item" => r("trait", name),
            "impl_item" => r("impl", RustImpl),
            "mod_item" => r("mod", name),
            "const_item" => r("const", name),
            "static_item" => r("static", name),
            "type_item" => r("type", name),
            "macro_definition" => r("macro_rules!", name),
            _ => None,
        },
        Lang::Python => match kind {
            "function_definition" => r("def", name),
            "class_definition" => r("class", name),
            "assignment" => r("", PyConst),
            _ => None,
        },
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => match kind {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                r("function", name)
            }
            "class_declaration" | "abstract_class_declaration" => r("class", name),
            "method_definition" | "method_signature" | "abstract_method_signature" => r("", name),
            "interface_declaration" => r("interface", name),
            "type_alias_declaration" => r("type", name),
            "enum_declaration" => r("enum", name),
            "internal_module" => r("namespace", name),
            "variable_declarator" => r("", JsDeclarator),
            _ => None,
        },
        Lang::Go => match kind {
            "function_declaration" | "method_declaration" => r("func", name),
            "type_spec" | "type_alias" => r("type", name),
            "const_spec" => r("const", name),
            _ => None,
        },
        Lang::Java => match kind {
            "class_declaration" => r("class", name),
            "interface_declaration" => r("interface", name),
            "enum_declaration" => r("enum", name),
            "record_declaration" => r("record", name),
            "annotation_type_declaration" => r("@interface", name),
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => r("", name),
            _ => None,
        },
        Lang::C | Lang::Cpp => match kind {
            "function_definition" => r("", Declarator),
            "struct_specifier" => r("struct", Tagged),
            "union_specifier" => r("union", Tagged),
            "enum_specifier" => r("enum", Tagged),
            "class_specifier" => r("class", Tagged),
            "type_definition" => r("typedef", Declarator),
            "preproc_def" | "preproc_function_def" => r("#define", name),
            "namespace_definition" => r("namespace", name),
            "alias_declaration" => r("using", name),
            "concept_definition" => r("concept", name),
            "field_declaration" => r("", MemberFn),
            _ => None,
        },
        Lang::CSharp => match kind {
            "class_declaration" => r("class", name),
            "struct_declaration" => r("struct", name),
            "interface_declaration" => r("interface", name),
            "enum_declaration" => r("enum", name),
            "record_declaration" | "record_struct_declaration" => r("record", name),
            "method_declaration"
            | "constructor_declaration"
            | "property_declaration"
            | "local_function_statement" => r("", name),
            "destructor_declaration" => r("~", name),
            "delegate_declaration" => r("delegate", name),
            "namespace_declaration" | "file_scoped_namespace_declaration" => r("namespace", name),
            _ => None,
        },
        Lang::Ruby => match kind {
            "method" | "singleton_method" => r("def", name),
            "class" => r("class", name),
            "module" => r("module", name),
            "assignment" => r("", RubyConst),
            _ => None,
        },
        Lang::Php => match kind {
            "function_definition" | "method_declaration" => r("function", name),
            "class_declaration" => r("class", name),
            "interface_declaration" => r("interface", name),
            "trait_declaration" => r("trait", name),
            "enum_declaration" => r("enum", name),
            "namespace_definition" => r("namespace", name),
            "const_element" => r("const", name),
            _ => None,
        },
        Lang::Kotlin => match kind {
            "class_declaration" => r("class", name),
            "object_declaration" => r("object", name),
            "companion_object" => r("companion object", name),
            "function_declaration" => r("fun", name),
            "type_alias" => r("typealias", Field("type")),
            "secondary_constructor" => r("constructor", Anonymous),
            "property_declaration" => r("val", KotlinProperty),
            _ => None,
        },
        Lang::Swift => match kind {
            "class_declaration" => r("", SwiftType),
            "protocol_declaration" => r("protocol", name),
            "function_declaration" | "protocol_function_declaration" => r("func", name),
            "init_declaration" => r("init", Anonymous),
            "deinit_declaration" => r("deinit", Anonymous),
            "typealias_declaration" => r("typealias", name),
            "property_declaration" => r("let", SwiftProperty),
            _ => None,
        },
        Lang::Text => None,
    }
}

/// Dense per-language lookup tables indexed by `kind_id`.
pub(crate) struct KindTable {
    rules: Vec<Option<DefRule>>,
    trivia: Vec<bool>,
}

impl KindTable {
    fn build(lang: Lang, language: &Language) -> Self {
        let n = language.node_kind_count();
        let mut rules = vec![None; n];
        let mut trivia = vec![false; n];
        for id in 0..n {
            let Ok(id16) = u16::try_from(id) else { break };
            if !language.node_kind_is_named(id16) {
                continue;
            }
            let Some(kind) = language.node_kind_for_id(id16) else {
                continue;
            };
            rules[id] = rule_for(lang, kind);
            trivia[id] = kind.contains("comment")
                || kind == "attribute_item"
                || kind == "inner_attribute_item";
        }
        Self { rules, trivia }
    }

    /// Comments and attributes: attached to the following item when chunking.
    pub(crate) fn is_trivia(&self, node: Node<'_>) -> bool {
        self.trivia
            .get(node.kind_id() as usize)
            .copied()
            .unwrap_or(false)
    }

    fn rule(&self, node: Node<'_>) -> Option<DefRule> {
        self.rules.get(node.kind_id() as usize).copied().flatten()
    }
}

pub(crate) fn kind_table(lang: Lang, language: &Language) -> &'static KindTable {
    static TABLES: [OnceLock<KindTable>; LANG_COUNT] = [const { OnceLock::new() }; LANG_COUNT];
    TABLES[lang_index(lang)].get_or_init(|| KindTable::build(lang, language))
}

/// A definition found in the tree. Rows are 0-based and inclusive.
#[derive(Debug, Clone)]
pub(crate) struct Def {
    pub start: usize,
    pub end: usize,
    pub name_row: usize,
    pub label: String,
    /// Identifier this definition introduces (`None` for impls, constructors, ...).
    pub name: Option<String>,
    pub parent: Option<usize>,
}

/// Inclusive row span of a node; a node ending at column 0 does not own that last row.
pub(crate) fn node_rows(node: Node<'_>) -> (usize, usize) {
    let s = node.start_position().row;
    let end = node.end_position();
    let e = if end.column == 0 && end.row > s {
        end.row - 1
    } else {
        end.row
    };
    (s, e)
}

/// Pre-order list of definitions with parent links (iterative walk: no recursion limit).
pub(crate) fn collect_defs(table: &KindTable, root: Node<'_>, src: &str) -> Vec<Def> {
    let mut defs: Vec<Def> = Vec::new();
    let mut open: Vec<usize> = Vec::new(); // defs on the current root->node path
    let mut pushed: Vec<bool> = Vec::new(); // per cursor depth: did that node open a def
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let mut opened = false;
        if let Some(rule) = table.rule(node)
            && let Some((label, name, name_row)) = describe(rule, node, src)
        {
            let (start, end) = node_rows(node);
            defs.push(Def {
                start,
                end,
                name_row,
                label,
                name,
                parent: open.last().copied(),
            });
            open.push(defs.len() - 1);
            opened = true;
        }
        if cursor.goto_first_child() {
            pushed.push(opened);
            continue;
        }
        if opened {
            open.pop();
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return defs;
            }
            if pushed.pop().unwrap_or(false) {
                open.pop();
            }
        }
    }
}

fn text<'s>(node: Node<'_>, src: &'s str) -> &'s str {
    src.get(node.byte_range()).unwrap_or("")
}

/// Single-line, bounded rendering of a (possibly multi-line) name/type node.
fn compact(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(80));
    for (i, w) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(w);
        if out.len() > 80 {
            let mut cut = 80;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            break;
        }
    }
    out
}

fn is_ident_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "simple_identifier"
            | "constant"
            | "name"
            | "namespace_name"
            | "qualified_identifier"
            | "scope_resolution"
            | "destructor_name"
            | "operator_name"
            | "user_type"
            | "private_property_identifier"
    )
}

fn first_ident_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| is_ident_kind(c.kind()))
}

fn field_or_ident<'t>(node: Node<'t>, field: &str) -> Option<Node<'t>> {
    node.child_by_field_name(field)
        .or_else(|| first_ident_child(node))
}

/// Follow a C/C++ `declarator` chain down to the declared name.
fn declarator_name(node: Node<'_>) -> Option<Node<'_>> {
    let mut cur = node.child_by_field_name("declarator")?;
    for _ in 0..16 {
        if is_ident_kind(cur.kind()) {
            return Some(cur);
        }
        cur = match cur.child_by_field_name("declarator") {
            Some(next) => next,
            None => first_ident_child(cur).or_else(|| cur.named_child(0))?,
        };
    }
    None
}

fn is_root_child(node: Node<'_>) -> bool {
    node.parent().is_some_and(|p| p.parent().is_none())
}

/// `(label, defined name, row of the name)` for a candidate definition node, or `None` when
/// the node does not qualify (e.g. a forward `struct foo;` or a local JS variable).
fn describe(rule: DefRule, node: Node<'_>, src: &str) -> Option<(String, Option<String>, usize)> {
    let named = |n: Node<'_>| {
        let name = compact(text(n, src));
        if name.is_empty() {
            return None;
        }
        let label = if rule.prefix.is_empty() {
            name.clone()
        } else {
            format!("{} {name}", rule.prefix)
        };
        Some((label, Some(name), n.start_position().row))
    };
    match rule.name {
        Field(field) => match field_or_ident(node, field) {
            Some(n) => named(n),
            None => Some((rule.prefix.to_string(), None, node.start_position().row)),
        },
        Anonymous => Some((rule.prefix.to_string(), None, node.start_position().row)),
        Declarator => named(declarator_name(node)?),
        Tagged => {
            node.child_by_field_name("body")?;
            named(node.child_by_field_name("name")?)
        }
        MemberFn => {
            let d = node.child_by_field_name("declarator")?;
            if d.kind() != "function_declarator" {
                return None;
            }
            named(declarator_name(node)?)
        }
        RustImpl => {
            let ty = compact(text(node.child_by_field_name("type")?, src));
            let label = match node.child_by_field_name("trait") {
                Some(t) => format!("impl {} for {ty}", compact(text(t, src))),
                None => format!("impl {ty}"),
            };
            Some((label, None, node.start_position().row))
        }
        JsDeclarator => {
            let n = node.child_by_field_name("name")?;
            if n.kind() != "identifier" {
                return None;
            }
            let is_fn = node.child_by_field_name("value").is_some_and(|v| {
                matches!(
                    v.kind(),
                    "arrow_function"
                        | "function_expression"
                        | "function"
                        | "generator_function"
                        | "class"
                )
            });
            let top = node.parent().is_some_and(|decl| {
                decl.parent().is_some_and(|p| {
                    p.parent().is_none() || (p.kind() == "export_statement" && is_root_child(p))
                })
            });
            if !(is_fn || top) {
                return None;
            }
            named(n)
        }
        PyConst => {
            let stmt = node.parent()?;
            if stmt.kind() != "expression_statement" || !is_root_child(stmt) {
                return None;
            }
            let left = node.child_by_field_name("left")?;
            let name = text(left, src);
            let is_const = left.kind() == "identifier"
                && name.bytes().any(|b| b.is_ascii_uppercase())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
            if !is_const {
                return None;
            }
            named(left)
        }
        RubyConst => {
            let left = node.child_by_field_name("left")?;
            if left.kind() != "constant" {
                return None;
            }
            named(left)
        }
        KotlinProperty => {
            if !is_root_child(node) {
                return None;
            }
            let mut cursor = node.walk();
            let var = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "variable_declaration")?;
            named(first_ident_child(var)?)
        }
        SwiftType => {
            let kind = node
                .child_by_field_name("declaration_kind")
                .map(|k| text(k, src))
                .unwrap_or("class");
            let n = node.child_by_field_name("name")?;
            let name = compact(text(n, src));
            Some((format!("{kind} {name}"), Some(name), n.start_position().row))
        }
        SwiftProperty => {
            if !is_root_child(node) {
                return None;
            }
            let n = node.child_by_field_name("name")?;
            let n = if is_ident_kind(n.kind()) {
                n
            } else {
                first_ident_child(n).unwrap_or(n)
            };
            named(n)
        }
    }
}

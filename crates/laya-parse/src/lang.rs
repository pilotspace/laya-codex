//! Path-based language detection and the statically linked grammar registry.

use laya_core::Lang;
use tree_sitter::Language;

/// Files larger than this are never indexed (generated code, data dumps, bundles).
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Directory names that hold generated, vendored or VCS content. Matched against every
/// directory component of a (repo-relative) path.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".git",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".next",
    ".gradle",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
];

/// Lockfiles and checksum manifests: machine-written, useless for retrieval.
const SKIP_FILES: &[&str] = &[
    "cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "poetry.lock",
    "pipfile.lock",
    "gemfile.lock",
    "composer.lock",
    "go.sum",
    "flake.lock",
    "uv.lock",
    "package.resolved",
];

/// Extension-less file names that are known text/code.
fn lang_for_bare_name(name: &str) -> Option<Lang> {
    match name {
        "rakefile" | "gemfile" | "guardfile" | "podfile" | "vagrantfile" => Some(Lang::Ruby),
        "makefile" | "gnumakefile" | "dockerfile" | "containerfile" | "justfile" | "procfile"
        | "readme" | "license" | "copying" | "changelog" | "authors" | "codeowners"
        | "cmakelists.txt" => Some(Lang::Text),
        _ => None,
    }
}

fn lang_for_ext(ext: &str) -> Option<Lang> {
    Some(match ext {
        "rs" => Lang::Rust,
        "py" | "pyi" | "pyw" => Lang::Python,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
        "go" => Lang::Go,
        "java" => Lang::Java,
        "c" | "h" => Lang::C,
        "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "ipp" | "tpp" | "cu"
        | "cuh" => Lang::Cpp,
        "cs" => Lang::CSharp,
        "rb" | "rake" | "gemspec" | "ru" => Lang::Ruby,
        "php" | "phtml" => Lang::Php,
        "kt" | "kts" => Lang::Kotlin,
        "swift" => Lang::Swift,
        "md" | "markdown" | "mdx" | "txt" | "rst" | "adoc" | "org" | "tex" | "toml" | "yaml"
        | "yml" | "json" | "jsonc" | "json5" | "ini" | "cfg" | "conf" | "properties" | "xml"
        | "html" | "htm" | "css" | "scss" | "sass" | "less" | "sh" | "bash" | "zsh" | "fish"
        | "ps1" | "bat" | "cmd" | "sql" | "proto" | "graphql" | "gql" | "thrift" | "cmake"
        | "mk" | "make" | "gradle" | "dockerfile" | "tf" | "hcl" | "nix" | "lua" | "scala"
        | "sc" | "zig" | "ex" | "exs" | "erl" | "hrl" | "hs" | "ml" | "mli" | "r" | "jl"
        | "dart" | "vue" | "svelte" | "astro" | "pl" | "pm" | "vim" | "el" | "clj" | "cljs"
        | "edn" | "fs" | "fsx" | "csv" | "tsv" | "mod" | "work" | "cabal" | "sbt" | "tmpl"
        | "tpl" | "j2" | "jinja" | "hbs" | "mustache" | "rego" | "cue" | "bzl" | "bazel"
        | "star" | "wgsl" | "glsl" | "hlsl" | "metal" | "v" | "sv" | "vhd" | "asm" | "s"
        | "nim" | "cr" | "d" | "m" | "mm" | "groovy" | "pyx" | "pxd" | "ipynb" => Lang::Text,
        _ => return None,
    })
}

/// Pure, path-only classification (no filesystem access). `path` should be repo-relative so
/// that directory filters apply to the repo, not to wherever the repo lives on disk.
pub fn lang_for_path(path: &str) -> Option<Lang> {
    let norm = path.replace('\\', "/");
    let mut parts = norm.split('/').filter(|p| !p.is_empty() && *p != ".");
    let name = parts.next_back()?.to_ascii_lowercase();
    if parts.any(|dir| SKIP_DIRS.contains(&dir)) {
        return None;
    }
    if SKIP_FILES.contains(&name.as_str()) || name.ends_with(".lock") {
        return None;
    }
    // Secrets never get indexed.
    if name == ".env" || name.starts_with(".env.") || name.ends_with(".env") {
        return None;
    }
    if name.contains(".min.") || name.ends_with(".map") || name.ends_with(".bundle.js") {
        return None;
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            lang_for_ext(ext).or_else(|| lang_for_bare_name(&name))
        }
        _ => lang_for_bare_name(&name),
    }
}

/// Language for `path`, or `None` when the file must be skipped (binary/unknown extension,
/// lockfile, minified bundle, generated/vendored directory, or larger than 1 MiB on disk).
pub fn detect_lang(path: &str) -> Option<Lang> {
    let lang = lang_for_path(path)?;
    match std::fs::metadata(path) {
        Ok(m) if m.len() > MAX_FILE_BYTES => None,
        _ => Some(lang),
    }
}

/// Statically linked tree-sitter grammar for `lang`; `None` for [`Lang::Text`].
pub fn grammar(lang: Lang) -> Option<Language> {
    let f = match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE,
        Lang::Python => tree_sitter_python::LANGUAGE,
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE,
        Lang::Go => tree_sitter_go::LANGUAGE,
        Lang::Java => tree_sitter_java::LANGUAGE,
        Lang::C => tree_sitter_c::LANGUAGE,
        Lang::Cpp => tree_sitter_cpp::LANGUAGE,
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE,
        Lang::Ruby => tree_sitter_ruby::LANGUAGE,
        Lang::Php => tree_sitter_php::LANGUAGE_PHP,
        Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE,
        Lang::Swift => tree_sitter_swift::LANGUAGE,
        Lang::Text => return None,
    };
    Some(f.into())
}

/// Dense index for per-language tables (thread-local parsers, kind tables).
pub(crate) fn lang_index(lang: Lang) -> usize {
    match lang {
        Lang::Rust => 0,
        Lang::Python => 1,
        Lang::TypeScript => 2,
        Lang::Tsx => 3,
        Lang::JavaScript => 4,
        Lang::Go => 5,
        Lang::Java => 6,
        Lang::C => 7,
        Lang::Cpp => 8,
        Lang::CSharp => 9,
        Lang::Ruby => 10,
        Lang::Php => 11,
        Lang::Kotlin => 12,
        Lang::Swift => 13,
        Lang::Text => 14,
    }
}

pub(crate) const LANG_COUNT: usize = 15;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_lang_has_a_loadable_grammar() {
        for lang in [
            Lang::Rust,
            Lang::Python,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::JavaScript,
            Lang::Go,
            Lang::Java,
            Lang::C,
            Lang::Cpp,
            Lang::CSharp,
            Lang::Ruby,
            Lang::Php,
            Lang::Kotlin,
            Lang::Swift,
        ] {
            let g = grammar(lang).expect("grammar");
            let mut p = tree_sitter::Parser::new();
            assert!(p.set_language(&g).is_ok(), "{lang:?} ABI incompatible");
        }
        assert!(grammar(Lang::Text).is_none());
    }

    #[test]
    fn skip_dirs_only_apply_to_directories() {
        assert_eq!(lang_for_path("src/build.rs"), Some(Lang::Rust));
        assert_eq!(lang_for_path("build/x.rs"), None);
        assert_eq!(lang_for_path("./src/lib.rs"), Some(Lang::Rust));
    }
}

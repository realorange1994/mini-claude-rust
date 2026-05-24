use std::collections::HashMap;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref DEFAULT_TYPES: HashMap<&'static str, Vec<&'static str>> = {
        let mut m = HashMap::new();
        m.insert("rust", vec![".rs"]);
        m.insert("go", vec![".go"]);
        m.insert("python", vec![".py", ".pyi"]);
        m.insert("py", vec![".py", ".pyi"]);
        m.insert("javascript", vec![".js", ".jsx", ".mjs", ".cjs"]);
        m.insert("typescript", vec![".ts", ".mts", ".cts", ".tsx"]);
        m.insert("ts", vec![".ts", ".tsx", ".mts", ".cts"]);
        m.insert("tsx", vec![".tsx"]);
        m.insert("jsx", vec![".jsx"]);
        m.insert("java", vec![".java"]);
        m.insert("c", vec![".c", ".h"]);
        m.insert("cpp", vec![".cpp", ".cc", ".cxx", ".c++", ".hh", ".hpp", ".hxx", ".h++", ".inl", ".ipp"]);
        m.insert("cc", vec![".c", ".h", ".cats", ".cproto", ".h.in"]);
        m.insert("csharp", vec![".cs"]);
        m.insert("ruby", vec![".rb", ".erb", ".gemspec", ".rake", ".ru"]);
        m.insert("php", vec![".php", ".php3", ".php4", ".php5", ".phtml"]);
        m.insert("swift", vec![".swift"]);
        m.insert("kotlin", vec![".kt", ".kts", ".ktm"]);
        m.insert("scala", vec![".scala", ".sbt", ".sc"]);
        m.insert("html", vec![".htm", ".html", ".xhtml"]);
        m.insert("css", vec![".css", ".scss", ".sass", ".less"]);
        m.insert("json", vec![".json"]);
        m.insert("yaml", vec![".yaml", ".yml"]);
        m.insert("toml", vec![".toml"]);
        m.insert("xml", vec![".xml"]);
        m.insert("markdown", vec![".md", ".mdown", ".markdown"]);
        m.insert("md", vec![".md", ".mdown", ".markdown"]);
        m.insert("sh", vec![".sh", ".bash", ".zsh", ".fish", ".ksh"]);
        m.insert("bash", vec![".sh", ".bash"]);
        m.insert("zsh", vec![".zsh"]);
        m.insert("fish", vec![".fish"]);
        m.insert("sql", vec![".sql"]);
        m.insert("lua", vec![".lua"]);
        m.insert("perl", vec![".pl", ".pm", ".t", ".pod", ".xs"]);
        m.insert("r", vec![".R", ".r", ".Rmd", ".rmd"]);
        m.insert("dart", vec![".dart"]);
        m.insert("elixir", vec![".ex", ".exs"]);
        m.insert("erlang", vec![".erl", ".hrl", ".escript", ".app.src"]);
        m.insert("haskell", vec![".hs", ".lhs"]);
        m.insert("clojure", vec![".clj", ".cljs", ".cljc", ".cljx", ".edn"]);
        m.insert("nim", vec![".nim", ".nims", ".nimble"]);
        m.insert("zig", vec![".zig"]);
        m.insert("vue", vec![".vue"]);
        m.insert("svelte", vec![".svelte"]);
        m.insert("docker", vec!["Dockerfile", ".dockerignore"]);
        m.insert("make", vec!["Makefile", "GNUMakefile", "makefile", "GNUmakefile"]);
        m.insert("cmake", vec![".cmake", "CMakeLists.txt"]);
        m.insert("jupyter", vec![".ipynb"]);
        m.insert("protobuf", vec![".proto"]);
        m.insert("graphql", vec![".graphql", ".gql"]);
        m.insert("terraform", vec![".tf", ".tfvars", ".hcl"]);
        m.insert("hcl", vec![".hcl", ".tf", ".tfvars"]);
        m.insert("nix", vec![".nix"]);
        m.insert("batch", vec![".bat", ".cmd"]);
        m.insert("ps1", vec![".ps1", ".psm1", ".psd1"]);
        m.insert("asm", vec![".asm", ".s", ".S"]);
        m.insert("fortran", vec![".f", ".for", ".f90", ".f95", ".f03", ".f08", ".f15"]);
        m.insert("objective-c", vec![".h", ".m"]);
        m.insert("objective-cpp", vec![".mm"]);
        m.insert("fsharp", vec![".fs", ".fsi", ".fsx"]);
        m.insert("ocaml", vec![".ml", ".mli", ".mll", ".mly", ".eliom", ".eliomi"]);
        m.insert("solidity", vec![".sol"]);
        m.insert("text", vec![".txt"]);
        m.insert("cfg", vec![".cfg", ".conf"]);
        m.insert("ini", vec![".ini"]);
        m.insert("log", vec![".log"]);
        m.insert("license", vec!["COPYING", "LICENSE", "LICENSE-*"]);
        m.insert("readme", vec!["README*", "README.*"]);
        m.insert("diff", vec![".diff", ".patch"]);
        m.insert("org", vec![".org"]);
        m.insert("rst", vec![".rst"]);
        m.insert("tex", vec![".tex", ".sty", ".cls", ".dtx", ".ins"]);
        m.insert("adoc", vec![".adoc", ".asc", ".asciidoc"]);
        m.insert("vim", vec![".vim"]);
        m.insert("elisp", vec![".el"]);
        m.insert("lisp", vec![".lisp", ".lsp", ".el", ".cl", ".clj", ".cljs", ".scm", ".ss"]);
        m.insert("scheme", vec![".scm", ".ss"]);
        m.insert("tcl", vec![".tcl"]);
        m.insert("verilog", vec![".v", ".vh", ".sv", ".svh"]);
        m.insert("vhdl", vec![".vhd", ".vhdl"]);
        m.insert("matlab", vec![".m"]);
        m
    };
}

/// Returns the file extensions for a given language type name.
pub fn extensions_for_type(type_name: &str) -> Option<&Vec<&'static str>> {
    DEFAULT_TYPES.get(type_name)
}

/// Returns the type names that match a given file extension.
pub fn types_for_extension(ext: &str) -> Vec<&'static str> {
    DEFAULT_TYPES
        .iter()
        .filter(|(_, exts)| exts.iter().any(|e| *e == ext))
        .map(|(name, _)| *name)
        .collect()
}

/// Check if a file path matches any of the given type filters.
pub fn file_matches_type(path: &str, type_filters: &[&str]) -> bool {
    if type_filters.is_empty() {
        return true;
    }
    for type_name in type_filters {
        if let Some(extensions) = extensions_for_type(type_name) {
            for ext in extensions {
                if path.ends_with(ext) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extensions_for_type() {
        let rust_exts = extensions_for_type("rust").unwrap();
        assert!(rust_exts.contains(&".rs"));

        let py_exts = extensions_for_type("python").unwrap();
        assert!(py_exts.contains(&".py"));
        assert!(py_exts.contains(&".pyi"));
    }

    #[test]
    fn test_extensions_for_unknown_type() {
        assert!(extensions_for_type("unknown_language").is_none());
    }

    #[test]
    fn test_types_for_extension() {
        let types = types_for_extension(".rs");
        assert!(types.contains(&"rust"));
    }

    #[test]
    fn test_file_matches_type() {
        assert!(file_matches_type("main.rs", &["rust"]));
        assert!(file_matches_type("app.py", &["python"]));
        assert!(file_matches_type("test.rs", &["rust", "python"]));
        assert!(!file_matches_type("test.rs", &["python"]));
        assert!(file_matches_type("any.txt", &[])); // empty filter matches all
    }
}

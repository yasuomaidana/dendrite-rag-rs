//! Best-effort resolution of import specifiers to indexed files, per language
//! (`docs/specs/graph.md` § Import edges). A specifier that names nothing in the project
//! (the standard library, a third-party package) resolves to no file.

use std::collections::{BTreeMap, BTreeSet};

use crate::lang::Lang;

/// Resolves import specifiers against the set of indexed files.
pub struct Resolver {
    files: BTreeSet<String>,
    /// Directory → its files, for package-level imports (Go, Java wildcards).
    dirs: BTreeMap<String, Vec<String>>,
    /// Go module path → the directory holding its `go.mod` (`""` for the root).
    go_modules: Vec<(String, String)>,
}

impl Resolver {
    /// `go_mods` holds `(path of a go.mod file, its contents)`.
    pub fn new<'a>(
        files: impl IntoIterator<Item = &'a str>,
        go_mods: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let files: BTreeSet<String> = files.into_iter().map(str::to_string).collect();
        let mut dirs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in &files {
            dirs.entry(parent(f).to_string()).or_default().push(f.clone());
        }
        let mut go_modules: Vec<(String, String)> = go_mods
            .into_iter()
            .filter_map(|(path, text)| {
                let module = text
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("module "))?
                    .trim()
                    .trim_matches('"')
                    .to_string();
                Some((module, parent(&path).to_string()))
            })
            .collect();
        // Longest module path first, so nested modules win over their parent.
        go_modules.sort_by_key(|(m, _)| std::cmp::Reverse(m.len()));
        Self {
            files,
            dirs,
            go_modules,
        }
    }

    /// The project files `spec`, imported from `from`, refers to.
    pub fn resolve(&self, from: &str, lang: Lang, spec: &str) -> Vec<String> {
        let mut out = match lang {
            Lang::Go => self.go(spec),
            Lang::Rust => self.rust(from, spec),
            Lang::Python => self.python(from, spec),
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => self.script(from, spec),
            Lang::Java => self.java(spec),
            Lang::C | Lang::Cpp => self.include(from, spec),
            Lang::Markdown | Lang::Text => Vec::new(),
        };
        out.retain(|f| f != from);
        out.sort();
        out.dedup();
        out
    }

    fn first(&self, candidates: impl IntoIterator<Item = String>) -> Vec<String> {
        candidates
            .into_iter()
            .find(|c| self.files.contains(c))
            .into_iter()
            .collect()
    }

    fn dir_files(&self, dir: &str, ext: &str, skip_suffix: &str) -> Vec<String> {
        self.dirs.get(dir).map_or_else(Vec::new, |fs| {
            fs.iter()
                .filter(|f| f.ends_with(ext) && (skip_suffix.is_empty() || !f.ends_with(skip_suffix)))
                .cloned()
                .collect()
        })
    }

    /// Files whose path ends with `/suffix` (or equals it).
    fn by_suffix(&self, suffix: &str) -> Vec<String> {
        let slash = format!("/{suffix}");
        self.files
            .iter()
            .filter(|f| *f == suffix || f.ends_with(&slash))
            .cloned()
            .collect()
    }

    fn go(&self, spec: &str) -> Vec<String> {
        for (module, dir) in &self.go_modules {
            let rest = if spec == module {
                ""
            } else if let Some(r) = spec
                .strip_prefix(module.as_str())
                .and_then(|r| r.strip_prefix('/'))
            {
                r
            } else {
                continue;
            };
            return self.dir_files(&join(dir, rest), ".go", "_test.go");
        }
        Vec::new()
    }

    fn rust(&self, from: &str, spec: &str) -> Vec<String> {
        let module_dir = rust_module_dir(from);
        if let Some(name) = spec.strip_prefix("mod:") {
            return self.first(rust_module_files(&module_dir, name));
        }
        // `a::b::{C, D}` and `a::b::*` resolve through `a::b`.
        let path = spec.split("::{").next().unwrap_or(spec).trim_end_matches("::*");
        let segs: Vec<&str> = path.split("::").collect();
        let (base, rest) = match segs[0] {
            "crate" => (rust_crate_root(from), &segs[1..]),
            "self" => (module_dir, &segs[1..]),
            "super" => {
                // Each `super` climbs one module: its children live one directory up.
                let ups = segs.iter().take_while(|s| **s == "super").count();
                let mut d = module_dir.as_str();
                for _ in 0..ups {
                    d = parent(d);
                }
                (d.to_string(), &segs[ups..])
            }
            _ => return Vec::new(), // an external crate or std
        };
        // Longest module prefix that exists: `a::b::Item` tries `a/b/Item`, `a/b`, `a`.
        for n in (1..=rest.len()).rev() {
            let dir = join(&base, &rest[..n - 1].join("/"));
            let found = self.first(rust_module_files(&dir, rest[n - 1]));
            if !found.is_empty() {
                return found;
            }
        }
        Vec::new()
    }

    fn python(&self, from: &str, spec: &str) -> Vec<String> {
        let dots = spec.chars().take_while(|&c| c == '.').count();
        let module = spec[dots..].replace('.', "/");
        let candidates = |dir: &str| {
            let p = join(dir, &module);
            if module.is_empty() {
                vec![join(dir, "__init__.py")]
            } else {
                vec![format!("{p}.py"), format!("{p}/__init__.py")]
            }
        };
        if dots > 0 {
            let mut dir = parent(from).to_string();
            for _ in 1..dots {
                dir = parent(&dir).to_string();
            }
            return self.first(candidates(&dir));
        }
        for root in ["", "src", "lib"] {
            let found = self.first(candidates(root));
            if !found.is_empty() {
                return found;
            }
        }
        // A package nested under some directory (monorepos): match by suffix.
        let mut out = self.by_suffix(&format!("{module}.py"));
        if out.is_empty() {
            out = self.by_suffix(&format!("{module}/__init__.py"));
        }
        if out.len() == 1 { out } else { Vec::new() }
    }

    fn script(&self, from: &str, spec: &str) -> Vec<String> {
        if !spec.starts_with('.') {
            return Vec::new(); // a package
        }
        let base = normalize(&join(parent(from), spec));
        // TypeScript sources import `./x.js` that compiles from `./x.ts`.
        let stem = ["js", "jsx", "mjs", "cjs"]
            .iter()
            .find_map(|e| base.strip_suffix(&format!(".{e}")))
            .unwrap_or(&base);
        let mut candidates = vec![base.clone()];
        for s in [stem, base.as_str()] {
            for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"] {
                candidates.push(format!("{s}.{ext}"));
            }
        }
        for ext in ["ts", "tsx", "js", "jsx"] {
            candidates.push(format!("{base}/index.{ext}"));
        }
        self.first(candidates)
    }

    fn java(&self, spec: &str) -> Vec<String> {
        let spec = spec.trim_start_matches("static ").trim();
        if let Some(pkg) = spec.strip_suffix(".*") {
            let suffix = pkg.replace('.', "/");
            let dir = self
                .dirs
                .keys()
                .find(|d| *d == &suffix || d.ends_with(&format!("/{suffix}")));
            return dir.map_or_else(Vec::new, |d| self.dir_files(d, ".java", ""));
        }
        // `a.b.C` or a static import `a.b.C.member`.
        let parts: Vec<&str> = spec.split('.').collect();
        for n in (1..=parts.len()).rev() {
            let found = self.by_suffix(&format!("{}.java", parts[..n].join("/")));
            if !found.is_empty() {
                return found;
            }
        }
        Vec::new()
    }

    fn include(&self, from: &str, spec: &str) -> Vec<String> {
        let local = self.first([normalize(&join(parent(from), spec))]);
        if !local.is_empty() {
            return local;
        }
        let found = self.by_suffix(spec);
        if found.len() == 1 { found } else { Vec::new() }
    }
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join(dir: &str, rest: &str) -> String {
    match (dir.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{rest}"),
    }
}

/// Resolves `.` and `..` segments; `..` above the root is dropped.
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// The directory holding a Rust file's child modules: `src/` for `src/lib.rs`, `main.rs`
/// and `mod.rs`, `src/engine/` for `src/engine.rs`.
fn rust_module_dir(from: &str) -> String {
    let dir = parent(from);
    let file = from.rsplit('/').next().unwrap_or(from);
    match file {
        "lib.rs" | "main.rs" | "mod.rs" => dir.to_string(),
        f => join(dir, f.trim_end_matches(".rs")),
    }
}

/// The crate root directory: the nearest ancestor `src` directory, else the file's own.
fn rust_crate_root(from: &str) -> String {
    let mut dir = parent(from);
    loop {
        if dir == "src" || dir.ends_with("/src") {
            return dir.to_string();
        }
        if dir.is_empty() {
            return parent(from).to_string();
        }
        dir = parent(dir);
    }
}

fn rust_module_files(dir: &str, name: &str) -> Vec<String> {
    vec![
        join(dir, &format!("{name}.rs")),
        join(dir, &format!("{name}/mod.rs")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> Resolver {
        let files = [
            "go.mod",
            "pkg/sidecar/sidecar.go",
            "pkg/sidecar/client.go",
            "pkg/sidecar/sidecar_test.go",
            "tools/go.mod",
            "tools/gen/gen.go",
            "src/lib.rs",
            "src/engine.rs",
            "src/graph/mod.rs",
            "src/graph/resolve.rs",
            "app/util.py",
            "app/models/__init__.py",
            "app/main.py",
            "web/src/b.ts",
            "web/src/c/index.ts",
            "web/src/a.ts",
            "java/src/com/acme/util/Strings.java",
            "java/src/com/acme/App.java",
            "native/util.h",
            "native/a.c",
        ];
        Resolver::new(
            files,
            [
                (
                    "go.mod".to_string(),
                    "module example.com/m\n\ngo 1.24\n".to_string(),
                ),
                (
                    "tools/go.mod".to_string(),
                    "module example.com/m/tools\n".to_string(),
                ),
            ],
        )
    }

    #[test]
    fn resolves_imports_per_language() {
        let r = resolver();
        let go = r.resolve("cmd/main.go", Lang::Go, "example.com/m/pkg/sidecar");
        assert_eq!(go, ["pkg/sidecar/client.go", "pkg/sidecar/sidecar.go"]);
        assert_eq!(
            r.resolve("cmd/main.go", Lang::Go, "example.com/m/tools/gen"),
            ["tools/gen/gen.go"]
        );
        assert!(r.resolve("cmd/main.go", Lang::Go, "fmt").is_empty());

        assert_eq!(
            r.resolve("src/lib.rs", Lang::Rust, "mod:engine"),
            ["src/engine.rs"]
        );
        assert_eq!(
            r.resolve("src/lib.rs", Lang::Rust, "mod:graph"),
            ["src/graph/mod.rs"]
        );
        assert_eq!(
            r.resolve(
                "src/graph/resolve.rs",
                Lang::Rust,
                "crate::engine::{Engine, Config}"
            ),
            ["src/engine.rs"]
        );
        assert_eq!(
            r.resolve("src/graph/resolve.rs", Lang::Rust, "super::super::engine::Engine"),
            ["src/engine.rs"]
        );
        assert!(r.resolve("src/lib.rs", Lang::Rust, "std::path::Path").is_empty());

        assert_eq!(r.resolve("app/main.py", Lang::Python, ".util"), ["app/util.py"]);
        assert_eq!(
            r.resolve("app/main.py", Lang::Python, "app.models"),
            ["app/models/__init__.py"]
        );
        assert!(r.resolve("app/main.py", Lang::Python, "os.path").is_empty());

        assert_eq!(
            r.resolve("web/src/a.ts", Lang::TypeScript, "./b.js"),
            ["web/src/b.ts"]
        );
        assert_eq!(
            r.resolve("web/src/a.ts", Lang::TypeScript, "./c"),
            ["web/src/c/index.ts"]
        );
        assert!(r.resolve("web/src/a.ts", Lang::TypeScript, "react").is_empty());

        assert_eq!(
            r.resolve("java/src/com/acme/App.java", Lang::Java, "com.acme.util.Strings"),
            ["java/src/com/acme/util/Strings.java"]
        );
        assert_eq!(
            r.resolve("java/src/com/acme/App.java", Lang::Java, "com.acme.util.*"),
            ["java/src/com/acme/util/Strings.java"]
        );

        assert_eq!(r.resolve("native/a.c", Lang::C, "util.h"), ["native/util.h"]);
        assert!(r.resolve("native/a.c", Lang::C, "stdio.h").is_empty());
    }

    #[test]
    fn normalizes_relative_segments() {
        assert_eq!(normalize("a/b/../c/./d"), "a/c/d");
        assert_eq!(normalize("../x"), "x");
    }
}

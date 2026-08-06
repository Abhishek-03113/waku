use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

const TAB_SCROLL_FADE_WIDTH: f32 = 24.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkingTreeEntry {
    relative_path: String,
    absolute_path: PathBuf,
    name: String,
    is_dir: bool,
    file_icon: Option<&'static str>,
    expanded: bool,
    depth: usize,
}

/// Select from a compact, embedded subset of Material Icon Theme rather than
/// shipping its entire icon catalog. The SVG path is resolved once per entry
/// during the directory scan, not on every row paint.
fn file_icon_for_path(path: &str) -> &'static str {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    file_icon_for_name(name)
}

fn file_icon_for_name(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    let named_icon = if name.starts_with("readme") {
        Some("icons/file-types/readme.svg")
    } else if name.starts_with("license")
        || name.starts_with("licence")
        || name.starts_with("copying")
    {
        Some("icons/file-types/certificate.svg")
    } else if name.starts_with("dockerfile") || name.starts_with("compose.") {
        Some("icons/file-types/docker.svg")
    } else if name == "cmakelists.txt" || name.starts_with("cmake.") {
        Some("icons/file-types/cmake.svg")
    } else if name == "makefile" || name.starts_with("makefile.") || name == "justfile" {
        Some("icons/file-types/makefile.svg")
    } else if matches!(
        name.as_str(),
        "cargo.toml" | "cargo.lock" | "rust-toolchain.toml"
    ) {
        Some("icons/file-types/rust.svg")
    } else if matches!(name.as_str(), "go.mod" | "go.sum" | "go.work") {
        Some("icons/file-types/go.svg")
    } else if name == "pyproject.toml" || name == "pipfile" || name.starts_with("requirements") {
        Some("icons/file-types/python.svg")
    } else if matches!(name.as_str(), "bun.lock" | "bun.lockb" | "bunfig.toml") {
        Some("icons/file-types/bun.svg")
    } else if name.starts_with("pnpm-") || name == ".pnpmfile.cjs" {
        Some("icons/file-types/pnpm.svg")
    } else if name == "yarn.lock" || name.starts_with(".yarnrc") {
        Some("icons/file-types/yarn.svg")
    } else if name == "package.json" {
        Some("icons/file-types/nodejs.svg")
    } else if name == "package-lock.json" {
        Some("icons/file-types/npm.svg")
    } else if name.starts_with("tsconfig.") || name == "tsconfig.json" {
        Some("icons/file-types/typescript.svg")
    } else if name.starts_with("jsconfig.") || name == "jsconfig.json" {
        Some("icons/file-types/javascript.svg")
    } else if name == ".gitignore"
        || name == ".gitattributes"
        || name == ".gitmodules"
        || name == ".gitconfig"
    {
        Some("icons/file-types/git.svg")
    } else if name == ".editorconfig" {
        Some("icons/file-types/editorconfig.svg")
    } else if name.starts_with(".env") {
        Some("icons/file-types/settings.svg")
    } else if name.starts_with(".prettier") || name.starts_with("prettier.config.") {
        Some("icons/file-types/prettier.svg")
    } else if name.starts_with(".eslint") || name.starts_with("eslint.config.") {
        Some("icons/file-types/eslint.svg")
    } else if name.starts_with("biome.json") {
        Some("icons/file-types/biome.svg")
    } else if name.starts_with(".babel") || name.starts_with("babel.config.") {
        Some("icons/file-types/babel.svg")
    } else if name.starts_with(".stylelint") || name.starts_with("stylelint.config.") {
        Some("icons/file-types/stylelint.svg")
    } else if name.starts_with("vite.config.") {
        Some("icons/file-types/vite.svg")
    } else if name.starts_with("vitest.config.") || name.starts_with("vitest.workspace.") {
        Some("icons/file-types/vitest.svg")
    } else if name.starts_with("webpack.") {
        Some("icons/file-types/webpack.svg")
    } else if name.starts_with("rollup.config.") {
        Some("icons/file-types/rollup.svg")
    } else if name.starts_with("next.config.") {
        Some("icons/file-types/next.svg")
    } else if name == "next-env.d.ts" {
        Some("icons/file-types/next.svg")
    } else if name.starts_with("nuxt.config.") || name == ".nuxtrc" {
        Some("icons/file-types/nuxt.svg")
    } else if name.starts_with("astro.config.") {
        Some("icons/file-types/astro.svg")
    } else if name == "angular.json" || name.ends_with(".component.ts") {
        Some("icons/file-types/angular.svg")
    } else if name == "nest-cli.json" {
        Some("icons/file-types/nest.svg")
    } else if name.starts_with("tailwind.config.") {
        Some("icons/file-types/tailwindcss.svg")
    } else if name.starts_with("svelte.config.") {
        Some("icons/file-types/svelte.svg")
    } else if name.starts_with("vue.config.") {
        Some("icons/file-types/vue.svg")
    } else if name == "firebase.json" || name == ".firebaserc" {
        Some("icons/file-types/firebase.svg")
    } else if name == "supabase.toml" {
        Some("icons/file-types/supabase.svg")
    } else if name.starts_with("prisma.config.") {
        Some("icons/file-types/prisma.svg")
    } else if name == "turbo.json" {
        Some("icons/file-types/turborepo.svg")
    } else if name.starts_with("deno.json") || name == "deno.lock" {
        Some("icons/file-types/deno.svg")
    } else if name == ".gitlab-ci.yml" || name == ".gitlab-ci.yaml" {
        Some("icons/file-types/gitlab.svg")
    } else if name == "kustomization.yaml" || name == "kustomization.yml" {
        Some("icons/file-types/kubernetes.svg")
    } else if name == "chart.yaml" || name == "values.yaml" {
        Some("icons/file-types/helm.svg")
    } else if name == "nginx.conf" {
        Some("icons/file-types/nginx.svg")
    } else if name == ".nvmrc" || name == ".node-version" {
        Some("icons/file-types/nodejs.svg")
    } else if name == "build.gradle"
        || name == "settings.gradle"
        || name == "gradlew"
        || name == "gradlew.bat"
    {
        Some("icons/file-types/gradle.svg")
    } else if name.contains(".stories.") || name.contains(".story.") {
        Some("icons/file-types/storybook.svg")
    } else if name == "gemfile" || name == "gemfile.lock" {
        Some("icons/file-types/ruby.svg")
    } else if name == "pom.xml" {
        Some("icons/file-types/java.svg")
    } else {
        None
    };
    if let Some(icon) = named_icon {
        return icon;
    }

    let extension = Path::new(&name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    match extension {
        "rs" => "icons/file-types/rust.svg",
        "js" | "mjs" | "cjs" => "icons/file-types/javascript.svg",
        "ts" | "mts" | "cts" => "icons/file-types/typescript.svg",
        "jsx" | "tsx" => "icons/file-types/react.svg",
        "py" | "pyi" | "pyw" => "icons/file-types/python.svg",
        "go" => "icons/file-types/go.svg",
        "c" | "h" | "m" => "icons/file-types/c.svg",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "mm" => "icons/file-types/cpp.svg",
        "cs" => "icons/file-types/csharp.svg",
        "swift" => "icons/file-types/swift.svg",
        "kt" | "kts" => "icons/file-types/kotlin.svg",
        "java" | "class" => "icons/file-types/java.svg",
        "rb" => "icons/file-types/ruby.svg",
        "php" => "icons/file-types/php.svg",
        "html" | "htm" => "icons/file-types/html.svg",
        "css" | "less" => "icons/file-types/css.svg",
        "scss" | "sass" => "icons/file-types/sass.svg",
        "json" | "jsonc" | "jsonl" => "icons/file-types/json.svg",
        "yaml" | "yml" => "icons/file-types/yaml.svg",
        "toml" | "ini" | "cfg" | "conf" | "config" => "icons/file-types/settings.svg",
        "xml" | "xsl" | "plist" => "icons/file-types/xml.svg",
        "md" | "mdx" | "markdown" => "icons/file-types/markdown.svg",
        "sh" | "bash" | "zsh" | "fish" => "icons/file-types/console.svg",
        "ps1" | "psm1" => "icons/file-types/powershell.svg",
        "sql" | "db" | "sqlite" | "sqlite3" | "csv" | "xls" | "xlsx" => {
            "icons/file-types/database.svg"
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "ico" | "tiff" => {
            "icons/file-types/image.svg"
        }
        "svg" => "icons/file-types/svg.svg",
        "pdf" => "icons/file-types/pdf.svg",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" => "icons/file-types/audio.svg",
        "mp4" | "mov" | "avi" | "webm" | "mkv" => "icons/file-types/video.svg",
        "zip" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "tar" | "jar" => {
            "icons/file-types/zip.svg"
        }
        "wasm" | "wat" => "icons/file-types/webassembly.svg",
        "svelte" => "icons/file-types/svelte.svg",
        "vue" => "icons/file-types/vue.svg",
        "tf" | "tfvars" => "icons/file-types/terraform.svg",
        "graphql" | "gql" => "icons/file-types/graphql.svg",
        "lua" => "icons/file-types/lua.svg",
        "dart" => "icons/file-types/dart.svg",
        "astro" => "icons/file-types/astro.svg",
        "coffee" | "cson" => "icons/file-types/coffee.svg",
        "cr" => "icons/file-types/crystal.svg",
        "ex" | "exs" => "icons/file-types/elixir.svg",
        "elm" => "icons/file-types/elm.svg",
        "erl" | "hrl" => "icons/file-types/erlang.svg",
        "clj" | "cljs" | "cljc" | "edn" => "icons/file-types/clojure.svg",
        "hs" | "lhs" => "icons/file-types/haskell.svg",
        "hx" | "hxml" => "icons/file-types/haxe.svg",
        "jinja" | "jinja2" | "j2" => "icons/file-types/jinja.svg",
        "jl" => "icons/file-types/julia.svg",
        "ml" | "mli" => "icons/file-types/ocaml.svg",
        "pl" | "pm" => "icons/file-types/perl.svg",
        "prisma" => "icons/file-types/prisma.svg",
        "pug" | "jade" => "icons/file-types/pug.svg",
        "scala" | "sbt" | "sc" => "icons/file-types/scala.svg",
        "sol" => "icons/file-types/solidity.svg",
        "tex" | "sty" | "cls" => "icons/file-types/tex.svg",
        "xaml" => "icons/file-types/xaml.svg",
        "zig" => "icons/file-types/zig.svg",
        "nix" => "icons/file-types/nix.svg",
        "proto" => "icons/file-types/proto.svg",
        "diff" | "patch" => "icons/file-types/diff.svg",
        "exe" | "dll" | "so" | "dylib" => "icons/file-types/exe.svg",
        "lock" => "icons/file-types/lock.svg",
        _ => "icons/file-types/file.svg",
    }
}

fn visible_working_tree_entries(
    root: &Path,
    expanded_paths: &HashSet<PathBuf>,
) -> Vec<WorkingTreeEntry> {
    fn visit(
        directory: &Path,
        relative_directory: &Path,
        depth: usize,
        expanded_paths: &HashSet<PathBuf>,
        entries: &mut Vec<WorkingTreeEntry>,
    ) {
        let Ok(read_dir) = std::fs::read_dir(directory) else {
            return;
        };
        let mut children = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" {
                    return None;
                }
                let is_dir = entry.file_type().ok()?.is_dir();
                Some((entry.path(), name, is_dir))
            })
            .collect::<Vec<_>>();
        children.sort_by_key(|(_, name, is_dir)| (!*is_dir, name.to_lowercase()));

        for (absolute_path, name, is_dir) in children {
            let relative_path = relative_directory.join(&name);
            let expanded = is_dir && expanded_paths.contains(&absolute_path);
            let file_icon = (!is_dir).then(|| file_icon_for_name(&name));
            entries.push(WorkingTreeEntry {
                relative_path: relative_path.to_string_lossy().into_owned(),
                absolute_path: absolute_path.clone(),
                name,
                is_dir,
                file_icon,
                expanded,
                depth,
            });
            if expanded {
                visit(
                    &absolute_path,
                    &relative_path,
                    depth + 1,
                    expanded_paths,
                    entries,
                );
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, Path::new(""), 0, expanded_paths, &mut entries);
    entries
}

/// The language name for a file, as understood by [`crate::md::highlight`].
/// Names the lexer does not know simply render unhighlighted.
fn file_highlighter_language(relative_path: &str) -> &'static str {
    let path = Path::new(relative_path);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let normalized_file_name = file_name.to_ascii_lowercase();

    // Lockfiles often have a generic `.lock` suffix (or no useful extension),
    // so resolve their actual serialization format before extension fallback.
    let lockfile_language = match normalized_file_name.as_str() {
        "bun.lock"
        | "composer.lock"
        | "conan.lock"
        | "deno.lock"
        | "flake.lock"
        | "npm-shrinkwrap.json"
        | "package-lock.json"
        | "package.resolved"
        | "packages.lock.json"
        | "pipfile.lock" => Some("json"),
        "cargo.lock" | "pdm.lock" | "poetry.lock" | "uv.lock" => Some("toml"),
        "chart.lock" | "gemfile.lock" | "pnpm-lock.yaml" | "podfile.lock" | "pubspec.lock"
        | "yarn.lock" => Some("yaml"),
        "mix.lock" => Some("elixir"),
        _ => None,
    };
    if let Some(language) = lockfile_language {
        return language;
    }

    if file_name == "Makefile" || file_name.starts_with("Makefile.") {
        return "make";
    }
    if normalized_file_name == "dockerfile" || normalized_file_name.starts_with("dockerfile.") {
        return "dockerfile";
    }

    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("rs") => "rust",
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("py" | "pyi") => "python",
        Some("go") => "go",
        Some("c") => "c",
        Some("h" | "hpp" | "hh" | "hxx" | "cc" | "cpp" | "cxx") => "cpp",
        Some("m" | "mm") => "objc",
        Some("java" | "kt" | "kts") => "java",
        Some("cs") => "csharp",
        Some("scala" | "sc") => "scala",
        Some("rb" | "rake" | "gemspec") => "ruby",
        Some("swift") => "swift",
        Some("json" | "jsonc" | "json5") => "json",
        Some("yaml" | "yml") => "yaml",
        Some("toml") => "toml",
        Some("ini" | "cfg" | "conf") => "ini",
        Some("sh" | "bash" | "zsh" | "fish") => "bash",
        Some("css" | "scss" | "sass" | "less") => "css",
        Some("html" | "htm" | "xml" | "svg" | "vue" | "svelte") => "html",
        Some("sql") => "sql",
        Some("diff" | "patch") => "diff",
        Some("md" | "markdown" | "mdx") => "markdown",
        _ => "text",
    }
}

fn read_right_panel_file(project_path: &Path, relative_path: &str) -> (String, bool) {
    match std::fs::read_to_string(project_path.join(relative_path)) {
        Ok(content) => (content, true),
        Err(error) => (format!("Unable to edit this file: {error}"), false),
    }
}

impl RightPanelSurface {
    fn new_browser() -> Self {
        Self::Browser(Uuid::new_v4())
    }

    fn new_terminal() -> Self {
        Self::Terminal(Uuid::new_v4())
    }

    fn terminal_id(&self) -> Option<Uuid> {
        match self {
            Self::Terminal(id) => Some(*id),
            _ => None,
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Browser(_) => "Browser",
            Self::Terminal(_) => "Terminal",
            Self::Files => "Files",
            Self::Diff => "Diff",
            Self::File(path) => path.rsplit('/').next().unwrap_or(path),
        }
    }

    fn icon_path(&self) -> &'static str {
        match self {
            Self::Browser(_) => "icons/globe.svg",
            Self::Terminal(_) => "icons/terminal.svg",
            Self::Files => "icons/folder.svg",
            Self::Diff => "icons/file-diff.svg",
            Self::File(path) => file_icon_for_path(path),
        }
    }
}

fn right_panel_tab_label<'a>(
    surface: &'a RightPanelSurface,
    files_selected_path: Option<&'a str>,
) -> &'a str {
    match surface {
        RightPanelSurface::Files => files_selected_path
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Files"),
        _ => surface.label(),
    }
}

fn right_panel_tab_icon(
    surface: &RightPanelSurface,
    files_selected_path: Option<&str>,
) -> &'static str {
    match surface {
        RightPanelSurface::Files => files_selected_path
            .map(file_icon_for_path)
            .unwrap_or_else(|| surface.icon_path()),
        _ => surface.icon_path(),
    }
}

fn reusable_surface_index(
    surfaces: &[RightPanelSurface],
    requested: &RightPanelSurface,
) -> Option<usize> {
    match requested {
        RightPanelSurface::Browser(_) | RightPanelSurface::Terminal(_) => None,
        RightPanelSurface::Files | RightPanelSurface::Diff | RightPanelSurface::File(_) => {
            surfaces.iter().position(|surface| surface == requested)
        }
    }
}

#[derive(Clone, Copy)]
enum TabScrollFadeSide {
    Left,
    Right,
}

fn tab_scroll_fade_visibility(offset_x: Pixels, max_offset: Pixels) -> (bool, bool) {
    let scrolled = -offset_x;
    let threshold = px(0.5);
    (scrolled > threshold, max_offset - scrolled > threshold)
}

fn fade_safe_tab_offset(
    current_offset: Pixels,
    max_offset: Pixels,
    item_left: Pixels,
    item_right: Pixels,
    viewport_left: Pixels,
    viewport_right: Pixels,
) -> Pixels {
    let inset = px(TAB_SCROLL_FADE_WIDTH);
    let mut offset = current_offset;
    let visible_left = item_left + offset;
    let visible_right = item_right + offset;
    if visible_left < viewport_left + inset {
        offset += viewport_left + inset - visible_left;
    } else if visible_right > viewport_right - inset {
        offset -= visible_right - (viewport_right - inset);
    }
    offset.clamp(-max_offset, px(0.0))
}

fn tab_scroll_reveal_guard(
    scroll_handle: ScrollHandle,
    tab_index: usize,
    waku: WeakEntity<Waku>,
) -> impl IntoElement {
    canvas(
        move |_, window, _| {
            if let Some(item) = scroll_handle.bounds_for_item(tab_index) {
                let viewport = scroll_handle.bounds();
                let offset = scroll_handle.offset();
                let safe_offset = fade_safe_tab_offset(
                    offset.x,
                    scroll_handle.max_offset().x,
                    item.left(),
                    item.right(),
                    viewport.left(),
                    viewport.right(),
                );
                if safe_offset != offset.x {
                    scroll_handle.set_offset(point(safe_offset, offset.y));
                }
            }

            window.on_next_frame(move |_, cx| {
                let _ = waku.update(cx, |this, cx| {
                    if this.right_panel_pending_tab_reveal == Some(tab_index) {
                        this.right_panel_pending_tab_reveal = None;
                        cx.notify();
                    }
                });
            });
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

fn tab_scroll_fade(
    scroll_handle: ScrollHandle,
    side: TabScrollFadeSide,
    surface: Hsla,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| {
            let (show_left, show_right) =
                tab_scroll_fade_visibility(scroll_handle.offset().x, scroll_handle.max_offset().x);
            let visible = match side {
                TabScrollFadeSide::Left => show_left,
                TabScrollFadeSide::Right => show_right,
            };
            visible.then(|| {
                let transparent = surface.opacity(0.0);
                let background = match side {
                    TabScrollFadeSide::Left => linear_gradient(
                        90.0,
                        linear_color_stop(surface, 0.0),
                        linear_color_stop(transparent, 1.0),
                    ),
                    TabScrollFadeSide::Right => linear_gradient(
                        90.0,
                        linear_color_stop(transparent, 0.0),
                        linear_color_stop(surface, 1.0),
                    ),
                };
                fill(bounds, background)
            })
        },
        |_, fade, window, _| {
            if let Some(fade) = fade {
                window.paint_quad(fade);
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .when(matches!(side, TabScrollFadeSide::Left), |element| {
        element.left_0()
    })
    .when(matches!(side, TabScrollFadeSide::Right), |element| {
        element.right_0()
    })
    .w(px(TAB_SCROLL_FADE_WIDTH))
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod tests {
    use super::*;

    /// The render path must never reach the filesystem. This reads the source
    /// rather than the behaviour, because the cost of a regression here is a
    /// syscall per directory entry on every frame — invisible until a project
    /// is large or its volume is slow.
    #[test]
    fn the_working_tree_render_path_does_no_filesystem_work() {
        let source = include_str!("right_panel.rs");
        // Anchored on the definition's indentation so this test does not match
        // its own string literals.
        let start = source
            .find("\n    fn render_right_panel_working_tree(")
            .expect("render fn");
        let body = &source[start + 1..];
        let end = body.find("\n    fn ").unwrap_or(body.len());
        let body = &body[..end];

        for forbidden in [
            "visible_working_tree_entries",
            "read_dir",
            "std::fs::",
            "metadata(",
        ] {
            assert!(
                !body.contains(forbidden),
                "render_right_panel_working_tree must not call `{forbidden}`; \
                 walk the tree in refresh_right_panel_working_tree instead"
            );
        }
    }

    #[test]
    fn working_tree_only_descends_into_expanded_directories() {
        let root = std::env::temp_dir().join(format!("waku-working-tree-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "# Waku\n").unwrap();

        let collapsed = visible_working_tree_entries(&root, &HashSet::new());
        assert_eq!(
            collapsed
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["src", "README.md"]
        );

        let expanded = HashSet::from([root.join("src")]);
        let visible = visible_working_tree_entries(&root, &expanded);
        assert_eq!(
            visible
                .iter()
                .map(|entry| (entry.relative_path.as_str(), entry.depth))
                .collect::<Vec<_>>(),
            vec![
                ("src", 0),
                ("src/nested", 1),
                ("src/main.rs", 1),
                ("README.md", 0)
            ]
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_highlighter_language_follows_file_name_and_extension() {
        assert_eq!(file_highlighter_language("src/app.rs"), "rust");
        assert_eq!(file_highlighter_language("ui/panel.tsx"), "tsx");
        assert_eq!(file_highlighter_language("Sources/App.swift"), "swift");
        assert_eq!(file_highlighter_language("Makefile"), "make");
        assert_eq!(file_highlighter_language("src/native.hpp"), "cpp");
        assert_eq!(file_highlighter_language("LICENSE"), "text");

        for (path, expected_language) in [
            ("bun.lock", "json"),
            ("package-lock.json", "json"),
            ("deno.lock", "json"),
            ("composer.lock", "json"),
            ("Pipfile.lock", "json"),
            ("Package.resolved", "json"),
            ("Cargo.lock", "toml"),
            ("uv.lock", "toml"),
            ("poetry.lock", "toml"),
            ("pnpm-lock.yaml", "yaml"),
            ("yarn.lock", "yaml"),
            ("Podfile.lock", "yaml"),
            ("Gemfile.lock", "yaml"),
            ("mix.lock", "elixir"),
        ] {
            assert_eq!(file_highlighter_language(path), expected_language, "{path}");
        }
    }

    /// The editor colours code with the in-house lexer, so what matters is that
    /// the names `file_highlighter_language` produces are ones the lexer knows.
    /// The few it does not are listed here deliberately: they render as plain
    /// monospace rather than silently looking broken.
    #[test]
    fn mapped_languages_resolve_in_the_in_house_lexer() {
        use crate::md::highlight::{Lang, lang_for_tag};

        for (language, expected) in [
            ("rust", Some(Lang::Rust)),
            ("tsx", Some(Lang::Script)),
            ("swift", Some(Lang::Swift)),
            ("json", Some(Lang::Json)),
            ("toml", Some(Lang::Toml)),
            ("yaml", Some(Lang::Yaml)),
            ("make", Some(Lang::Shell)),
            ("cpp", Some(Lang::C)),
            // Not yet lexed; these fall back to unhighlighted monospace.
            ("elixir", None),
            ("text", None),
        ] {
            assert_eq!(lang_for_tag(language), expected, "{language}");
        }
    }

    #[test]
    fn the_editor_lexer_colours_code_it_recognises() {
        use crate::md::highlight::{Carry, Lang, TokenClass, tokenize_line};

        let line = r#"export function Card({ title }: { title: string }) {"#;
        let spans = tokenize_line(Lang::Script, line, Carry::None)
            .0
            .into_iter()
            .map(|token| (&line[token.range], token.class))
            .collect::<Vec<_>>();

        assert!(spans.contains(&("export", TokenClass::Keyword)));
        assert!(spans.contains(&("function", TokenClass::Keyword)));
        assert!(spans.contains(&("Card", TokenClass::Function)));
    }

    #[test]
    fn working_tree_file_icons_follow_names_and_extensions() {
        assert_eq!(file_icon_for_name("main.rs"), "icons/file-types/rust.svg");
        assert_eq!(
            file_icon_for_name("Panel.tsx"),
            "icons/file-types/react.svg"
        );
        assert_eq!(
            file_icon_for_name("README.md"),
            "icons/file-types/readme.svg"
        );
        assert_eq!(
            file_icon_for_name("Dockerfile.dev"),
            "icons/file-types/docker.svg"
        );
        assert_eq!(file_icon_for_name("bun.lock"), "icons/file-types/bun.svg");
        assert_eq!(
            file_icon_for_name("pnpm-lock.yaml"),
            "icons/file-types/pnpm.svg"
        );
        assert_eq!(
            file_icon_for_name("vite.config.ts"),
            "icons/file-types/vite.svg"
        );
        assert_eq!(
            file_icon_for_name("unknown.data"),
            "icons/file-types/file.svg"
        );
    }

    #[test]
    fn files_tab_uses_the_selected_file_name_and_icon() {
        let files = RightPanelSurface::Files;
        assert_eq!(right_panel_tab_label(&files, None), "Files");
        assert_eq!(
            right_panel_tab_label(&files, Some("packages/desktop/bun.lock")),
            "bun.lock"
        );
        assert_eq!(
            right_panel_tab_icon(&files, Some("packages/desktop/bun.lock")),
            "icons/file-types/bun.svg"
        );

        let file = RightPanelSurface::File("src/main.rs".into());
        assert_eq!(right_panel_tab_label(&file, None), "main.rs");
        assert_eq!(
            right_panel_tab_icon(&file, None),
            "icons/file-types/rust.svg"
        );
    }

    #[test]
    fn editable_file_reader_keeps_the_complete_disk_content() {
        let root = std::env::temp_dir().join(format!("waku-editor-file-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let content = "line\n".repeat(10_000);
        std::fs::write(root.join("large.txt"), &content).unwrap();

        assert_eq!(read_right_panel_file(&root, "large.txt"), (content, true));
        assert!(!read_right_panel_file(&root, "missing.txt").1);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_reuses_single_instance_surface_tabs() {
        let browser = RightPanelSurface::new_browser();
        let terminal = RightPanelSurface::new_terminal();
        let surfaces = vec![
            browser,
            terminal,
            RightPanelSurface::Files,
            RightPanelSurface::Diff,
        ];

        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::new_browser()),
            None
        );
        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::new_terminal()),
            None
        );
        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::Files),
            Some(2)
        );
        assert_eq!(
            reusable_surface_index(&surfaces, &RightPanelSurface::Diff),
            Some(3)
        );
    }

    #[test]
    fn right_panel_state_isolated_by_session() {
        let session_with_terminal = Uuid::new_v4();
        let other_session = Uuid::new_v4();
        let terminal_id = Uuid::new_v4();
        let mut states = HashMap::new();
        let mut terminal_state = RightPanelSessionState::empty(true);
        terminal_state.surfaces = vec![RightPanelSurface::Terminal(terminal_id)];
        terminal_state.active_surface = Some(0);
        terminal_state.file_tree_width = 248.0;
        states.insert(session_with_terminal, terminal_state);

        let other_state = RightPanelSessionState::take_or_closed(&mut states, other_session);
        assert!(!other_state.visible);
        assert!(other_state.surfaces.is_empty());
        assert_eq!(other_state.active_surface, None);
        assert_eq!(other_state.file_tree_width, DEFAULT_FILE_TREE_WIDTH);

        let restored = RightPanelSessionState::take_or_closed(&mut states, session_with_terminal);
        assert!(restored.visible);
        assert_eq!(
            restored.surfaces,
            vec![RightPanelSurface::Terminal(terminal_id)]
        );
        assert_eq!(restored.active_surface, Some(0));
        assert_eq!(restored.file_tree_width, 248.0);
    }

    #[test]
    fn tab_scroll_fades_only_show_toward_hidden_content() {
        assert_eq!(
            tab_scroll_fade_visibility(px(0.0), px(120.0)),
            (false, true)
        );
        assert_eq!(
            tab_scroll_fade_visibility(px(-40.0), px(120.0)),
            (true, true)
        );
        assert_eq!(
            tab_scroll_fade_visibility(px(-120.0), px(120.0)),
            (true, false)
        );
        assert_eq!(tab_scroll_fade_visibility(px(0.0), px(0.0)), (false, false));
    }

    #[test]
    fn selected_tab_offset_clears_fade_overlays() {
        assert_eq!(
            fade_safe_tab_offset(
                px(-100.0),
                px(300.0),
                px(90.0),
                px(190.0),
                px(0.0),
                px(300.0),
            ),
            px(-66.0)
        );
        assert_eq!(
            fade_safe_tab_offset(
                px(-100.0),
                px(324.0),
                px(300.0),
                px(400.0),
                px(0.0),
                px(300.0),
            ),
            px(-124.0)
        );
        assert_eq!(
            fade_safe_tab_offset(px(0.0), px(0.0), px(0.0), px(100.0), px(0.0), px(300.0),),
            px(0.0)
        );
    }
}

impl Waku {
    pub(super) fn store_selected_right_panel_state(&mut self) {
        let Some(session_id) = self.state.selected_session else {
            return;
        };
        let state = self.take_active_right_panel_state();
        self.right_panel_session_states.insert(session_id, state);
    }

    pub(super) fn restore_right_panel_state(&mut self, session_id: Uuid, cx: &mut Context<Self>) {
        let state = RightPanelSessionState::take_or_closed(
            &mut self.right_panel_session_states,
            session_id,
        );
        self.replace_active_right_panel_state(state);
        self.state.right_panel_visible = self.right_panel_visible;
        if self.active_right_panel_surface() == Some(&RightPanelSurface::Diff) {
            self.refresh_right_panel_diff(cx);
        }
        if matches!(
            self.active_right_panel_surface(),
            Some(RightPanelSurface::Files | RightPanelSurface::File(_))
        ) {
            self.refresh_right_panel_working_tree(cx);
        }
        self.ensure_right_panel_terminals(cx);
        if self.right_panel_visible {
            self.request_active_terminal_focus();
        }
    }

    pub(super) fn remove_right_panel_session_state(&mut self, session_id: Uuid) {
        let state = if self.state.selected_session == Some(session_id) {
            let state = self.take_active_right_panel_state();
            self.replace_active_right_panel_state(RightPanelSessionState::empty(false));
            Some(state)
        } else {
            self.right_panel_session_states.remove(&session_id)
        };
        if let Some(state) = state {
            for terminal_id in state
                .surfaces
                .iter()
                .filter_map(RightPanelSurface::terminal_id)
            {
                self.right_panel_terminals.remove(&terminal_id);
            }
        }
    }

    fn take_active_right_panel_state(&mut self) -> RightPanelSessionState {
        RightPanelSessionState {
            visible: self.right_panel_visible,
            surfaces: std::mem::take(&mut self.right_panel_surfaces),
            active_surface: self.right_panel_active_surface.take(),
            tabs_scroll_handle: std::mem::replace(
                &mut self.right_panel_tabs_scroll_handle,
                ScrollHandle::new(),
            ),
            pending_tab_reveal: self.right_panel_pending_tab_reveal.take(),
            expanded_paths: std::mem::take(&mut self.right_panel_expanded_paths),
            files_selected_path: self.right_panel_files_selected_path.take(),
            file_tree_width: self.right_panel_file_tree_width,
            file_editors: std::mem::take(&mut self.right_panel_file_editors),
            diff_files: std::mem::take(&mut self.right_panel_diff_files),
        }
    }

    fn replace_active_right_panel_state(&mut self, state: RightPanelSessionState) {
        self.right_panel_visible = state.visible;
        self.right_panel_surfaces = state.surfaces;
        self.right_panel_active_surface = state.active_surface;
        self.right_panel_tabs_scroll_handle = state.tabs_scroll_handle;
        self.right_panel_pending_tab_reveal = state.pending_tab_reveal;
        self.right_panel_expanded_paths = state.expanded_paths;
        self.right_panel_files_selected_path = state.files_selected_path;
        self.right_panel_file_tree_width = state.file_tree_width;
        self.right_panel_file_editors = state.file_editors;
        self.right_panel_diff_files = state.diff_files;
    }

    fn reveal_right_panel_tab(&mut self, index: usize) {
        self.right_panel_pending_tab_reveal = Some(index);
        self.right_panel_tabs_scroll_handle.scroll_to_item(index);
    }

    fn active_right_panel_surface(&self) -> Option<&RightPanelSurface> {
        self.right_panel_active_surface
            .and_then(|index| self.right_panel_surfaces.get(index))
    }

    pub(super) fn request_active_terminal_focus(&mut self) {
        self.right_panel_pending_terminal_focus = self
            .active_right_panel_surface()
            .and_then(RightPanelSurface::terminal_id);
    }

    fn right_panel_file_is_dirty(&self, relative_path: &str) -> bool {
        self.right_panel_file_editors
            .get(relative_path)
            .is_some_and(|editor| editor.dirty)
    }

    fn right_panel_surface_is_dirty(&self, surface: &RightPanelSurface) -> bool {
        match surface {
            RightPanelSurface::Files => self
                .right_panel_files_selected_path
                .as_deref()
                .is_some_and(|path| self.right_panel_file_is_dirty(path)),
            RightPanelSurface::File(path) => self.right_panel_file_is_dirty(path),
            _ => false,
        }
    }

    fn ensure_initial_right_panel_file_editor_width(&mut self) {
        if self.right_panel_file_editors.is_empty() {
            self.right_panel_width = widened_panel_width_for_file_editor(
                self.right_panel_width,
                self.right_panel_file_tree_width,
            );
        }
    }

    fn open_right_panel_surface(&mut self, surface: RightPanelSurface, cx: &mut Context<Self>) {
        if matches!(&surface, RightPanelSurface::File(_)) {
            self.ensure_initial_right_panel_file_editor_width();
        }
        if surface == RightPanelSurface::Diff {
            self.refresh_right_panel_diff(cx);
        }
        if matches!(surface, RightPanelSurface::Files | RightPanelSurface::File(_)) {
            self.refresh_right_panel_working_tree(cx);
        }
        if let Some(terminal_id) = surface.terminal_id() {
            self.ensure_right_panel_terminal(terminal_id, cx);
        }
        let index =
            reusable_surface_index(&self.right_panel_surfaces, &surface).unwrap_or_else(|| {
                self.right_panel_surfaces.push(surface);
                self.right_panel_surfaces.len() - 1
            });
        self.right_panel_active_surface = Some(index);
        self.reveal_right_panel_tab(index);
        self.request_active_terminal_focus();
        self.set_right_panel_visible(true, cx);
        cx.notify();
    }

    fn open_right_panel_file(&mut self, relative_path: String, cx: &mut Context<Self>) {
        self.ensure_initial_right_panel_file_editor_width();
        let Some(active) = self.right_panel_active_surface else {
            self.open_right_panel_surface(RightPanelSurface::File(relative_path), cx);
            return;
        };
        match self.right_panel_surfaces.get(active).cloned() {
            Some(RightPanelSurface::Files) => {
                let dirty_file_would_be_replaced = self
                    .right_panel_files_selected_path
                    .as_deref()
                    .is_some_and(|current_path| {
                        current_path != relative_path
                            && self.right_panel_file_is_dirty(current_path)
                    });
                if dirty_file_would_be_replaced {
                    self.open_right_panel_surface(RightPanelSurface::File(relative_path), cx);
                    return;
                }

                self.right_panel_files_selected_path = Some(relative_path);
                self.set_right_panel_visible(true, cx);
                cx.notify();
            }
            Some(RightPanelSurface::File(current_path)) => {
                if current_path == relative_path {
                    return;
                }
                if self.right_panel_file_is_dirty(&current_path) {
                    self.open_right_panel_surface(RightPanelSurface::File(relative_path), cx);
                    return;
                }

                let requested = RightPanelSurface::File(relative_path);
                if let Some(existing) =
                    reusable_surface_index(&self.right_panel_surfaces, &requested)
                {
                    self.right_panel_surfaces.remove(active);
                    let existing = if existing > active {
                        existing - 1
                    } else {
                        existing
                    };
                    self.right_panel_active_surface = Some(existing);
                    self.reveal_right_panel_tab(existing);
                } else {
                    self.right_panel_surfaces[active] = requested;
                    self.reveal_right_panel_tab(active);
                }
                self.set_right_panel_visible(true, cx);
                cx.notify();
            }
            _ => self.open_right_panel_surface(RightPanelSurface::File(relative_path), cx),
        }
    }

    fn close_right_panel_surface(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.right_panel_surfaces.len() {
            return;
        }
        if let Some(terminal_id) = self.right_panel_surfaces[index].terminal_id() {
            self.right_panel_terminals.remove(&terminal_id);
        }
        self.right_panel_surfaces.remove(index);
        self.right_panel_active_surface = if self.right_panel_surfaces.is_empty() {
            None
        } else {
            Some(match self.right_panel_active_surface {
                Some(active) if active > index => active - 1,
                Some(active) if active == index => index.saturating_sub(1),
                Some(active) => active.min(self.right_panel_surfaces.len() - 1),
                None => 0,
            })
        };
        if let Some(active) = self.right_panel_active_surface {
            self.reveal_right_panel_tab(active);
            self.request_active_terminal_focus();
        } else {
            self.right_panel_pending_tab_reveal = None;
            self.right_panel_pending_terminal_focus = None;
        }
        cx.notify();
    }

    pub(super) fn close_window_or_right_panel_tab_action(
        &mut self,
        _: &CloseWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(active) = self.right_panel_active_surface {
            self.close_right_panel_surface(active, cx);
            if self.right_panel_surfaces.is_empty() {
                let focus_handle = self.composer_focus(cx);
                window.focus(&focus_handle, cx);
            }
        } else {
            crate::platform::hide_window(window);
        }
    }

    pub(super) fn render_right_panel_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id("toggle-right-panel")
            .w(px(26.0))
            .h(px(26.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon("icons/panel-right.svg", 14.0, theme.text_tertiary))
            .tooltip(|window, cx| Tooltip::new("Toggle right panel").build(window, cx))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.set_right_panel_visible(!this.right_panel_visible, cx);
            }))
    }

    pub(super) fn render_right_panel(
        &mut self,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let active_terminal_id = self
            .active_right_panel_surface()
            .and_then(RightPanelSurface::terminal_id);
        if self.right_panel_pending_terminal_focus == active_terminal_id
            && let Some(terminal_id) = active_terminal_id
            && let Some(terminal) = self.right_panel_terminals.get(&terminal_id)
        {
            let focus_handle = terminal.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
            self.right_panel_pending_terminal_focus = None;
        }
        let body = match self.active_right_panel_surface().cloned() {
            None => self.render_right_panel_chooser(cx).into_any_element(),
            Some(RightPanelSurface::Files) => self
                .render_right_panel_files(width, window, cx)
                .into_any_element(),
            Some(RightPanelSurface::Diff) => self.render_right_panel_diff(cx).into_any_element(),
            Some(RightPanelSurface::Terminal(terminal_id)) => self
                .right_panel_terminals
                .get(&terminal_id)
                .cloned()
                .inspect(|terminal| {
                    terminal.update(cx, |terminal, _| terminal.set_panel_width(width));
                })
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| {
                    self.render_right_panel_empty_message(
                        "Terminal unavailable",
                        "Open the Terminal surface again to start a shell.",
                        cx,
                    )
                    .into_any_element()
                }),
            Some(RightPanelSurface::File(path)) => self
                .render_right_panel_file(path, width, window, cx)
                .into_any_element(),
            Some(surface) => self
                .render_right_panel_placeholder(surface, cx)
                .into_any_element(),
        };

        div()
            .id("right-panel")
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .min_w_0()
            .border_l_1()
            .border_color(theme.border_strong)
            .bg(theme.surface)
            .relative()
            .child(self.render_right_panel_header(cx))
            .child(body)
            .child(self.render_panel_resize_handle(
                "right-panel-resize-handle",
                PanelResizeTarget::RightPanel,
                cx,
            ))
    }

    fn ensure_right_panel_terminal(&mut self, terminal_id: Uuid, cx: &mut Context<Self>) {
        let Some(working_directory) = self.selected_project().map(|project| project.path.clone())
        else {
            self.right_panel_terminals.remove(&terminal_id);
            return;
        };
        let matches_project = self
            .right_panel_terminals
            .get(&terminal_id)
            .is_some_and(|terminal| terminal.read(cx).working_directory() == working_directory);
        if !matches_project {
            self.right_panel_terminals.insert(
                terminal_id,
                cx.new(|cx| TerminalView::new(working_directory.clone(), cx)),
            );
        }
    }

    pub(super) fn ensure_right_panel_terminals(&mut self, cx: &mut Context<Self>) {
        let active_terminal_ids = self
            .right_panel_surfaces
            .iter()
            .filter_map(RightPanelSurface::terminal_id)
            .collect::<Vec<_>>();
        let retained_terminal_ids = active_terminal_ids
            .iter()
            .copied()
            .chain(self.right_panel_session_states.values().flat_map(|state| {
                state
                    .surfaces
                    .iter()
                    .filter_map(RightPanelSurface::terminal_id)
            }))
            .collect::<HashSet<_>>();
        self.right_panel_terminals
            .retain(|terminal_id, _| retained_terminal_ids.contains(terminal_id));
        for terminal_id in active_terminal_ids {
            self.ensure_right_panel_terminal(terminal_id, cx);
        }
    }

    fn render_right_panel_header(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let active_surface = self.right_panel_active_surface;
        let mut tabs = div()
            .id("right-panel-tabs")
            .h_full()
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .gap(px(4.0))
            .overflow_x_scroll()
            .track_scroll(&self.right_panel_tabs_scroll_handle);
        for (index, surface) in self.right_panel_surfaces.iter().cloned().enumerate() {
            let active = active_surface == Some(index);
            let dirty = self.right_panel_surface_is_dirty(&surface);
            let label = SharedString::from(
                right_panel_tab_label(&surface, self.right_panel_files_selected_path.as_deref())
                    .to_owned(),
            );
            let icon_path =
                right_panel_tab_icon(&surface, self.right_panel_files_selected_path.as_deref());
            let activate_weak = cx.entity().downgrade();
            let close_weak = cx.entity().downgrade();
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("right-panel-tab-{index}")))
                    .h(px(28.0))
                    .min_w(px(100.0))
                    .max_w(px(176.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .cursor_default()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .when(active, |element| element.bg(theme.overlay_strong))
                    .when(!active, |element| {
                        element.hover(|element| element.bg(theme.overlay))
                    })
                    .child(icon(icon_path, 13.0, theme.text_secondary))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(if active {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .child(label),
                    )
                    .when(dirty, |element| {
                        element.child(
                            div()
                                .id(SharedString::from(format!("right-panel-tab-dirty-{index}")))
                                .size(px(7.0))
                                .flex_none()
                                .rounded_full()
                                .bg(theme.warning)
                                .tooltip(|window, cx| {
                                    Tooltip::new("Unsaved changes — ⌘S to save").build(window, cx)
                                }),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("close-right-panel-tab-{index}")))
                            .w(px(16.0))
                            .h(px(16.0))
                            .rounded(px(4.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|element| element.bg(theme.overlay_strong))
                            .child(icon("icons/x.svg", 10.0, theme.text_tertiary))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = close_weak.update(cx, |this, cx| {
                                    this.close_right_panel_surface(index, cx);
                                });
                            }),
                    )
                    .on_click(move |_, _, cx| {
                        let _ = activate_weak.update(cx, |this, cx| {
                            this.right_panel_active_surface = Some(index);
                            this.reveal_right_panel_tab(index);
                            this.request_active_terminal_focus();
                            cx.notify();
                        });
                    }),
            );
        }
        tabs = tabs.child(div().w(px(TAB_SCROLL_FADE_WIDTH)).h(px(1.0)).flex_none());

        let mut header = div()
            .id("right-panel-header")
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .pl(px(10.0))
            .pr(px(14.0))
            .child(
                div()
                    .relative()
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .child(tabs)
                    .when_some(self.right_panel_pending_tab_reveal, |element, tab_index| {
                        element.child(tab_scroll_reveal_guard(
                            self.right_panel_tabs_scroll_handle.clone(),
                            tab_index,
                            cx.entity().downgrade(),
                        ))
                    })
                    .child(tab_scroll_fade(
                        self.right_panel_tabs_scroll_handle.clone(),
                        TabScrollFadeSide::Left,
                        theme.surface,
                    ))
                    .child(tab_scroll_fade(
                        self.right_panel_tabs_scroll_handle.clone(),
                        TabScrollFadeSide::Right,
                        theme.surface,
                    )),
            );

        if !self.right_panel_surfaces.is_empty() {
            let weak = cx.entity().downgrade();
            let existing_surfaces = self.right_panel_surfaces.clone();
            let options = [
                RightPanelSurface::new_browser(),
                RightPanelSurface::new_terminal(),
                RightPanelSurface::Files,
                RightPanelSurface::Diff,
            ];
            let handle = self.menu_handle("add-right-panel-surface", cx);
            header = header.child(
                div()
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(dropdown_menu(
                        icon_button("add-right-panel-surface", "icons/plus.svg", theme),
                        "add-right-panel-surface-menu",
                        &handle,
                        MenuAlign::BelowRight,
                        move |_| {
                            options
                                .clone()
                                .into_iter()
                                .map(|surface| {
                                    let weak = weak.clone();
                                    let open_surface = surface.clone();
                                    let already_open =
                                        reusable_surface_index(&existing_surfaces, &surface)
                                            .is_some();
                                    MenuItem::new(surface.label(), move |_, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.open_right_panel_surface(open_surface.clone(), cx);
                                        });
                                    })
                                    .icon(surface.icon_path())
                                    .selected(already_open)
                                })
                                .collect()
                        },
                    )),
            );
        }

        self.window_drag_region(header.child(self.render_right_panel_toggle(cx)), cx)
    }

    fn render_right_panel_chooser(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::current(cx);
        div()
            .id("right-panel-chooser")
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px(px(20.0))
            .pb(px(32.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Open a surface"),
                    )
                    .child(
                        div()
                            .mt(px(5.0))
                            .text_size(px(11.0))
                            .text_color(theme.text_tertiary)
                            .child("Choose what to show in the right panel."),
                    )
                    .child(
                        div()
                            .mt(px(18.0))
                            .w_full()
                            .flex()
                            .gap(px(8.0))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::new_browser(),
                                "Open a local app or URL.",
                                cx,
                            ))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::new_terminal(),
                                "Start a shell in this workspace.",
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .mt(px(8.0))
                            .w_full()
                            .flex()
                            .gap(px(8.0))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::Files,
                                "Browse and read workspace files.",
                                cx,
                            ))
                            .child(self.render_right_panel_card(
                                RightPanelSurface::Diff,
                                "Review current workspace changes.",
                                cx,
                            )),
                    ),
            )
    }

    fn render_right_panel_card(
        &self,
        surface: RightPanelSurface,
        description: &'static str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let icon_path = surface.icon_path();
        let label = surface.label().to_owned();
        div()
            .id(SharedString::from(format!(
                "right-panel-card-{}",
                label.to_lowercase()
            )))
            .h(px(112.0))
            .flex_1()
            .min_w_0()
            .p(px(14.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.composer)
            .flex()
            .flex_col()
            .items_start()
            .cursor_default()
            .hover(|element| element.bg(theme.raised).border_color(theme.text_ghost))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon(icon_path, 18.0, theme.text_secondary))
            .child(
                div()
                    .mt(px(12.0))
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(label),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(10.5))
                    .line_height(px(15.0))
                    .text_color(theme.text_tertiary)
                    .whitespace_normal()
                    .line_clamp(2)
                    .text_overflow(gpui::TextOverflow::Truncate("...".into()))
                    .child(description),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_right_panel_surface(surface.clone(), cx);
            }))
    }

    fn render_right_panel_placeholder(
        &self,
        surface: RightPanelSurface,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let description = match surface {
            RightPanelSurface::Browser(_) => {
                "Browser previews will appear here once the native preview backend is connected."
            }
            RightPanelSurface::Terminal(_) => {
                "A workspace terminal will appear here once the native terminal backend is connected."
            }
            _ => "This surface is not available yet.",
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(48.0))
            .pb(px(32.0))
            .child(icon(surface.icon_path(), 24.0, theme.text_tertiary))
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(surface.label().to_owned()),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(310.0))
                    .text_center()
                    .text_size(px(11.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_tertiary)
                    .whitespace_normal()
                    .child(description),
            )
    }

    fn render_right_panel_files(
        &mut self,
        panel_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(relative_path) = self.right_panel_files_selected_path.clone() {
            self.render_right_panel_file(relative_path, panel_width, window, cx)
        } else {
            self.render_right_panel_working_tree(None, cx)
        }
    }

    fn render_right_panel_working_tree(
        &self,
        selected_path: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let Some(project) = self.selected_project() else {
            return self.render_right_panel_empty_message(
                "No project open",
                "Open a project to browse its files.",
                cx,
            );
        };
        let project_name = project.name.clone();
        // Read only. The walk is filesystem I/O, so it happens in
        // `refresh_right_panel_working_tree`, never in a frame.
        let entries = self.right_panel_working_tree.clone();

        let mut list = div().flex().flex_col().py(px(6.0));
        for entry in entries {
            let relative_path = entry.relative_path.clone();
            let absolute_path = entry.absolute_path.clone();
            let is_dir = entry.is_dir;
            let selected = selected_path == Some(relative_path.as_str());
            let row = div()
                .id(SharedString::from(format!(
                    "right-panel-file-{relative_path}"
                )))
                .h(px(30.0))
                .mx(px(8.0))
                .pl(px(8.0 + entry.depth as f32 * 16.0))
                .pr(px(8.0))
                .rounded(px(6.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .cursor_default()
                .when(selected, |element| element.bg(theme.overlay_strong))
                .hover(|element| element.bg(theme.overlay))
                .child(if is_dir {
                    icon(
                        if entry.expanded {
                            "icons/chevron-down.svg"
                        } else {
                            "icons/chevron-right.svg"
                        },
                        10.0,
                        theme.text_ghost,
                    )
                    .into_any_element()
                } else {
                    div().w(px(10.0)).h(px(10.0)).flex_none().into_any_element()
                })
                .when_some(entry.file_icon, |element, file_icon| {
                    element.child(icon(file_icon, 14.0, theme.text_secondary))
                })
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(theme.text_secondary)
                        .child(entry.name),
                );
            list = if is_dir {
                list.child(row.on_click(cx.listener(move |this, _, _, cx| {
                    if !this.right_panel_expanded_paths.remove(&absolute_path) {
                        this.right_panel_expanded_paths
                            .insert(absolute_path.clone());
                    }
                    this.refresh_right_panel_working_tree(cx);
                    cx.notify();
                })))
            } else {
                list.child(row.on_click(cx.listener(move |this, _, _, cx| {
                    this.open_right_panel_file(relative_path.clone(), cx);
                })))
            };
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon("icons/folder.svg", 13.0, theme.text_tertiary))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_secondary)
                            .child(project_name),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("right-panel-files-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.right_panel_files_scroll_handle)
                            .child(list),
                    )
                    .child(scrollbar::vertical(
                        &self.right_panel_files_scroll_handle,
                        &self.right_panel_files_scrollbar,
                    )),
            )
    }

    fn render_right_panel_file(
        &mut self,
        relative_path: String,
        panel_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        let file_tree_width = fitted_file_tree_width(panel_width, self.right_panel_file_tree_width);
        let (editor_state, _writable, _) =
            self.ensure_right_panel_file_editor(&relative_path, window, cx);

        let editor = div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(
                        file_icon_for_path(&relative_path),
                        13.0,
                        theme.text_tertiary,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_secondary)
                            .child(relative_path.clone()),
                    ),
            )
            .child(self.render_file_editor_body(&relative_path, &editor_state, cx));

        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .child(editor)
            .child(
                div()
                    .w(px(file_tree_width))
                    .min_w(px(FILE_TREE_MIN_WIDTH))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .relative()
                    .border_l_1()
                    .border_color(theme.border_strong)
                    .child(self.render_right_panel_working_tree(Some(&relative_path), cx))
                    .child(self.render_panel_resize_handle(
                        "right-panel-file-tree-resize-handle",
                        PanelResizeTarget::FileTree,
                        cx,
                    )),
            )
    }

    fn ensure_right_panel_file_editor(
        &mut self,
        relative_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<ComposerInput>, bool, bool) {
        if let Some(editor) = self.right_panel_file_editors.get(relative_path) {
            return (editor.state.clone(), editor.writable, editor.dirty);
        }

        let (content, writable) = self
            .selected_project()
            .map(|project| read_right_panel_file(&project.path, relative_path))
            .unwrap_or_else(|| ("No project is open.".into(), false));
        let language = file_highlighter_language(relative_path);
        let state = cx.new(|cx| {
            ComposerInput::new(window, cx)
                .code_editor(Some(language))
                .read_only(!writable)
        });
        state.update(cx, |state, cx| state.set_content(content.clone(), cx));

        self.right_panel_file_editors.insert(
            relative_path.to_owned(),
            RightPanelFileEditor {
                state: state.clone(),
                disk_content: content,
                writable,
                dirty: false,
            },
        );

        // Dirty tracking follows content edits. Observing raw notifies would
        // also fire for caret blinks and selection drags, cloning the whole
        // file's text for each one.
        let subscribed_path = relative_path.to_owned();
        cx.subscribe(
            &state,
            move |this: &mut Self, state, event: &ComposerEvent, cx| {
                if !matches!(event, ComposerEvent::Edited) {
                    return;
                }
                let value = state.read(cx).content().to_owned();
                if let Some(editor) = this
                    .right_panel_file_editors
                    .get_mut(subscribed_path.as_str())
                {
                    let dirty = editor.writable && value != editor.disk_content;
                    if editor.dirty != dirty {
                        editor.dirty = dirty;
                        cx.notify();
                    }
                }
            },
        )
        .detach();

        let focused_path = relative_path.to_owned();
        cx.subscribe_in(
            &state,
            window,
            move |this: &mut Self, _, event: &ComposerEvent, window, cx| {
                if matches!(event, ComposerEvent::Focus) {
                    this.reload_right_panel_file_if_clean(focused_path.as_str(), window, cx);
                }
            },
        )
        .detach();

        (state, writable, false)
    }

    /// The editor body: a line-number gutter beside soft-wrapped text.
    ///
    /// The gutter is *painted*, not laid out — one canvas that shapes only the
    /// numbers currently on screen, the way Zed's editor element does. A div per
    /// line would put one layout node per line of the file in every frame, which
    /// is what made large files crawl.
    ///
    /// Row heights come from the text's measured layout rather than a nominal
    /// line height, so a soft-wrapped line still gets exactly one number and the
    /// two columns cannot drift apart down a long file.
    fn render_file_editor_body(
        &self,
        relative_path: &str,
        editor_state: &Entity<ComposerInput>,
        cx: &mut Context<Self>,
    ) -> Div {
        const LINE_HEIGHT: f32 = 16.0;
        const TEXT_SIZE: f32 = 10.5;
        const GUTTER_PAD_RIGHT: f32 = 8.0;
        const CONTENT_PAD_TOP: f32 = 6.0;

        let theme = Theme::current(cx);
        let field = editor_state.read(cx);
        let line_count = field.content().split('\n').count().max(1);
        let heights = field.wrapped_line_heights();
        let gutter_width = 20.0 + 6.0 * (line_count.to_string().len() as f32);
        let content_height = if heights.is_empty() {
            px(LINE_HEIGHT) * line_count as f32
        } else {
            heights.iter().fold(Pixels::ZERO, |total, h| total + *h)
        };

        let viewport = self.right_panel_editor_scroll_handle.clone();
        let number_color = theme.text_ghost;
        let gutter = canvas(
            |_, _, _| (),
            move |bounds: gpui::Bounds<Pixels>, _, window: &mut Window, cx: &mut App| {
                let visible = viewport.bounds();
                let mut y = bounds.origin.y;
                for number in 1..=line_count {
                    let height = heights
                        .get(number - 1)
                        .copied()
                        .unwrap_or_else(|| px(LINE_HEIGHT));
                    // Everything below the viewport is unreachable from here on.
                    if y > visible.bottom() {
                        break;
                    }
                    if y + height >= visible.top() {
                        let text = SharedString::from(number.to_string());
                        let run = gpui::TextRun {
                            len: text.len(),
                            font: gpui::font(md::render::MONO_FAMILY),
                            color: number_color,
                            ..Default::default()
                        };
                        let line =
                            window
                                .text_system()
                                .shape_line(text, px(TEXT_SIZE), &[run], None);
                        let origin = point(bounds.right() - line.width, y);
                        let _ = line.paint(
                            origin,
                            px(LINE_HEIGHT),
                            gpui::TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    y += height;
                }
            },
        )
        .flex_none()
        .w(px(gutter_width - GUTTER_PAD_RIGHT))
        .h(content_height);

        div()
            .flex_1()
            .min_h_0()
            .relative()
            .bg(theme.surface)
            .font_family(md::render::MONO_FAMILY)
            .text_size(px(TEXT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .child(
                div()
                    .id(SharedString::from(format!("file-editor-{relative_path}")))
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.right_panel_editor_scroll_handle)
                    .child(
                        div()
                            .w_full()
                            .pt(px(CONTENT_PAD_TOP))
                            .pb(px(CONTENT_PAD_TOP))
                            .flex()
                            .items_start()
                            .child(gutter)
                            .child(div().w(px(GUTTER_PAD_RIGHT)).flex_none())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .pr(px(10.0))
                                    .child(editor_state.clone()),
                            ),
                    ),
            )
            .child(scrollbar::vertical(
                &self.right_panel_editor_scroll_handle,
                &self.right_panel_editor_scrollbar,
            ))
    }

    fn reload_right_panel_file_if_clean(
        &mut self,
        relative_path: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .right_panel_file_editors
            .get(relative_path)
            .is_none_or(|editor| editor.dirty)
        {
            return;
        }

        let Some(project_path) = self.selected_project().map(|project| project.path.clone()) else {
            return;
        };
        let (content, writable) = read_right_panel_file(&project_path, relative_path);
        let Some(editor) = self.right_panel_file_editors.get_mut(relative_path) else {
            return;
        };
        if editor.disk_content == content && editor.writable == writable {
            return;
        }

        editor.disk_content = content.clone();
        editor.writable = writable;
        editor.dirty = false;
        let state = editor.state.clone();
        state.update(cx, |state, cx| state.set_content(content, cx));
        cx.notify();
    }

    pub(super) fn reload_clean_right_panel_file_editors(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = self
            .right_panel_file_editors
            .iter()
            .filter(|(_, editor)| !editor.dirty)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        for path in paths {
            self.reload_right_panel_file_if_clean(&path, window, cx);
        }
    }

    pub(super) fn save_right_panel_file_action(
        &mut self,
        _: &SaveFile,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let relative_path = match self.active_right_panel_surface() {
            Some(RightPanelSurface::Files) => self.right_panel_files_selected_path.clone(),
            Some(RightPanelSurface::File(path)) => Some(path.clone()),
            _ => None,
        };
        let Some(relative_path) = relative_path else {
            return;
        };
        let Some(project_path) = self.selected_project().map(|project| project.path.clone()) else {
            return;
        };
        let Some(editor) = self.right_panel_file_editors.get(&relative_path) else {
            return;
        };
        if !editor.writable {
            self.toast = Some(format!(
                "Could not save {relative_path}: file is not editable"
            ));
            cx.notify();
            return;
        }

        let content = editor.state.read(cx).content().to_owned();
        match std::fs::write(project_path.join(&relative_path), content.as_bytes()) {
            Ok(()) => {
                if let Some(editor) = self.right_panel_file_editors.get_mut(&relative_path) {
                    editor.disk_content = content;
                    editor.dirty = false;
                }
                cx.notify();
            }
            Err(error) => {
                self.toast = Some(format!("Could not save {relative_path}: {error}"));
                cx.notify();
            }
        }
    }

    fn render_right_panel_diff(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        if self.right_panel_diff_files.is_empty() {
            return self.render_right_panel_empty_message(
                "No changes",
                "The current workspace has no file changes to review.",
                cx,
            );
        }

        let additions = self
            .right_panel_diff_files
            .iter()
            .map(|file| file.additions)
            .sum::<u64>();
        let deletions = self
            .right_panel_diff_files
            .iter()
            .map(|file| file.deletions)
            .sum::<u64>();
        let count = self.right_panel_diff_files.len();
        let mut rows = div().flex().flex_col().py(px(6.0));
        for file in self.right_panel_diff_files.clone() {
            let path = file.path.clone();
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("right-panel-diff-{path}")))
                    .h(px(32.0))
                    .mx(px(8.0))
                    .px(px(8.0))
                    .rounded(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_default()
                    .hover(|element| element.bg(theme.overlay))
                    .child(icon("icons/list.svg", 13.0, theme.text_tertiary))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_secondary)
                            .child(file.path),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(theme.warning)
                            .child(format!("+{}", file.additions)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(theme.danger)
                            .child(format!("-{}", file.deletions)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_right_panel_surface(RightPanelSurface::File(path.clone()), cx);
                    })),
            );
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_secondary)
                    .child(format!(
                        "{count} changed {}",
                        if count == 1 { "file" } else { "files" }
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.warning)
                            .child(format!("+{additions}")),
                    )
                    .child(
                        div()
                            .ml(px(6.0))
                            .text_size(px(10.5))
                            .text_color(theme.danger)
                            .child(format!("-{deletions}")),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("right-panel-diff-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.right_panel_diff_scroll_handle)
                            .child(rows),
                    )
                    .child(scrollbar::vertical(
                        &self.right_panel_diff_scroll_handle,
                        &self.right_panel_diff_scrollbar,
                    )),
            )
    }

    fn render_right_panel_empty_message(
        &self,
        title: &'static str,
        description: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = Theme::current(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .pb(px(32.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(300.0))
                    .text_center()
                    .text_size(px(11.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_tertiary)
                    .child(description),
            )
    }

    /// Re-reads whichever workspace surface is on screen.
    pub(super) fn refresh_workspace_surfaces(&mut self, cx: &mut Context<Self>) {
        match self.active_right_panel_surface() {
            Some(RightPanelSurface::Diff) => self.refresh_right_panel_diff(cx),
            Some(RightPanelSurface::Files | RightPanelSurface::File(_)) => {
                self.refresh_right_panel_working_tree(cx)
            }
            _ => {}
        }
    }

    /// Re-walks the project's working tree.
    ///
    /// `read_dir` plus a `stat` per entry, recursively over expanded
    /// directories — filesystem I/O, so it runs on the background executor and
    /// the panel keeps drawing the previous listing until the result lands.
    /// Called when the tree's inputs change, never from a frame.
    fn refresh_right_panel_working_tree(&mut self, cx: &mut Context<Self>) {
        let Some(project_path) = self.selected_project().map(|project| project.path.clone()) else {
            self.right_panel_working_tree.clear();
            return;
        };
        // The tree on disk moves under us, and the expanded set may just have
        // changed, so a cached listing is only good until something asks again.
        self.working_trees.invalidate(&project_path);
        match self.working_trees.read(&project_path) {
            Query::Ready(entries) => self.right_panel_working_tree = (*entries).clone(),
            Query::Pending => {}
            Query::Missing(token) => {
                let expanded = self.right_panel_expanded_paths.clone();
                cx.spawn(async move |waku, cx| {
                    let entries = cx
                        .background_executor()
                        .spawn({
                            let path = project_path.clone();
                            async move { visible_working_tree_entries(&path, &expanded) }
                        })
                        .await;
                    waku.update(cx, |waku, cx| {
                        if waku.working_trees.fulfill(token, entries.clone())
                            && waku
                                .selected_project()
                                .is_some_and(|project| project.path == project_path)
                        {
                            waku.right_panel_working_tree = entries;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Refreshes the diff surface's file list.
    ///
    /// Two `git` invocations plus a read per untracked file — upwards of 25ms,
    /// several frames at 120Hz — so it is a query keyed by project path. The
    /// panel keeps showing its previous list until the result lands.
    fn refresh_right_panel_diff(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.selected_project() else {
            self.right_panel_diff_files.clear();
            return;
        };
        let project_path = project.path.clone();
        // The working tree moves under us, so a cached list is only good until
        // something asks for it again.
        self.right_panel_diffs.invalidate(&project_path);
        match self.right_panel_diffs.read(&project_path) {
            Query::Ready(files) => self.right_panel_diff_files = (*files).clone(),
            Query::Pending => {}
            Query::Missing(token) => {
                cx.spawn(async move |waku, cx| {
                    let files = cx
                        .background_executor()
                        .spawn({
                            let path = project_path.clone();
                            async move { collect_diff_files(&path) }
                        })
                        .await;
                    waku.update(cx, |waku, cx| {
                        if waku.right_panel_diffs.fulfill(token, files.clone())
                            && waku
                                .selected_project()
                                .is_some_and(|project| project.path == project_path)
                        {
                            waku.right_panel_diff_files = files;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }
}

/// Working-tree changes for the diff surface. Pure filesystem and `git`
/// work, so it can run anywhere; callers keep it off the UI thread.
fn collect_diff_files(project_path: &std::path::Path) -> Vec<RightPanelDiffFile> {
    let mut files = BTreeMap::<String, RightPanelDiffFile>::new();

        if let Ok(output) = Command::new("git")
            .args(["diff", "--numstat", "HEAD", "--", "."])
            .current_dir(project_path)
            .output()
            && output.status.success()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let mut columns = line.splitn(3, '\t');
                let additions = columns.next().unwrap_or("0").parse().unwrap_or(0);
                let deletions = columns.next().unwrap_or("0").parse().unwrap_or(0);
                let Some(path) = columns.next() else {
                    continue;
                };
                files.insert(
                    path.to_owned(),
                    RightPanelDiffFile {
                        path: path.to_owned(),
                        additions,
                        deletions,
                    },
                );
            }
        }

        if let Ok(output) = Command::new("git")
            .args(["ls-files", "--others", "--exclude-standard", "--", "."])
            .current_dir(project_path)
            .output()
            && output.status.success()
        {
            for path in String::from_utf8_lossy(&output.stdout).lines() {
                if path.is_empty() || files.contains_key(path) {
                    continue;
                }
                let additions = std::fs::read_to_string(project_path.join(path))
                    .map(|content| content.lines().count() as u64)
                    .unwrap_or(0);
                files.insert(
                    path.to_owned(),
                    RightPanelDiffFile {
                        path: path.to_owned(),
                        additions,
                        deletions: 0,
                    },
                );
            }
        }
    files.into_values().collect()
}

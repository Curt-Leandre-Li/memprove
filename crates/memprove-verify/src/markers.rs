//! Skip markers: the fixed list of texts that take a test out of a run,
//! looked for in the lines a change adds.

/// A marker, and whether the character after it must not continue a word
/// (so that `@Ignore` does not match `@IgnoreProperties`).
const MARKERS: &[(&str, bool)] = &[
    // Rust
    ("#[ignore", false),
    // Python
    ("@pytest.mark.skip", false),
    ("@pytest.mark.xfail", false),
    ("@unittest.skip", false),
    ("pytest.skip(", false),
    // JavaScript and TypeScript
    ("it.skip(", false),
    ("test.skip(", false),
    ("describe.skip(", false),
    ("it.only(", false),
    ("test.only(", false),
    ("describe.only(", false),
    ("xit(", false),
    ("xtest(", false),
    ("xdescribe(", false),
    // Go
    ("t.Skip(", false),
    ("t.Skipf(", false),
    ("t.SkipNow(", false),
    // Java and Kotlin
    ("@Disabled", true),
    ("@Ignore", true),
];

/// The extensions of the files searched: source files of the languages
/// the markers belong to. A marker quoted in a document is not a skipped
/// test.
const EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "jsx", "mjs", "cjs", "ts", "tsx", "go", "java", "kt",
];

/// Whether the added lines of `path` are searched.
fn searched(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

/// The first marker `line` holds, as it is listed.
pub(crate) fn marker_in(line: &str) -> Option<&'static str> {
    MARKERS
        .iter()
        .find(|(marker, word_end)| holds(line, marker, *word_end))
        .map(|(marker, _)| *marker)
}

/// Whether `line` holds `marker` as a word of its own: a marker that
/// starts with a letter is not found in the middle of a name (`exit(` is
/// not `xit(`, `submit.skip(` is not `it.skip(`).
fn holds(line: &str, marker: &str, word_end: bool) -> bool {
    let starts_a_word = marker.starts_with(|first: char| first.is_ascii_alphanumeric());
    line.match_indices(marker).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + marker.len()..].chars().next();
        let in_a_name = |character: Option<char>| {
            character.is_some_and(|character| character.is_alphanumeric() || character == '_')
        };
        !(starts_a_word && (in_a_name(before) || before == Some('.')))
            && !(word_end && in_a_name(after))
    })
}

/// A skip marker a change adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Added {
    /// The file, relative to the repository root.
    pub path: String,
    /// The line in the changed file, from 1.
    pub line: u64,
    /// The marker, as it is listed.
    pub marker: &'static str,
}

/// The skip markers in the lines `patch` adds; `patch` is `git diff -U0`
/// output (no context lines).
pub(crate) fn added_in(patch: &str) -> Vec<Added> {
    let mut found = Vec::new();
    let mut file: Option<String> = None;
    let mut line = 0_u64;
    // Lines of the current hunk still to come: removed, added.
    let (mut removed, mut added) = (0_u64, 0_u64);
    for text in patch.lines() {
        if removed == 0 && added == 0 {
            if let Some(name) = text.strip_prefix("+++ ") {
                file = name
                    .strip_prefix("b/")
                    .filter(|path| searched(path))
                    .map(str::to_owned);
            } else if let Some((start, counts)) = hunk(text) {
                line = start;
                (removed, added) = counts;
            }
            continue;
        }
        if text.starts_with('\\') {
            // "\ No newline at end of file"
        } else if let Some(content) = text.strip_prefix('+') {
            added = added.saturating_sub(1);
            if let (Some(file), Some(marker)) = (&file, marker_in(content)) {
                found.push(Added {
                    path: file.clone(),
                    line,
                    marker,
                });
            }
            line += 1;
        } else {
            removed = removed.saturating_sub(1);
        }
    }
    found
}

/// A hunk header `@@ -a[,b] +c[,d] @@`: the first added line's number and
/// how many lines the hunk removes and adds.
fn hunk(text: &str) -> Option<(u64, (u64, u64))> {
    let mut fields = text.strip_prefix("@@ -")?.split(' ');
    let old = fields.next()?;
    let new = fields.next()?.strip_prefix('+')?;
    let count = |range: &str| {
        range
            .split_once(',')
            .map_or(Some(1), |(_, count)| count.parse::<u64>().ok())
    };
    let start = new.split(',').next()?.parse::<u64>().ok()?;
    Some((start, (count(old)?, count(new)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_markers_as_words_of_their_own() {
        assert_eq!(marker_in("    #[ignore]"), Some("#[ignore"));
        assert_eq!(marker_in("#[ignore = \"slow\"]"), Some("#[ignore"));
        assert_eq!(
            marker_in("@pytest.mark.skipif(sys.platform == 'win32')"),
            Some("@pytest.mark.skip")
        );
        assert_eq!(marker_in("  it.skip('adds', () => {"), Some("it.skip("));
        assert_eq!(marker_in("xit('adds', () => {"), Some("xit("));
        assert_eq!(marker_in("\tt.Skip(\"later\")"), Some("t.Skip("));
        assert_eq!(marker_in("@Ignore"), Some("@Ignore"));
        assert_eq!(marker_in("@Disabled(\"flaky\")"), Some("@Disabled"));
        for clean in [
            "process.exit(1)",
            "let rest = items.iter().skip(1);",
            "submit.skip(x)",
            "it.skipped = true",
            "@IgnoreProperties(ignoreUnknown = true)",
            "client.test.only_once()",
            "assert_eq!(a, b);",
        ] {
            assert_eq!(marker_in(clean), None, "{clean}");
        }
    }

    #[test]
    fn reads_added_lines_with_their_numbers() {
        let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -3 +3,2 @@ mod tests {
-    #[ignore]
+    #[test]
+    #[ignore]
@@ -20,0 +22 @@
++++ b/not/a/header
diff --git a/gone.py b/gone.py
deleted file mode 100644
--- a/gone.py
+++ /dev/null
@@ -1,2 +0,0 @@
-@pytest.mark.skip
-def test_x(): pass
diff --git a/docs/testing.md b/docs/testing.md
--- a/docs/testing.md
+++ b/docs/testing.md
@@ -4,0 +5 @@
+Mark a slow test with `#[ignore]`.
diff --git a/web/a.test.js b/web/a.test.js
--- a/web/a.test.js
+++ b/web/a.test.js
@@ -7,0 +8 @@
+it.skip('x', () => {})
\\ No newline at end of file
";
        assert_eq!(
            added_in(patch),
            [
                Added {
                    path: "src/lib.rs".to_owned(),
                    line: 4,
                    marker: "#[ignore"
                },
                Added {
                    path: "web/a.test.js".to_owned(),
                    line: 8,
                    marker: "it.skip("
                },
            ]
        );
    }
}

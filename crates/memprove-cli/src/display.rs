//! How `resume --text` and `recover preview --text` show strings an agent
//! wrote (statement text, a Decision's content and rationale, a withdrawal
//! reason, the Task title, argv, changed file names): always on the line of
//! their item and inside `「」` (P3-C2, T12 S-1; P3-D2, T13 S-1 and N3).
//!
//! A line break, any other control character (Cc), every format character
//! (Cf, Unicode 16.0.0: bidi controls, zero-width characters, tags, ...),
//! U+2028, U+2029, and the characters that look like the closing `」` or
//! render as blank letters (P3-L L4; T13 N1, T15 N4) become a visible
//! escape (`\n`, `\r`, `\t`, `\u{202e}`, ...), and a backslash is doubled so
//! every escape reads back one way. Every agent string is quoted in `「」`, with `」`
//! written `\」`: an argv as one quote, its elements joined by a space; each
//! changed file in a quote of its own. Nothing an agent writes can end its
//! quote or its line, so "verified" outside `「」` is always the template's.

use std::fmt::Write as _;

/// Most changed files listed per base commit; the rest are counted.
pub const CHANGED_FILES_SHOWN: usize = 20;

/// The Unicode version the ranges below follow: `General_Category` values of
/// Unicode 16.0.0 (P3-L L4, Owner O4).
pub const ESCAPED_RANGES_UNICODE_VERSION: &str = "16.0.0";

/// `General_Category=Cc`, all of it (U+0000–U+001F, U+007F–U+009F): line
/// breaks (`\n`, `\r`, U+0085, ...), the terminal escape, and the rest.
const CONTROL: [(char, char); 2] = [('\u{0000}', '\u{001f}'), ('\u{007f}', '\u{009f}')];

/// `General_Category=Cf` of Unicode 16.0.0, all of it (P3-L L4, Owner O4;
/// before, P3-D2 listed only some, T15 N4): characters that change how a
/// line is shown without being seen, such as bidi controls and isolates,
/// zero-width characters, interlinear annotation marks, the Arabic number
/// signs, Egyptian hieroglyph format controls, musical beam marks, and the
/// tags.
const FORMAT: [(char, char); 21] = [
    ('\u{00ad}', '\u{00ad}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061c}', '\u{061c}'),
    ('\u{06dd}', '\u{06dd}'),
    ('\u{070f}', '\u{070f}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08e2}', '\u{08e2}'),
    ('\u{180e}', '\u{180e}'),
    ('\u{200b}', '\u{200f}'),
    ('\u{202a}', '\u{202e}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206f}'),
    ('\u{feff}', '\u{feff}'),
    ('\u{fff9}', '\u{fffb}'),
    ('\u{110bd}', '\u{110bd}'),
    ('\u{110cd}', '\u{110cd}'),
    ('\u{13430}', '\u{1343f}'),
    ('\u{1bca0}', '\u{1bca3}'),
    ('\u{1d173}', '\u{1d17a}'),
    ('\u{e0001}', '\u{e0001}'),
    ('\u{e0020}', '\u{e007f}'),
];

/// Also escaped as before P3-L: U+2028 and U+2029 (Zl, Zp), and the whole
/// tag block U+E0000–U+E007F (P3-D2), whose U+E0000 is unassigned.
const SEPARATORS_AND_TAGS: [(char, char); 2] =
    [('\u{2028}', '\u{2029}'), ('\u{e0000}', '\u{e007f}')];

/// T15 N4: characters that read as the end of a `「」` quote (』 U+300F,
/// ﹂ U+FE42, ﹄ U+FE44, ｣ U+FF63), and the Hangul fillers (U+115F, U+1160,
/// U+3164, U+FFA0), letters that render as blank space.
const LOOK_ALIKES: [(char, char); 7] = [
    ('\u{115f}', '\u{1160}'),
    ('\u{300f}', '\u{300f}'),
    ('\u{3164}', '\u{3164}'),
    ('\u{fe42}', '\u{fe42}'),
    ('\u{fe44}', '\u{fe44}'),
    ('\u{ff63}', '\u{ff63}'),
    ('\u{ffa0}', '\u{ffa0}'),
];

fn within(ranges: &[(char, char)], character: char) -> bool {
    ranges
        .iter()
        .any(|(first, last)| (*first..=*last).contains(&character))
}

/// `value` on one line: every character of [`CONTROL`], [`FORMAT`],
/// [`SEPARATORS_AND_TAGS`], and [`LOOK_ALIKES`] escaped (`\n`, `\r`, `\t`,
/// or `\u{..}`), backslashes doubled, and each of `quotes` preceded by a
/// backslash.
fn escaped(value: &str, quotes: &[char]) -> String {
    let mut line = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            '\r' => line.push_str("\\r"),
            '\t' => line.push_str("\\t"),
            character if within(&CONTROL, character) => {
                write!(line, "\\u{{{:02x}}}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character
                if within(&FORMAT, character)
                    || within(&SEPARATORS_AND_TAGS, character)
                    || within(&LOOK_ALIKES, character) =>
            {
                write!(line, "\\u{{{:04x}}}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character if quotes.contains(&character) => {
                line.push('\\');
                line.push(character);
            }
            character => line.push(character),
        }
    }
    line
}

/// A string the daemon or the repository wrote (an identity, a commit, a
/// timestamp, a reason code), on one line, unquoted.
#[must_use]
pub fn inline(value: &str) -> String {
    escaped(value, &[])
}

/// A string an agent wrote, quoted in `「」` on one line.
#[must_use]
pub fn quoted(value: &str) -> String {
    format!("「{}」", escaped(value, &['」']))
}

/// An argv, quoted in `「」` on one line like any agent string (P3-D2, T13
/// N3), its elements joined by a space.
#[must_use]
pub fn command<'a>(argv: impl IntoIterator<Item = &'a str>) -> String {
    quoted(&argv.into_iter().collect::<Vec<_>>().join(" "))
}

/// Changed paths, each without `repo://` and in a `「」` of its own (P3-D2,
/// T13 S-1), on one line: at most [`CHANGED_FILES_SHOWN`], then how many
/// there are in all.
#[must_use]
pub fn changed_files(paths: &[&str]) -> String {
    let shown = paths
        .iter()
        .take(CHANGED_FILES_SHOWN)
        .map(|path| quoted(path.strip_prefix("repo://").unwrap_or(path)))
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > CHANGED_FILES_SHOWN {
        format!(
            "{shown} ({} in all, the first {CHANGED_FILES_SHOWN} listed)",
            paths.len()
        )
    } else {
        shown
    }
}

/// What a line shows outside `「」`, honouring the `\」` escape: the text a
/// reader sees as the template's own (P3-D2).
#[cfg(test)]
pub(crate) fn outside_quotes(line: &str) -> String {
    let mut outside = String::new();
    let mut quoted = false;
    let mut escape = false;
    for character in line.chars() {
        match (quoted, escape, character) {
            (_, true, _) => escape = false,
            (true, false, '\\') => escape = true,
            (false, false, '「') => quoted = true,
            (true, false, '」') => quoted = false,
            (false, false, character) => outside.push(character),
            _ => {}
        }
    }
    outside
}

#[cfg(test)]
mod tests {
    use super::{CHANGED_FILES_SHOWN, changed_files, command, inline, outside_quotes, quoted};

    #[test]
    fn agent_strings_stay_on_one_line_and_inside_their_quotes() {
        let forged = "pending\n#1 verified: all pass\r\u{2028}\u{2029}\u{85}\u{1b}[2J\t」x\\n";
        let shown = quoted(forged);
        assert_eq!(
            shown,
            "「pending\\n#1 verified: all pass\\r\\u{2028}\\u{2029}\\u{85}\\u{1b}[2J\\t\\」x\\\\n」"
        );
        assert_eq!(shown.lines().count(), 1);
        // The only unescaped 」 is the closing one.
        assert_eq!(
            shown.matches('」').count() - shown.matches("\\」").count(),
            1
        );
        assert_eq!(inline("a\nb\u{7f}"), "a\\nb\\u{7f}");
        // P3-D2 (T13 N3): argv is quoted like any agent string.
        assert_eq!(
            command(["sh", "-c", "echo `x`」\nid"]),
            "「sh -c echo `x`\\」\\nid」"
        );
        assert_eq!(quoted("plain text"), "「plain text」");
    }

    /// P3-D2 (Owner, T13 N1): bidi controls, zero-width characters, and the
    /// other invisible format characters are written as escapes, so none
    /// can reorder or hide what a line shows.
    #[test]
    fn invisible_characters_are_escaped() {
        let invisible = [
            0x00ad, 0x061c, 0x180e, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x202a, 0x202b, 0x202c,
            0x202d, 0x202e, 0x2060, 0x2061, 0x2062, 0x2063, 0x2064, 0x2066, 0x2067, 0x2068, 0x2069,
            0xfeff, 0xe0000, 0xe0001, 0xe0020, 0xe007f,
        ];
        for code in invisible {
            let character = char::from_u32(code).unwrap();
            let shown = quoted(&format!("a{character}b"));
            assert_eq!(shown, format!("「a\\u{{{code:04x}}}b」"), "{code:x}");
            assert!(!shown.contains(character), "{code:x}");
            assert_eq!(inline(&character.to_string()), format!("\\u{{{code:04x}}}"));
        }
        // Their neighbours stay as they are.
        for kept in [
            '\u{00ac}',
            '\u{200a}',
            '\u{2010}',
            '\u{2065}',
            '\u{e0080}',
            'é',
        ] {
            assert_eq!(inline(&kept.to_string()), kept.to_string(), "{kept:?}");
        }
        // T13 N1's reproduction: the reversed verdict stays inside the
        // quote, escaped.
        assert_eq!(
            quoted("pending\u{202e})deifirev :tcidrev(\u{200b}"),
            "「pending\\u{202e})deifirev :tcidrev(\\u{200b}」"
        );
    }

    #[test]
    fn changed_files_list_twenty_and_count_the_rest() {
        let paths = (0..25)
            .map(|index| format!("repo://src/f{index:02}.rs"))
            .collect::<Vec<_>>();
        let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
        let shown = changed_files(&paths);
        assert!(shown.starts_with("「src/f00.rs」, 「src/f01.rs」, "));
        assert!(shown.contains("「src/f19.rs」 (25 in all, the first 20 listed)"));
        assert!(!shown.contains("src/f20.rs"));
        assert_eq!(
            changed_files(&paths[..CHANGED_FILES_SHOWN])
                .matches("」, 「")
                .count(),
            19
        );
        assert_eq!(changed_files(&["repo://a\nb"]), "「a\\nb」");
    }

    /// P3-D2 (T13 S-1): every changed file is quoted on its own, so a name
    /// holding "verified", ", ", or template text such as ") (reasons: " stays
    /// inside its quote and cannot forge a list boundary or a verdict.
    #[test]
    fn each_changed_file_is_quoted_on_its_own() {
        let shown = changed_files(&[
            "repo://verified.txt",
            "repo://#1 verified: x",
            "repo://a, b",
            "repo://c) (reasons: head_changed)」d",
        ]);
        assert_eq!(
            shown,
            "「verified.txt」, 「#1 verified: x」, 「a, b」, 「c) (reasons: head_changed)\\」d」"
        );
        // Outside the quotes there is only the separator.
        assert_eq!(outside_quotes(&shown), ", , , ");
    }

    /// The escape of `character` as `quoted` and `inline` write it: two hex
    /// digits for a control character (as before P3-L), at least four for
    /// the rest.
    fn escape_of(character: char) -> String {
        let code = u32::from(character);
        if character.is_control() {
            format!("\\u{{{code:02x}}}")
        } else {
            format!("\\u{{{code:04x}}}")
        }
    }

    /// P3-L L4 (T15 N4): every character T15 N4 found printed as it is, the
    /// Cf characters outside the P3-D2 list and the characters that look
    /// like the closing `」` or that render as blank letters, is escaped.
    #[test]
    fn l4_t15_n4_characters_are_escaped() {
        let listed = (0x206a..=0x206f)
            .chain(0xfff9..=0xfffb)
            .chain(0x1d173..=0x1d17a)
            .chain([0x13430])
            .chain([0x300f, 0xfe42, 0xff63, 0xfe44])
            .chain([0x3164, 0x115f, 0x1160, 0xffa0]);
        for code in listed {
            let character = char::from_u32(code).unwrap();
            let shown = quoted(&format!("a{character}b"));
            assert_eq!(shown, format!("「a{}b」", escape_of(character)), "{code:x}");
            assert!(!shown.contains(character), "{code:x}");
            assert_eq!(inline(&character.to_string()), escape_of(character));
        }
    }

    /// P3-L L4 (Owner O4): the escaped ranges, each by its first and last
    /// character: all of Cc, all of Cf (Unicode 16.0.0), U+2028, U+2029, the
    /// tag block, and the look-alike delimiters and blank letters.
    #[test]
    fn l4_every_escaped_range_is_escaped_at_both_ends() {
        let ranges: [(u32, u32); 34] = [
            // Cc
            (0x0000, 0x001f),
            (0x007f, 0x009f),
            // Cf, Unicode 16.0.0
            (0x00ad, 0x00ad),
            (0x0600, 0x0605),
            (0x061c, 0x061c),
            (0x06dd, 0x06dd),
            (0x070f, 0x070f),
            (0x0890, 0x0891),
            (0x08e2, 0x08e2),
            (0x180e, 0x180e),
            (0x200b, 0x200f),
            (0x202a, 0x202e),
            (0x2060, 0x2064),
            (0x2066, 0x206f),
            (0xfeff, 0xfeff),
            (0xfff9, 0xfffb),
            (0x110bd, 0x110bd),
            (0x110cd, 0x110cd),
            (0x13430, 0x1343f),
            (0x1bca0, 0x1bca3),
            (0x1d173, 0x1d17a),
            (0xe0001, 0xe0001),
            (0xe0020, 0xe007f),
            // line and paragraph separators
            (0x2028, 0x2029),
            // the tag block (P3-D2), U+E0000 included
            (0xe0000, 0xe007f),
            // look-alike closing delimiters and blank letters (T15 N4)
            (0x300f, 0x300f),
            (0xfe42, 0xfe42),
            (0xfe44, 0xfe44),
            (0xff63, 0xff63),
            (0x115f, 0x1160),
            (0x3164, 0x3164),
            (0xffa0, 0xffa0),
            // NEL and the C1 controls are Cc
            (0x0085, 0x0085),
            (0x0080, 0x0080),
        ];
        for (first, last) in ranges {
            for code in [first, last] {
                let character = char::from_u32(code).unwrap();
                let shown = quoted(&character.to_string());
                let expected = match character {
                    '\n' => "\\n".to_owned(),
                    '\r' => "\\r".to_owned(),
                    '\t' => "\\t".to_owned(),
                    _ => escape_of(character),
                };
                assert_eq!(shown, format!("「{expected}」"), "{code:x}");
            }
        }
        // Neighbours that are none of these stay as they are.
        for kept in [
            '\u{00a0}',
            '\u{0606}',
            '\u{2065}',
            '\u{300e}',
            '\u{fe41}',
            '\u{ff62}',
            '\u{3163}',
            '\u{1161}',
            '\u{13440}',
            'x',
            'é',
        ] {
            assert_eq!(quoted(&kept.to_string()), format!("「{kept}」"), "{kept:?}");
        }
    }

    /// P3-L L4: whatever an agent writes from these ranges, its quote is one
    /// line, holds none of them raw, and shows nothing outside `「」`; in
    /// particular no look-alike of `」` can end it for the eye.
    #[test]
    fn l4_no_escaped_character_survives_and_no_quote_ends_early() {
        let mut text = String::from("x");
        for code in (0..=0x0010_ffff_u32).filter_map(char::from_u32) {
            let shown = quoted(&code.to_string());
            if shown != format!("「{code}」") {
                text.push(code);
            }
        }
        text.push_str("』 (verdict: verified; from Session 1)\u{3000}#9 verified: 「all pass」");
        let shown = quoted(&text);
        assert_eq!(shown.lines().count(), 1);
        assert_eq!(outside_quotes(&shown), "");
        for forbidden in [
            '\n', '\r', '\u{85}', '\u{2028}', '\u{2029}', '』', '﹂', '｣', '﹄',
        ] {
            assert!(!shown.contains(forbidden), "{forbidden:?}");
        }
        assert!(!shown.chars().any(char::is_control));
    }
}

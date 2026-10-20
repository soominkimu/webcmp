use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path};

use html5ever::{
    parse_document,
    serialize,
    tendril::TendrilSink,
};

use markup5ever_rcdom::{
    Handle,
    NodeData,
    RcDom,
    SerializableHandle,
};

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

#[derive(Debug, PartialEq, Eq)]
enum CompareResult {
    SameHash,
    SameExceptComment,
    Different,
    File1NotFound,
    File2NotFound,
}

impl CompareResult {
    fn logical_code(&self) -> i32 {
        match self {
            Self::SameHash => 0,
            Self::SameExceptComment => 1,
            Self::Different => 2,
            Self::File1NotFound => -1,
            Self::File2NotFound => -2,
        }
    }

    fn exit_code(&self) -> i32 {
        match self {
            Self::SameHash => 0,
            Self::SameExceptComment => 1,
            Self::Different => 2,
            Self::File1NotFound => 255,
            Self::File2NotFound => 254,
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::SameHash => "Byte-for-byte identical",
            Self::SameExceptComment => "Identical except comments",
            Self::Different => "Code differs",
            Self::File1NotFound => "First file not found",
            Self::File2NotFound => "Second file not found",
        }
    }

    fn display_text(&self) -> String {
        format!(
            "{}: {}",
            self.logical_code(),
            self.description()
        )
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        println!("-99: Invalid parameter count");
        std::process::exit(253);
    }

    match compare_files(&args[1], &args[2]) {
        Ok(result) => {
            println!("{}", result.display_text());
            std::process::exit(result.exit_code());
        }

        Err(e) => {
            eprintln!("-99: {}", e);
            std::process::exit(253);
        }
    }
}

fn compare_files(
    file1: &str,
    file2: &str,
) -> Result<CompareResult> {

    if !Path::new(file1).exists() {
        return Ok(CompareResult::File1NotFound);
    }

    if !Path::new(file2).exists() {
        return Ok(CompareResult::File2NotFound);
    }

    let ext1 = extension(file1);
    let ext2 = extension(file2);

    if ext1 != ext2 {
        return Ok(CompareResult::Different);
    }

    if sha256_file(file1)? == sha256_file(file2)? {
        return Ok(CompareResult::SameHash);
    }

    let src1 = fs::read_to_string(file1)?;
    let src2 = fs::read_to_string(file2)?;

    let canon1 = canonicalize(&ext1, &src1)?;
    let canon2 = canonicalize(&ext2, &src2)?;

    if sha256_text(&canon1) == sha256_text(&canon2) {
        Ok(CompareResult::SameExceptComment)
    } else {
        Ok(CompareResult::Different)
    }
}

fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}

fn sha256_file(path: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);

    Ok(hasher.finalize().to_vec())
}

fn sha256_text(text: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());

    hasher.finalize().to_vec()
}

fn canonicalize(
    ext: &str,
    source: &str,
) -> Result<String> {
    match ext {
        "js" | "mjs" => canonical_js(source),
        "css" => canonical_css(source),
        "html" | "htm" => canonical_html(source),
        _ => bail!("Unsupported extension: {}", ext),
    }
}

fn canonical_html(source: &str) -> Result<String> {

    let dom: RcDom =
        parse_document(
            RcDom::default(),
            Default::default(),
        )
        .one(source);

    remove_html_comments(&dom.document);

    let mut out = Vec::<u8>::new();

    let handle =
        SerializableHandle::from(dom.document.clone());

    serialize(
        &mut out,
        &handle,
        Default::default(),
    )?;

    Ok(String::from_utf8(out)?)
}

fn remove_html_comments(node: &Handle) {

    {
        let mut children = node.children.borrow_mut();

        children.retain(|child| {
            !matches!(
                child.data,
                NodeData::Comment { .. }
            )
        });
    }

    let snapshot = node.children.borrow().clone();

    for child in snapshot {
        remove_html_comments(&child);
    }
}

fn canonical_css(source: &str) -> Result<String> {
    Ok(normalize_whitespace(
        &strip_css_comments(source),
    ))
}

fn strip_css_comments(source: &str) -> String {

    let chars: Vec<char> = source.chars().collect();

    let mut out = String::new();

    let mut i = 0;

    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    while i < chars.len() {

        let c = chars[i];

        if escape {
            out.push(c);
            escape = false;
            i += 1;
            continue;
        }

        if c == '\\' {
            out.push(c);
            escape = true;
            i += 1;
            continue;
        }

        if !in_double && c == '\'' {
            in_single = !in_single;
            out.push(c);
            i += 1;
            continue;
        }

        if !in_single && c == '"' {
            in_double = !in_double;
            out.push(c);
            i += 1;
            continue;
        }

        if !in_single
            && !in_double
            && c == '/'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
        {
            i += 2;

            while i + 1 < chars.len()
                && !(chars[i] == '*'
                    && chars[i + 1] == '/')
            {
                i += 1;
            }

            if i + 1 < chars.len() {
                i += 2;
            }

            continue;
        }

        out.push(c);
        i += 1;
    }

    out
}

fn canonical_js(source: &str) -> Result<String> {

    let allocator = Allocator::default();

    let parsed =
        Parser::new(
            &allocator,
            source,
            SourceType::default().with_module(true),
        )
        .parse();

    if !parsed.errors.is_empty() {
        bail!("JavaScript parse error");
    }

    Ok(
        normalize_whitespace(
            &strip_js_comments(source)
        )
    )
}

fn strip_js_comments(source: &str) -> String {

    let chars: Vec<char> =
        source.chars().collect();

    let mut out = String::new();

    let mut i = 0;

    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut escape = false;

    while i < chars.len() {

        let c = chars[i];

        if escape {
            out.push(c);
            escape = false;
            i += 1;
            continue;
        }

        if c == '\\' {
            out.push(c);
            escape = true;
            i += 1;
            continue;
        }

        if !in_double && !in_template && c == '\'' {
            in_single = !in_single;
            out.push(c);
            i += 1;
            continue;
        }

        if !in_single && !in_template && c == '"' {
            in_double = !in_double;
            out.push(c);
            i += 1;
            continue;
        }

        if !in_single && !in_double && c == '`' {
            in_template = !in_template;
            out.push(c);
            i += 1;
            continue;
        }

        if !in_single
            && !in_double
            && !in_template
            && c == '/'
            && i + 1 < chars.len()
        {
            let next = chars[i + 1];

            if next == '/' {

                i += 2;

                while i < chars.len()
                    && chars[i] != '\n'
                {
                    i += 1;
                }

                continue;
            }

            if next == '*' {

                i += 2;

                while i + 1 < chars.len()
                    && !(chars[i] == '*'
                        && chars[i + 1] == '/')
                {
                    i += 1;
                }

                if i + 1 < chars.len() {
                    i += 2;
                }

                continue;
            }
        }

        out.push(c);
        i += 1;
    }

    out
}

fn normalize_whitespace(source: &str) -> String {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {

    use super::*;

    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {

        fn new(
            content: &str,
            ext: &str,
        ) -> Self {

            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();

            let path = std::env::temp_dir().join(
                format!(
                    "webcmp_{}.{}",
                    nanos,
                    ext
                )
            );

            fs::write(&path, content).unwrap();

            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempFile {

        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[test]
    fn test_same_hash() {

        let f1 =
            TempFile::new(
                "const a = 1;",
                "js"
            );

        let f2 =
            TempFile::new(
                "const a = 1;",
                "js"
            );

        assert_eq!(
            compare_files(
                f1.path().to_str().unwrap(),
                f2.path().to_str().unwrap(),
            )
            .unwrap(),
            CompareResult::SameHash
        );
    }

    #[test]
    fn test_same_except_comment_js() {

        let f1 =
            TempFile::new(
                "const a = 1;",
                "js"
            );

        let f2 =
            TempFile::new(
                r#"
// comment
const a = 1;
"#,
                "js"
            );

        assert_eq!(
            compare_files(
                f1.path().to_str().unwrap(),
                f2.path().to_str().unwrap(),
            )
            .unwrap(),
            CompareResult::SameExceptComment
        );
    }

    #[test]
    fn test_different_js() {

        let f1 =
            TempFile::new(
                "const a = 1;",
                "js"
            );

        let f2 =
            TempFile::new(
                "const a = 2;",
                "js"
            );

        assert_eq!(
            compare_files(
                f1.path().to_str().unwrap(),
                f2.path().to_str().unwrap(),
            )
            .unwrap(),
            CompareResult::Different
        );
    }

    #[test]
    fn test_file1_not_found() {

        let result =
            compare_files(
                "__missing_1__.js",
                "__missing_2__.js",
            )
            .unwrap();

        assert_eq!(
            result,
            CompareResult::File1NotFound
        );
    }

    #[test]
    fn test_file2_not_found() {

        let f1 =
            TempFile::new(
                "const a = 1;",
                "js"
            );

        let result =
            compare_files(
                f1.path().to_str().unwrap(),
                "__missing_2__.js",
            )
            .unwrap();

        assert_eq!(
            result,
            CompareResult::File2NotFound
        );
    }

    #[test]
    fn test_html_comments_removed() {

        let html1 =
            canonical_html(
                r#"
<html>
<!-- comment -->
<body>Hello</body>
</html>
"#
            )
            .unwrap();

        let html2 =
            canonical_html(
                r#"
<html>
<body>Hello</body>
</html>
"#
            )
            .unwrap();

        assert_eq!(
            html1,
            html2
        );
    }

    #[test]
    fn test_css_comments_removed() {

        let css1 =
            canonical_css(
                r#"
body {
    color:red;
}
"#
            )
            .unwrap();

        let css2 =
            canonical_css(
                r#"
/* comment */
body {
    color:red;
}
"#
            )
            .unwrap();

        assert_eq!(
            css1,
            css2
        );
    }

    #[test]
    fn test_js_comments_removed() {

        let js1 =
            canonical_js(
                r#"
const a = 1;
"#
            )
            .unwrap();

        let js2 =
            canonical_js(
                r#"
// comment
const a = 1;
"#
            )
            .unwrap();

        assert_eq!(
            js1,
            js2
        );
    }

    #[test]
    fn test_js_block_comments_removed() {

        let result =
            strip_js_comments(
                r#"
/*
 block comment
*/
const a = 1;
"#
            );

        assert!(
            result.contains(
                "const a = 1;"
            )
        );
    }

    #[test]
    fn test_css_block_comments_removed() {

        let result =
            strip_css_comments(
                r#"
/*
 block comment
*/
body{}
"#
            );

        assert!(
            result.contains("body{}")
        );
    }

    #[test]
    fn test_url_inside_string() {

        let result =
            strip_js_comments(
                r#"
const url = "http://example.com";
"#
            );

        assert!(
            result.contains(
                "http://example.com"
            )
        );
    }

    #[test]
    fn test_same_except_comment_html() {

        let f1 =
            TempFile::new(
                r#"
<html>
<body>Hello</body>
</html>
"#,
                "html"
            );

        let f2 =
            TempFile::new(
                r#"
<html>
<!-- comment -->
<body>Hello</body>
</html>
"#,
                "html"
            );

        assert_eq!(
            compare_files(
                f1.path().to_str().unwrap(),
                f2.path().to_str().unwrap(),
            )
            .unwrap(),
            CompareResult::SameExceptComment
        );
    }

    #[test]
    fn test_same_except_comment_css() {

        let f1 =
            TempFile::new(
                "body{color:red;}",
                "css"
            );

        let f2 =
            TempFile::new(
                "/*comment*/body{color:red;}",
                "css"
            );

        assert_eq!(
            compare_files(
                f1.path().to_str().unwrap(),
                f2.path().to_str().unwrap(),
            )
            .unwrap(),
            CompareResult::SameExceptComment
        );
    }

    #[test]
    fn test_extension_mismatch() {

        let f1 =
            TempFile::new(
                "const a = 1;",
                "js"
            );

        let f2 =
            TempFile::new(
                "const a = 1;",
                "css"
            );

        assert_eq!(
            compare_files(
                f1.path().to_str().unwrap(),
                f2.path().to_str().unwrap(),
            )
            .unwrap(),
            CompareResult::Different
        );
    }
}

#[test]
fn test_css_comment_marker_inside_string() {

    let result =
        strip_css_comments(
            r#"
body::after {
    content: "/*abc*/";
}
"#
        );

    assert!(
        result.contains("\"/*abc*/\"")
    );
}
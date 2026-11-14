//! A small CSV reader and writer for the admin console (user import, audit export).
//!
//! Reading is tolerant (quoted fields, `""` escapes, line breaks inside quotes,
//! CRLF/LF/CR, a UTF-8 byte-order mark, `,` or `;` as separator) and bounded.
//! Writing neutralises spreadsheet formulas: a cell starting with `=`, `+`, `-`,
//! `@`, tab or carriage return would be *executed* by Excel or LibreOffice when an
//! administrator opens the file, so it is prefixed with an apostrophe.

use crate::error::{Error, Result};

/// Largest input accepted.
pub const MAX_BYTES: usize = 1024 * 1024;
/// Most rows (including the header) accepted.
pub const MAX_ROWS: usize = 1001;
/// Most columns per row accepted.
pub const MAX_COLUMNS: usize = 20;

/// Parses CSV text into rows of cells. Blank lines are skipped.
///
/// # Errors
/// [`Error::Validation`] for oversized input, too many rows or columns, or an
/// unterminated quoted field.
pub fn parse(input: &str) -> Result<Vec<Vec<String>>> {
    if input.len() > MAX_BYTES {
        return Err(Error::Validation(format!(
            "the file is larger than {} KB",
            MAX_BYTES / 1024
        )));
    }
    let text = input.strip_prefix('\u{feff}').unwrap_or(input);
    let separator = detect_separator(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut chars = text.chars().peekable();
    let finish_row =
        |row: &mut Vec<String>, cell: &mut String, rows: &mut Vec<Vec<String>>| -> Result<()> {
            row.push(std::mem::take(cell));
            let blank = row.len() == 1 && row[0].trim().is_empty();
            let done = std::mem::take(row);
            if !blank {
                if done.len() > MAX_COLUMNS {
                    return Err(Error::Validation(format!(
                        "a row has more than {MAX_COLUMNS} columns"
                    )));
                }
                rows.push(done);
                if rows.len() > MAX_ROWS {
                    return Err(Error::Validation(format!(
                        "more than {} rows",
                        MAX_ROWS - 1
                    )));
                }
            }
            Ok(())
        };
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() && !was_quoted => {
                quoted = true;
                was_quoted = true;
            }
            c if c == separator => {
                row.push(std::mem::take(&mut cell));
                was_quoted = false;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                finish_row(&mut row, &mut cell, &mut rows)?;
                was_quoted = false;
            }
            other => cell.push(other),
        }
    }
    if quoted {
        return Err(Error::Validation("a quoted value is never closed".into()));
    }
    if !cell.is_empty() || !row.is_empty() {
        finish_row(&mut row, &mut cell, &mut rows)?;
    }
    Ok(rows)
}

/// `;` if the first line uses it and no comma (common in German spreadsheets), else `,`.
fn detect_separator(text: &str) -> char {
    let first = text.lines().next().unwrap_or("");
    if first.contains(';') && !first.contains(',') {
        ';'
    } else {
        ','
    }
}

/// Makes one cell safe to write: neutralises formulas, then quotes if needed.
#[must_use]
pub fn escape_cell(value: &str) -> String {
    let mut text = value.to_owned();
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        text.insert(0, '\'');
    }
    if text.contains([',', '"', '\n', '\r', ';']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

/// One CSV line (with a trailing `\r\n`).
#[must_use]
pub fn write_row<S: AsRef<str>>(cells: &[S]) -> String {
    let mut line = cells
        .iter()
        .map(|c| escape_cell(c.as_ref()))
        .collect::<Vec<_>>()
        .join(",");
    line.push_str("\r\n");
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(input: &str) -> Vec<Vec<String>> {
        parse(input).unwrap()
    }

    #[test]
    fn plain_and_quoted_values() {
        assert_eq!(
            rows("a,b,c\n1,2,3\n"),
            vec![vec!["a", "b", "c"], vec!["1", "2", "3"]]
        );
        assert_eq!(
            rows("name,note\r\n\"Doe, Jane\",\"said \"\"hi\"\"\"\r\n"),
            vec![vec!["name", "note"], vec!["Doe, Jane", "said \"hi\""]]
        );
        assert_eq!(
            rows("a,\"line1\nline2\",c"),
            vec![vec!["a", "line1\nline2", "c"]]
        );
    }

    #[test]
    fn line_endings_blank_lines_and_bom() {
        assert_eq!(
            rows("\u{feff}a,b\r\n\r\n\n1,2\r3,4"),
            vec![vec!["a", "b"], vec!["1", "2"], vec!["3", "4"]]
        );
        assert_eq!(rows(""), Vec::<Vec<String>>::new());
        assert_eq!(rows("a,,c"), vec![vec!["a", "", "c"]]);
        assert_eq!(rows("a,b,"), vec![vec!["a", "b", ""]]);
    }

    #[test]
    fn semicolon_files_are_understood() {
        assert_eq!(
            rows("email;name\nx@y.z;Anna"),
            vec![vec!["email", "name"], vec!["x@y.z", "Anna"]]
        );
        // A comma in the header wins: the semicolon is then just text.
        assert_eq!(
            rows("a,b;c\n1,2;3"),
            vec![vec!["a", "b;c"], vec!["1", "2;3"]]
        );
    }

    #[test]
    fn broken_or_oversized_input_is_refused() {
        assert!(parse("a,\"never closed").is_err());
        assert!(parse(&"x".repeat(MAX_BYTES + 1)).is_err());
        assert!(parse(&"a\n".repeat(MAX_ROWS + 1)).is_err());
        assert!(parse(&vec!["c"; MAX_COLUMNS + 1].join(",")).is_err());
    }

    #[test]
    fn formulas_are_neutralised_when_writing() {
        for evil in [
            "=1+1",
            "+cmd|' /C calc'!A0",
            "-2+3",
            "@SUM(A1)",
            "\tx",
            "\rx",
        ] {
            let cell = escape_cell(evil);
            assert!(
                cell.starts_with('\'') || cell.starts_with("\"'"),
                "{evil:?} -> {cell:?}"
            );
        }
        assert_eq!(escape_cell("plain"), "plain");
        assert_eq!(escape_cell("a,b"), "\"a,b\"");
        assert_eq!(escape_cell("say \"x\""), "\"say \"\"x\"\"\"");
        assert_eq!(write_row(&["a", "=b", "c,d"]), "a,'=b,\"c,d\"\r\n");
    }

    #[test]
    fn what_we_write_we_can_read_back() {
        let line = write_row(&["x@y.z", "Doe, \"J\"", "multi\nline"]);
        assert_eq!(
            rows(&line),
            vec![vec!["x@y.z", "Doe, \"J\"", "multi\nline"]]
        );
    }
}

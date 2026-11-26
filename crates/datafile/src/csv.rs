//! A reader for comma-separated values as RFC 4180 describes them, with
//! the leniencies a data file wants: blank lines are skipped, a field may
//! be indented after its comma, and a quoted field may span lines. Every
//! record must have as many fields as the first.

use std::fmt;

/// Why a record could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseError {
    /// Line the record starts on.
    start_line: usize,
    /// Line the problem is on.
    line: usize,
    /// 1-based byte column of the problem.
    column: usize,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    BareQuote,
    Quote,
    FieldCount,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            Kind::BareQuote => "bare \" in non-quoted-field",
            Kind::Quote => "extraneous or missing \" in quoted-field",
            Kind::FieldCount => {
                return write!(f, "record on line {}: wrong number of fields", self.line)
            }
        };
        if self.start_line != self.line {
            write!(
                f,
                "record on line {}; parse error on line {}, column {}: {what}",
                self.start_line, self.line, self.column
            )
        } else {
            write!(
                f,
                "parse error on line {}, column {}: {what}",
                self.line, self.column
            )
        }
    }
}

pub(crate) struct Reader<'a> {
    rest: &'a str,
    /// Lines read so far.
    num_line: usize,
    /// Fields the first record had; every other must match.
    fields: Option<usize>,
}

/// How long the line ending of `line` is: one for a newline, else none.
fn nl_len(line: &str) -> usize {
    usize::from(line.ends_with('\n'))
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a str) -> Self {
        Reader {
            rest: data,
            num_line: 0,
            fields: None,
        }
    }

    /// Reads the next line with its ending, a `\r\n` read as `\n`.
    /// `None` is the end of the data.
    fn read_line(&mut self) -> Option<String> {
        if self.rest.is_empty() {
            return None;
        }
        let end = self.rest.find('\n').map_or(self.rest.len(), |i| i + 1);
        let (line, rest) = self.rest.split_at(end);
        self.rest = rest;
        self.num_line += 1;
        let mut line = line.to_string();
        if rest.is_empty() && line.ends_with('\r') {
            // A carriage return that ends the data is not content.
            line.pop();
        }
        if line.ends_with("\r\n") {
            line.truncate(line.len() - 2);
            line.push('\n');
        }
        Some(line)
    }

    /// Reads one record: `None` at the end of the data.
    pub(crate) fn read(&mut self) -> Option<Result<Vec<String>, ParseError>> {
        // Skip the empty lines.
        let mut line = loop {
            let line = self.read_line()?;
            if line.len() != nl_len(&line) {
                break line;
            }
        };
        let rec_line = self.num_line;
        let record = match self.record(&mut line, rec_line) {
            Ok(record) => record,
            Err(err) => return Some(Err(err)),
        };
        match self.fields {
            None => self.fields = Some(record.len()),
            Some(n) if n != record.len() => {
                return Some(Err(ParseError {
                    start_line: rec_line,
                    line: rec_line,
                    column: 1,
                    kind: Kind::FieldCount,
                }))
            }
            Some(_) => {}
        }
        Some(Ok(record))
    }

    fn record(&mut self, first: &mut String, rec_line: usize) -> Result<Vec<String>, ParseError> {
        let mut record = Vec::new();
        let mut line: String = std::mem::take(first);
        // The position of the start of `line` within its source line.
        let mut pos_line = rec_line;
        let mut col = 1usize;
        loop {
            // Leading blanks of a field are not part of it.
            let blank = line.len() - line.trim_start().len();
            col += blank;
            line.drain(..blank);
            if !line.starts_with('"') {
                // A plain field runs to the comma or the end of the line.
                let comma = line.find(',');
                let field = match comma {
                    Some(i) => &line[..i],
                    None => &line[..line.len() - nl_len(&line)],
                };
                if let Some(q) = field.find('"') {
                    return Err(ParseError {
                        start_line: rec_line,
                        line: pos_line,
                        column: col + q,
                        kind: Kind::BareQuote,
                    });
                }
                record.push(field.to_string());
                match comma {
                    Some(i) => {
                        col += i + 1;
                        line.drain(..=i);
                        continue;
                    }
                    None => break,
                }
            }
            // A quoted field: `""` inside it is one quote.
            let mut field = String::new();
            line.drain(..1);
            col += 1;
            loop {
                if let Some(i) = line.find('"') {
                    field.push_str(&line[..i]);
                    line.drain(..=i);
                    col += i + 1;
                    if line.starts_with('"') {
                        field.push('"');
                        line.drain(..1);
                        col += 1;
                    } else if line.starts_with(',') {
                        line.drain(..1);
                        col += 1;
                        record.push(field);
                        break;
                    } else if line.len() == nl_len(&line) {
                        record.push(field);
                        return Ok(record);
                    } else {
                        // A quote that neither doubles nor ends the field.
                        return Err(ParseError {
                            start_line: rec_line,
                            line: pos_line,
                            column: col - 1,
                            kind: Kind::Quote,
                        });
                    }
                } else if !line.is_empty() {
                    // The field carries on over the line break.
                    field.push_str(&line);
                    col += line.len();
                    match self.read_line() {
                        Some(next) => {
                            line = next;
                            pos_line += 1;
                            col = 1;
                        }
                        None => line.clear(),
                    }
                } else {
                    // The data ended before the closing quote.
                    return Err(ParseError {
                        start_line: rec_line,
                        line: pos_line,
                        column: col,
                        kind: Kind::Quote,
                    });
                }
            }
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(data: &str) -> Result<Vec<Vec<String>>, String> {
        let mut r = Reader::new(data);
        let mut out = Vec::new();
        while let Some(rec) = r.read() {
            out.push(rec.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    #[test]
    fn records() {
        assert_eq!(
            all("a,b\r\n\r\n1, \"x \"\"y\"\", z\"\n\n2,\"multi\nline\"\n3,").unwrap(),
            vec![
                vec!["a", "b"],
                vec!["1", "x \"y\", z"],
                vec!["2", "multi\nline"],
                vec!["3", ""],
            ]
        );
        assert_eq!(all("").unwrap(), Vec::<Vec<String>>::new());
        assert_eq!(all("a\n\"b\"").unwrap(), vec![vec!["a"], vec!["b"]]);
    }
}

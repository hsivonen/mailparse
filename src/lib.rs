extern crate base64;
extern crate encoding;
extern crate quoted_printable;

use std::error;
use std::fmt;
use std::str;

#[derive(Debug)]
pub struct MailParseError {
    description: String,
    position: usize,
}

impl fmt::Display for MailParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} (offset {})", self.description, self.position)
    }
}

impl error::Error for MailParseError {
    fn description(&self) -> &str {
        "An error occurred while attempting to parse the input"
    }

    fn cause(&self) -> Option<&error::Error> {
        None
    }
}

impl From<quoted_printable::QuotedPrintableError> for MailParseError {
    fn from(err: quoted_printable::QuotedPrintableError) -> MailParseError {
        use std::error::Error;
        MailParseError {
            description: err.description().to_string(),
            position: 0,
        }
    }
}

impl From<base64::Base64Error> for MailParseError {
    fn from(err: base64::Base64Error) -> MailParseError {
        use std::error::Error;
        MailParseError {
            description: err.description().to_string(),
            position: 0,
        }
    }
}

impl From<str::Utf8Error> for MailParseError {
    fn from(err: str::Utf8Error) -> MailParseError {
        use std::error::Error;
        MailParseError {
            description: err.description().to_string(),
            position: 0,
        }
    }
}

#[derive(Debug)]
pub struct MailHeader<'a> {
    key: &'a [u8],
    value: &'a [u8],
}

fn is_boundary(line: &str, ix: Option<usize>) -> bool {
    ix.map_or_else(|| true,
                   |v| v >= line.len() || line.chars().nth(v).unwrap().is_whitespace())
}

fn find_from(line: &str, ix_start: usize, key: &str) -> Option<usize> {
    line[ix_start..].find(key).map(|v| ix_start + v)
}

impl<'a> MailHeader<'a> {
    pub fn get_key(&self) -> Result<String, MailParseError> {
        let utf = try!(str::from_utf8(self.key));
        Ok(utf.trim().to_string())
    }

    fn decode_word(&self, encoded: &str) -> Result<String, MailParseError> {
        let ix_delim1 = try!(encoded.find("?").ok_or(MailParseError {
            description: "Unable to find '?' inside encoded-word".to_string(),
            position: 0,
        }));
        let ix_delim2 = try!(find_from(encoded, ix_delim1 + 1, "?").ok_or(MailParseError {
            description: "Unable to find second '?' inside encoded-word".to_string(),
            position: ix_delim1 + 1,
        }));

        let charset = &encoded[0..ix_delim1];
        let transfer_coding = &encoded[ix_delim1 + 1..ix_delim2];
        let input = &encoded[ix_delim2 + 1..];

        let decoded = match transfer_coding {
            "B" => try!(base64::u8de(input.as_bytes())),
            "Q" => {
                try!(quoted_printable::decode(&input.replace("_", " "),
                                              quoted_printable::ParseMode::Robust))
            }
            _ => {
                return Err(MailParseError {
                    description: "Unknown transfer-coding name found in encoded-word".to_string(),
                    position: ix_delim1 + 1,
                })
            }
        };
        let charset_conv = try!(encoding::label::encoding_from_whatwg_label(charset)
            .ok_or(MailParseError {
                description: "Unknown charset found in encoded-word".to_string(),
                position: 0,
            }));
        charset_conv.decode(&decoded, encoding::DecoderTrap::Replace).map_err(|_| {
            MailParseError {
                description: "Unable to convert transfer-decoded bytes from specified charset"
                    .to_string(),
                position: 0,
            }
        })
    }

    pub fn get_value(&self) -> Result<String, MailParseError> {
        let mut result = String::new();
        let utf = try!(str::from_utf8(self.value));
        let mut lines = utf.lines();
        let mut add_space = false;
        loop {
            let line = match lines.next() {
                Some(v) => v.trim_left(),
                None => break,
            };

            if add_space {
                result.push(' ');
            }
            add_space = true;

            let mut ix_search = 0;
            loop {
                match find_from(line, ix_search, "=?") {
                    Some(v) => {
                        let ix_begin = v + 2;
                        if !is_boundary(line, ix_begin.checked_sub(3)) {
                            result.push_str(&line[ix_search..ix_begin]);
                            ix_search = ix_begin;
                            continue;
                        }
                        result.push_str(&line[ix_search..ix_begin - 2]);
                        let mut ix_end_search = ix_begin;
                        loop {
                            match find_from(line, ix_end_search, "?=") {
                                Some(ix_end) => {
                                    if !is_boundary(line, ix_end.checked_add(2)) {
                                        ix_end_search = ix_end + 2;
                                        continue;
                                    }
                                    match self.decode_word(&line[ix_begin..ix_end]) {
                                        Ok(v) => {
                                            result.push_str(&v);
                                        }
                                        Err(_) => {
                                            result.push_str(&line[ix_begin - 2..ix_end + 2])
                                        }
                                    };
                                    ix_search = ix_end;
                                }
                                None => {
                                    result.push_str(&"=?");
                                }
                            };
                            break;
                        }
                        ix_search = ix_search + 2;
                        continue;
                    }
                    None => {
                        result.push_str(&line[ix_search..]);
                        break;
                    }
                };
            }
        }
        Ok(result)
    }
}

enum HeaderParseState {
    Initial,
    Key,
    PreValue,
    Value,
    ValueNewline,
}

pub fn parse_header(raw_data: &[u8]) -> Result<(MailHeader, usize), MailParseError> {
    let mut it = raw_data.iter();
    let mut ix = 0;
    let mut c = match it.next() {
        None => {
            return Err(MailParseError {
                description: "Empty string provided".to_string(),
                position: 0,
            })
        }
        Some(v) => *v,
    };

    let mut ix_key_end = None;
    let mut ix_value_start = 0;
    let mut ix_value_end = 0;

    let mut state = HeaderParseState::Initial;
    loop {
        match state {
            HeaderParseState::Initial => {
                if c == b' ' {
                    return Err(MailParseError {
                        description: "Header cannot start with a space; it is likely an \
                                      overhanging line from a previous header"
                            .to_string(),
                        position: ix,
                    });
                };
                state = HeaderParseState::Key;
                continue;
            }
            HeaderParseState::Key => {
                if c == b':' {
                    ix_key_end = Some(ix);
                    state = HeaderParseState::PreValue;
                } else if c == b'\n' {
                    return Err(MailParseError {
                        description: "Unexpected newline in header key".to_string(),
                        position: ix,
                    });
                }
            }
            HeaderParseState::PreValue => {
                if c != b' ' {
                    ix_value_start = ix;
                    ix_value_end = ix;
                    state = HeaderParseState::Value;
                    continue;
                }
            }
            HeaderParseState::Value => {
                if c == b'\n' {
                    state = HeaderParseState::ValueNewline;
                } else {
                    ix_value_end = ix + 1;
                }
            }
            HeaderParseState::ValueNewline => {
                if c == b' ' {
                    state = HeaderParseState::Value;
                    continue;
                } else {
                    break;
                }
            }
        }
        ix = ix + 1;
        c = match it.next() {
            None => break,
            Some(v) => *v,
        };
    }
    match ix_key_end {
        Some(v) => {
            Ok((MailHeader {
                key: &raw_data[0..v],
                value: &raw_data[ix_value_start..ix_value_end],
            },
                ix))
        }

        None => {
            Err(MailParseError {
                description: "Unable to determine end of the header key component".to_string(),
                position: ix,
            })
        }
    }
}

pub trait MailHeaderMap {
    fn get_first_value(&self, key: &str) -> Result<Option<String>, MailParseError>;
    fn get_all_values(&self, key: &str) -> Result<Vec<String>, MailParseError>;
}

impl<'a> MailHeaderMap for Vec<MailHeader<'a>> {
    fn get_first_value(&self, key: &str) -> Result<Option<String>, MailParseError> {
        for x in self {
            if try!(x.get_key()) == key {
                return x.get_value().map(|v| Some(v));
            }
        }
        Ok(None)
    }

    fn get_all_values(&self, key: &str) -> Result<Vec<String>, MailParseError> {
        let mut values: Vec<String> = Vec::new();
        for x in self {
            if try!(x.get_key()) == key {
                values.push(try!(x.get_value()));
            }
        }
        Ok(values)
    }
}

pub fn parse_headers(raw_data: &[u8]) -> Result<(Vec<MailHeader>, usize), MailParseError> {
    let mut headers: Vec<MailHeader> = Vec::new();
    let mut ix = 0;
    loop {
        let (header, ix_next) = try!(parse_header(&raw_data[ix..]).map_err(|e| {
            MailParseError {
                description: e.description,
                position: e.position + ix,
            }
        }));
        headers.push(header);
        ix = ix + ix_next;
        if ix >= raw_data.len() || raw_data[ix] == b'\n' {
            break;
        }
    }
    Ok((headers, ix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_header() {
        let (parsed, _) = parse_header(b"Key: Value").unwrap();
        assert_eq!(parsed.key, b"Key");
        assert_eq!(parsed.get_key().unwrap(), "Key");
        assert_eq!(parsed.value, b"Value");
        assert_eq!(parsed.get_value().unwrap(), "Value");

        let (parsed, _) = parse_header(b"Key :  Value ").unwrap();
        assert_eq!(parsed.key, b"Key ");
        assert_eq!(parsed.value, b"Value ");
        assert_eq!(parsed.get_value().unwrap(), "Value ");

        let (parsed, _) = parse_header(b"Key:").unwrap();
        assert_eq!(parsed.key, b"Key");
        assert_eq!(parsed.value, b"");

        let (parsed, _) = parse_header(b":\n").unwrap();
        assert_eq!(parsed.key, b"");
        assert_eq!(parsed.value, b"");

        let (parsed, _) = parse_header(b"Key:Multi-line\n value").unwrap();
        assert_eq!(parsed.key, b"Key");
        assert_eq!(parsed.value, b"Multi-line\n value");
        assert_eq!(parsed.get_value().unwrap(), "Multi-line value");

        let (parsed, _) = parse_header(b"Key:  Multi\n  line\n value\n").unwrap();
        assert_eq!(parsed.key, b"Key");
        assert_eq!(parsed.value, b"Multi\n  line\n value");
        assert_eq!(parsed.get_value().unwrap(), "Multi line value");

        let (parsed, _) = parse_header(b"Key: One\nKey2: Two").unwrap();
        assert_eq!(parsed.key, b"Key");
        assert_eq!(parsed.value, b"One");

        parse_header(b" Leading: Space").unwrap_err();
        parse_header(b"Just a string").unwrap_err();
        parse_header(b"Key\nBroken: Value").unwrap_err();
    }

    #[test]
    fn parse_encoded_headers() {
        let (parsed, _) = parse_header(b"Subject: =?iso-8859-1?Q?=A1Hola,_se=F1or!?=").unwrap();
        assert_eq!(parsed.get_key().unwrap(), "Subject");
        assert_eq!(parsed.get_value().unwrap(), "\u{a1}Hola, se\u{f1}or!");

        let (parsed, _) = parse_header(b"Subject: =?iso-8859-1?Q?=A1Hola,?=\n \
                                        =?iso-8859-1?Q?_se=F1or!?=")
            .unwrap();
        assert_eq!(parsed.get_key().unwrap(), "Subject");
        assert_eq!(parsed.get_value().unwrap(), "\u{a1}Hola,  se\u{f1}or!");

        let (parsed, _) = parse_header(b"Euro: =?utf-8?Q?=E2=82=AC?=").unwrap();
        assert_eq!(parsed.get_key().unwrap(), "Euro");
        assert_eq!(parsed.get_value().unwrap(), "\u{20ac}");

        let (parsed, _) = parse_header(b"HelloWorld: =?utf-8?B?aGVsbG8gd29ybGQ=?=").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "hello world");

        let (parsed, _) = parse_header(b"Empty: =?utf-8?Q??=").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "");

        let (parsed, _) = parse_header(b"Incomplete: =?").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "=?");

        let (parsed, _) = parse_header(b"BadEncoding: =?garbage?Q??=").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "=?garbage?Q??=");

        let (parsed, _) = parse_header(b"Invalid: =?utf-8?Q?=E2=AC?=").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "\u{fffd}");

        let (parsed, _) = parse_header(b"LineBreak: =?utf-8?Q?=E2=82\n =AC?=").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "=?utf-8?Q?=E2=82 =AC?=");

        let (parsed, _) = parse_header(b"NotSeparateWord: hello=?utf-8?Q?world?=").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "hello=?utf-8?Q?world?=");

        let (parsed, _) = parse_header(b"NotSeparateWord2: =?utf-8?Q?hello?=world").unwrap();
        assert_eq!(parsed.get_value().unwrap(), "=?utf-8?Q?hello?=world");
    }

    #[test]
    fn parse_multiple_headers() {
        let (parsed, _) = parse_headers(b"Key: Value\nTwo: Second").unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].key, b"Key");
        assert_eq!(parsed[0].value, b"Value");
        assert_eq!(parsed[1].key, b"Two");
        assert_eq!(parsed[1].value, b"Second");

        let (parsed, _) = parse_headers(b"Key: Value\n Overhang\nTwo: Second\nThree: Third")
            .unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].key, b"Key");
        assert_eq!(parsed[0].value, b"Value\n Overhang");
        assert_eq!(parsed[1].key, b"Two");
        assert_eq!(parsed[1].value, b"Second");
        assert_eq!(parsed[2].key, b"Three");
        assert_eq!(parsed[2].value, b"Third");

        let (parsed, _) = parse_headers(b"Key: Value\nTwo: Second\n\nBody").unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].key, b"Key");
        assert_eq!(parsed[0].value, b"Value");
        assert_eq!(parsed[1].key, b"Two");
        assert_eq!(parsed[1].value, b"Second");

        let (parsed, _) =
            parse_headers(b"Return-Path: <kats@foobar.staktrace.com>\nX-Original-To: \
                           kats@baz.staktrace.com\nDelivered-To: \
                           kats@baz.staktrace.com\nReceived: from foobar.staktrace.com \
                           (localhost [127.0.0.1])\n    by foobar.staktrace.com (Postfix) with \
                           ESMTP id 139F711C1C34\n    for <kats@baz.staktrace.com>; Fri, 27 May \
                           2016 02:34:26 -0400 (EDT)\nDate: Fri, 27 May 2016 02:34:25 -0400\nTo: \
                           kats@baz.staktrace.com\nFrom: kats@foobar.staktrace.com\nSubject: \
                           test Fri, 27 May 2016 02:34:25 -0400\nX-Mailer: swaks v20130209.0 \
                           jetmore.org/john/code/swaks/\nMessage-Id: \
                           <20160527063426.139F711C1C34@foobar.staktrace.com>\n\nThis is a test \
                           mailing\n")
                .unwrap();
        assert_eq!(parsed.len(), 10);
        assert_eq!(parsed[0].key, b"Return-Path");
        assert_eq!(parsed[9].key, b"Message-Id");

        let (parsed, _) = parse_headers(b"Key: Value\nAnotherKey: AnotherValue\nKey: Value2\nKey: Value3\n").unwrap();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed.get_first_value("Key").unwrap(), Some("Value".to_string()));
        assert_eq!(parsed.get_all_values("Key").unwrap(), vec!["Value", "Value2", "Value3"]);
        assert_eq!(parsed.get_first_value("AnotherKey").unwrap(), Some("AnotherValue".to_string()));
        assert_eq!(parsed.get_all_values("AnotherKey").unwrap(), vec!["AnotherValue"]);
        assert_eq!(parsed.get_first_value("NoKey").unwrap(), None);
        assert_eq!(parsed.get_all_values("NoKey").unwrap(), Vec::<String>::new());

        assert_eq!(parse_headers(b"Bad\nKey").unwrap_err().position, 3);
        assert_eq!(parse_headers(b"K:V\nBad\nKey").unwrap_err().position, 7);
    }
}

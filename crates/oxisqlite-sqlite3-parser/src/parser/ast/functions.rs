//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::custom_err;
use crate::parser::ParserError;
use std::str::{self};

use super::macros::JoinType;
#[cfg(test)]
use super::types::{Name, PragmaName};
use super::types_10::{QuotedIterator, TableReferenceId};

impl From<TableReferenceId> for usize {
    fn from(value: TableReferenceId) -> Self {
        value.0
    }
}

impl TryFrom<&[u8]> for JoinType {
    type Error = ParserError;
    fn try_from(s: &[u8]) -> Result<Self, ParserError> {
        if b"CROSS".eq_ignore_ascii_case(s) {
            Ok(Self::INNER | Self::CROSS)
        } else if b"FULL".eq_ignore_ascii_case(s) {
            Ok(Self::LEFT | Self::RIGHT | Self::OUTER)
        } else if b"INNER".eq_ignore_ascii_case(s) {
            Ok(Self::INNER)
        } else if b"LEFT".eq_ignore_ascii_case(s) {
            Ok(Self::LEFT | Self::OUTER)
        } else if b"NATURAL".eq_ignore_ascii_case(s) {
            Ok(Self::NATURAL)
        } else if b"RIGHT".eq_ignore_ascii_case(s) {
            Ok(Self::RIGHT | Self::OUTER)
        } else if b"OUTER".eq_ignore_ascii_case(s) {
            Ok(Self::OUTER)
        } else {
            Err(custom_err!(
                "unsupported JOIN type: {:?}",
                str::from_utf8(s)
            ))
        }
    }
}

pub(super) fn eq_ignore_case_and_quote(
    mut it: QuotedIterator<'_>,
    mut other: QuotedIterator<'_>,
) -> bool {
    loop {
        match (it.next(), other.next()) {
            (Some(b1), Some(b2)) => {
                if !b1.eq_ignore_ascii_case(&b2) {
                    return false;
                }
            }
            (None, None) => break,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
pub(super) mod test {
    use super::{Name, PragmaName};
    use strum::IntoEnumIterator;

    #[test]
    fn test_dequote() {
        assert_eq!(name("x"), "x");
        assert_eq!(name("`x`"), "x");
        assert_eq!(name("`x``y`"), "x`y");
        assert_eq!(name(r#""x""#), "x");
        assert_eq!(name(r#""x""y""#), "x\"y");
        assert_eq!(name("[x]"), "x");
    }

    #[test]
    // pragma pragma_list expects this to be sorted. We can avoid allocations there if we keep
    // the list sorted.
    fn pragma_list_sorted() {
        let pragma_strings: Vec<String> = PragmaName::iter().map(|x| x.to_string()).collect();
        let mut pragma_strings_sorted = pragma_strings.clone();
        pragma_strings_sorted.sort();
        assert_eq!(pragma_strings, pragma_strings_sorted);
    }

    fn name(s: &'static str) -> Name {
        Name(s.to_owned())
    }
}

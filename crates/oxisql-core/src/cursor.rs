//! [`Cursor`] — incremental result-set traversal over a [`Vec<Row>`].

use crate::Row;

/// An incremental cursor for traversing a materialised result set.
///
/// `Cursor` wraps a `Vec<Row>` and tracks the current position, supporting
/// both peek-ahead access and owned-row iteration via [`Iterator`].
///
/// # Example
///
/// ```rust
/// use oxisql_core::{Cursor, Row, Value};
///
/// let rows = vec![
///     Row::new(vec!["id".into()], vec![Value::I64(1)]),
///     Row::new(vec!["id".into()], vec![Value::I64(2)]),
/// ];
/// let mut cursor = Cursor::new(rows);
/// assert_eq!(cursor.len(), 2);
///
/// let first = cursor.advance().expect("first row");
/// assert_eq!(first.try_get::<i64>("id").unwrap(), 1);
///
/// cursor.reset();
/// assert_eq!(cursor.position(), 0);
/// ```
#[derive(Debug, Clone)]
pub struct Cursor {
    rows: Vec<Row>,
    position: usize,
}

impl Cursor {
    /// Create a new `Cursor` over `rows`, positioned at the beginning.
    pub fn new(rows: Vec<Row>) -> Self {
        Self { rows, position: 0 }
    }

    /// Advance by one and return a shared reference to the current row, or
    /// `None` when past the end.
    ///
    /// Unlike the [`Iterator`] implementation (which yields owned `Row`s),
    /// this method borrows the row in place, allowing repeated access to the
    /// same row without cloning.
    pub fn advance(&mut self) -> Option<&Row> {
        let pos = self.position;
        if pos < self.rows.len() {
            self.position = pos + 1;
            self.rows.get(pos)
        } else {
            None
        }
    }

    /// Return a shared reference to the *current* row without advancing, or
    /// `None` when past the end.
    pub fn peek(&self) -> Option<&Row> {
        self.rows.get(self.position)
    }

    /// Reset the cursor back to position 0.
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Return the current cursor position (zero-based index into the row
    /// collection; equal to the number of rows consumed so far).
    pub fn position(&self) -> usize {
        self.position
    }

    /// Return the total number of rows in the cursor's collection.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Return `true` if the underlying row collection is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Return the number of rows that have not yet been consumed.
    pub fn remaining(&self) -> usize {
        self.rows.len().saturating_sub(self.position)
    }

    /// Consume the cursor and return the underlying row vector.
    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }

    /// Advance the cursor by up to `n` positions without returning rows.
    ///
    /// The cursor will not move past the end of the collection.
    ///
    /// Note: this method is named `skip_by` to avoid shadowing the
    /// [`Iterator::skip`] adapter, which takes ownership of the iterator.
    pub fn skip_by(&mut self, n: usize) {
        self.position = (self.position + n).min(self.rows.len());
    }
}

impl Iterator for Cursor {
    type Item = Row;

    /// Advance the cursor and return an owned clone of the current row, or
    /// `None` when past the end.
    fn next(&mut self) -> Option<Row> {
        let pos = self.position;
        if pos < self.rows.len() {
            self.position = pos + 1;
            Some(self.rows[pos].clone())
        } else {
            None
        }
    }
}

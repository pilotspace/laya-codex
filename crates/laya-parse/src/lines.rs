//! Row index over a source string: row starts, blank flags and non-blank prefix counts.
//! Rows are split on `\n` exactly like tree-sitter's `Point::row`.

pub(crate) struct Lines<'a> {
    src: &'a str,
    starts: Vec<usize>,
    blank: Vec<bool>,
    /// `nonblank[r]` = number of non-blank rows strictly before row `r`.
    nonblank: Vec<u32>,
}

impl<'a> Lines<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        let mut starts = Vec::with_capacity(src.len() / 32 + 1);
        if !src.is_empty() {
            starts.push(0);
            for (i, b) in src.bytes().enumerate() {
                if b == b'\n' && i + 1 < src.len() {
                    starts.push(i + 1);
                }
            }
        }
        let mut this = Self {
            src,
            starts,
            blank: Vec::new(),
            nonblank: Vec::new(),
        };
        let n = this.len();
        this.blank = (0..n).map(|r| this.raw(r).trim().is_empty()).collect();
        this.nonblank = Vec::with_capacity(n + 1);
        let mut acc = 0u32;
        this.nonblank.push(0);
        for &b in &this.blank {
            acc += u32::from(!b);
            this.nonblank.push(acc);
        }
        this
    }

    pub(crate) fn len(&self) -> usize {
        self.starts.len()
    }

    /// Byte offset one past the row's content (excluding its `\n`).
    fn row_end(&self, r: usize) -> usize {
        if r + 1 < self.starts.len() {
            self.starts[r + 1] - 1
        } else if self.src.ends_with('\n') {
            self.src.len() - 1
        } else {
            self.src.len()
        }
    }

    fn raw(&self, r: usize) -> &'a str {
        &self.src[self.starts[r]..self.row_end(r)]
    }

    pub(crate) fn is_blank(&self, r: usize) -> bool {
        self.blank[r]
    }

    pub(crate) fn nonblank_count(&self, s: usize, e: usize) -> usize {
        (self.nonblank[e + 1] - self.nonblank[s]) as usize
    }

    /// Exact source text of rows `s..=e` without the final line terminator (`\n` or `\r\n`).
    pub(crate) fn text(&self, s: usize, e: usize) -> &'a str {
        let t = &self.src[self.starts[s]..self.row_end(e)];
        t.strip_suffix('\r').unwrap_or(t)
    }

    pub(crate) fn row(&self, r: usize) -> &'a str {
        self.raw(r)
    }
}

#[cfg(test)]
mod tests {
    use super::Lines;

    #[test]
    fn rows_and_text() {
        let l = Lines::new("a\n\n  b\r\nc");
        assert_eq!(l.len(), 4);
        assert!(l.is_blank(1));
        assert_eq!(l.text(0, 2), "a\n\n  b");
        assert_eq!(l.text(2, 3), "  b\r\nc");
        assert_eq!(l.nonblank_count(0, 3), 3);
        assert_eq!(Lines::new("").len(), 0);
        assert_eq!(Lines::new("x\n").len(), 1);
        assert_eq!(Lines::new("x\n").text(0, 0), "x");
    }
}

use std::fmt;
use std::ops::Index;

/// List containing max 31 items.
#[derive(Clone, PartialEq, Eq)]
pub struct ReportList<T>([Option<T>; 31]);

impl<T> ReportList<T> {
    pub(crate) fn new() -> Self {
        ReportList::default()
    }

    /// Number of elements in the list.
    pub fn len(&self) -> usize {
        self.0.iter().position(|i| i.is_none()).unwrap_or(31)
    }

    pub(crate) fn push(&mut self, v: T) {
        let pos = self.len();
        self.0[pos] = Some(v);
    }

    /// Get element at position.
    pub fn get(&self, i: usize) -> Option<&T> {
        self.0[i].as_ref()
    }

    /// Iterator over the elements in the list.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.into_iter()
    }

    /// Tells if the list contains zero elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn lists_from_iter(iterator: impl IntoIterator<Item = T>) -> Vec<Self> {
        let mut result = vec![];
        let mut current = Self::default();

        for (i, item) in iterator.into_iter().enumerate() {
            if i % 31 == 0 && i != 0 {
                result.push(current);
                current = Self::default();
            }

            current.push(item);
        }

        if !current.is_empty() {
            result.push(current);
        }

        result
    }

    pub(crate) fn is_full(&self) -> bool {
        self.len() == 31
    }
}

impl<T: private::WordSized> ReportList<T> {
    pub(crate) fn append_all_possible(&mut self, other: &mut Self, mut words_left: usize) -> usize {
        // Position where we start inserting in self.
        let pos = self.len();

        // Max number we can move.
        let max = (31 - pos).min(other.len());

        // after this loop ends, i will hold the number of items moved.
        let mut i = 0;
        loop {
            if i == max {
                break;
            }

            // first borrow item from other, to check the item size will fit.
            let item = other.0[i].as_ref().unwrap();
            let item_size = item.word_size();

            // can we fit one more item?
            if words_left < item_size {
                break;
            }

            // it fits, move it.
            self.0[pos + i] = other.0[i].take();

            // reduce space left.
            words_left -= item_size;

            i += 1;
        }

        // shift down remaining in other
        for j in i..31 {
            other.0[j - i] = other.0[j].take();
        }

        // return number of appended items.
        i
    }
}

pub(crate) mod private {
    pub trait WordSized {
        fn word_size(&self) -> usize;
    }
}

impl<T> Index<usize> for ReportList<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.0[index].as_ref().unwrap()
    }
}

impl<T> Default for ReportList<T> {
    fn default() -> Self {
        // We don't want to require T: Copy.
        ReportList([
            None, None, None, None, None, None, None, None, None, None, None, None, None, None,
            None, None, None, None, None, None, None, None, None, None, None, None, None, None,
            None, None, None,
        ])
    }
}

impl<'a, T> IntoIterator for &'a ReportList<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        Iter(self, 0)
    }
}

impl<T> IntoIterator for ReportList<T> {
    type Item = T;
    type IntoIter = IterOwned<T>;

    fn into_iter(self) -> Self::IntoIter {
        IterOwned(self, 0)
    }
}

pub struct Iter<'a, T>(&'a ReportList<T>, usize);

pub struct IterOwned<T>(ReportList<T>, usize);

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.1 > 30 {
            return None;
        }

        let n = self.0.0[self.1].as_ref();
        if n.is_some() {
            self.1 += 1;
        }

        n
    }
}

impl<T> Iterator for IterOwned<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.1 > 30 {
            return None;
        }

        let n = self.0.0[self.1].take();
        if n.is_some() {
            self.1 += 1;
        }
        n
    }
}

impl<T> From<T> for ReportList<T> {
    fn from(t: T) -> Self {
        let mut l = ReportList::default();
        l.push(t);
        l
    }
}

impl<T: fmt::Debug> fmt::Debug for ReportList<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[")?;
        let len = self.len();
        for (i, s) in self.0.iter().filter_map(|f| f.as_ref()).enumerate() {
            if i == len - 1 {
                write!(f, "{s:?}")?;
            } else {
                write!(f, "{s:?},")?;
            }
        }
        write!(f, "]")
    }
}

#[cfg(test)]
mod test {
    use super::ReportList;

    #[test]
    fn test_lists_from_iter() {
        let lists = ReportList::lists_from_iter(0..66);

        assert_eq!(lists.len(), 3);
        assert_eq!(lists[0].len(), 31);
        assert_eq!(lists[1].len(), 31);
        assert_eq!(lists[2].len(), 4);
    }

    #[test]
    fn test_max_length_iter() {
        let list = {
            let mut list = ReportList::new();
            for i in 1..=31 {
                list.push(i);
            }

            list
        };
        let sum: u64 = list.iter().sum();

        assert_eq!(sum, 496);
    }

    #[test]
    fn test_max_length_into_iter() {
        let list = {
            let mut list = ReportList::new();
            for i in 1..=31 {
                list.push(i);
            }

            list
        };
        let sum: u64 = list.into_iter().sum();

        assert_eq!(sum, 496);
    }
}

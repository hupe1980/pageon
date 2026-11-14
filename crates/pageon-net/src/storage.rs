//! Web Storage areas (`localStorage`, `sessionStorage`): ordered string pairs per origin.

/// The quota per origin, in UTF-16 code units of keys plus values.
pub const QUOTA: usize = 5 * 1024 * 1024;

/// The quota was exceeded (`QuotaExceededError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaExceeded;

/// One origin's storage area.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Storage {
    items: Vec<(String, String)>,
    used: usize,
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

impl Storage {
    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The `n`th key.
    pub fn key(&self, n: usize) -> Option<&str> {
        self.items.get(n).map(|(k, _)| k.as_str())
    }

    /// The keys, in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|(k, _)| k.as_str())
    }

    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Sets `key` to `value`, keeping its position if it exists.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), QuotaExceeded> {
        let old = self.items.iter().position(|(k, _)| k == key);
        let freed = old.map_or(0, |i| units(&self.items[i].0) + units(&self.items[i].1));
        let need = units(key) + units(value);
        if self.used - freed + need > QUOTA {
            return Err(QuotaExceeded);
        }
        self.used = self.used - freed + need;
        match old {
            Some(i) => self.items[i].1 = value.to_string(),
            None => self.items.push((key.to_string(), value.to_string())),
        }
        Ok(())
    }

    /// Removes `key`.
    pub fn remove(&mut self, key: &str) {
        if let Some(i) = self.items.iter().position(|(k, _)| k == key) {
            let (k, v) = self.items.remove(i);
            self.used -= units(&k) + units(&v);
        }
    }

    /// Removes everything.
    pub fn clear(&mut self) {
        self.items.clear();
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_order_and_quota() {
        let mut s = Storage::default();
        s.set("b", "1").unwrap();
        s.set("a", "2").unwrap();
        s.set("b", "3").unwrap();
        assert_eq!(s.keys().collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(s.get("b"), Some("3"));
        s.remove("b");
        assert_eq!(s.len(), 1);
        let big = "x".repeat(QUOTA);
        assert_eq!(s.set("k", &big), Err(QuotaExceeded));
        s.clear();
        assert!(s.set("", &"x".repeat(QUOTA)).is_ok());
    }
}

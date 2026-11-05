//! Typed editor settings (§12). Lua reads and writes them by name through
//! `get` and `set`, and every value is checked on the way in (I-11).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub fill_column: u16,
    pub tab_width: u8,
    /// TAB inserts spaces up to the next tab stop instead of a tab character.
    pub expand_tabs: bool,
    pub case_fold_search: bool,
    pub max_file_size: u64,
    pub undo_limit: usize,
}

impl Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            fill_column: 70,
            tab_width: 8,
            expand_tabs: false,
            case_fold_search: true,
            max_file_size: 64 << 20,
            undo_limit: 4 << 20,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// A setting value crossing the Lua boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bool(bool),
}

impl Config {
    /// Returns the current value of setting `name`.
    ///
    /// # Errors
    /// Returns an error if `name` is not a recognized configuration setting.
    pub fn get(&self, name: &str) -> Result<Value, String> {
        Ok(match name {
            "fill-column" => Value::Int(i64::from(self.fill_column)),
            "tab-width" => Value::Int(i64::from(self.tab_width)),
            "expand-tabs" => Value::Bool(self.expand_tabs),
            "case-fold-search" => Value::Bool(self.case_fold_search),
            "max-file-size" => Value::Int(i64::try_from(self.max_file_size).unwrap_or(i64::MAX)),
            "undo-limit" => Value::Int(i64::try_from(self.undo_limit).unwrap_or(i64::MAX)),
            _ => return Err(format!("unknown setting: {name}")),
        })
    }

    /// Sets `name` after checking the type and range. Errors leave the setting unchanged.
    ///
    /// # Errors
    /// Returns an error if `name` is unknown or `value` is out of bounds or wrong type.
    pub fn set(mut self, name: &str, value: Value) -> Result<Self, String> {
        match (name, value) {
            ("fill-column", Value::Int(n)) => {
                self.fill_column = bounded(name, n, 1, i64::from(u16::MAX))?;
            }
            ("tab-width", Value::Int(n)) => self.tab_width = bounded(name, n, 1, 16)?,
            ("expand-tabs", Value::Bool(b)) => self.expand_tabs = b,
            ("case-fold-search", Value::Bool(b)) => self.case_fold_search = b,
            ("max-file-size", Value::Int(n)) => self.max_file_size = bounded(name, n, 1, i64::MAX)?,
            ("undo-limit", Value::Int(n)) => self.undo_limit = bounded(name, n, 0, i64::MAX)?,
            ("fill-column" | "tab-width" | "max-file-size" | "undo-limit", _) => {
                return Err(format!("{name} takes an integer"));
            }
            ("case-fold-search" | "expand-tabs", _) => {
                return Err(format!("{name} takes a boolean"));
            }
            _ => return Err(format!("unknown setting: {name}")),
        }
        Ok(self)
    }
}

fn bounded<T: TryFrom<i64>>(name: &str, n: i64, min: i64, max: i64) -> Result<T, String> {
    if !(min..=max).contains(&n) {
        return Err(format!("{name} must be between {min} and {max}"));
    }
    T::try_from(n).map_err(|_| format!("{name} is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_and_set_are_checked() {
        let c = Config::default();
        assert_eq!(c.get("fill-column"), Ok(Value::Int(70)));
        assert_eq!(c.get("case-fold-search"), Ok(Value::Bool(true)));
        let c = c.set("fill-column", Value::Int(33)).unwrap();
        assert_eq!(c.fill_column, 33);
        assert!(c.set("tab-width", Value::Int(0)).is_err());
        assert!(c.set("fill-column", Value::Bool(true)).is_err());
        assert!(c.set("nope", Value::Int(1)).is_err());
        assert!(c.get("nope").is_err());
        assert_eq!(c.tab_width, 8);
    }

    #[test]
    fn expand_tabs_defaults_off_and_takes_a_boolean() {
        let c = Config::default();
        assert_eq!(c.get("expand-tabs"), Ok(Value::Bool(false)));
        let c = c.set("expand-tabs", Value::Bool(true)).unwrap();
        assert!(c.expand_tabs);
        assert_eq!(
            c.set("expand-tabs", Value::Int(1)),
            Err("expand-tabs takes a boolean".to_owned())
        );
    }
}
